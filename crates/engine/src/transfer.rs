use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use ruscello_core::{DownloadId, FailureReason, Source};
use ruscello_http::{
    CheckpointJob, DownloadOptions, DownloadProgress, ErrorClass, HttpDownloader, HttpError,
    RetryPolicy, Url, WriterError, filename_candidate, scan_checkpoints,
};
use ruscello_storage::QueueEntry;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{EngineError, Limits, RECOVERY_FOLDER};

#[derive(Debug)]
pub(super) struct Published {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TransferError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("download file operation failed")]
    File(#[from] io::Error),
    #[error("a file operation stopped unexpectedly")]
    Worker(#[from] tokio::task::JoinError),
}

impl TransferError {
    pub(super) fn cancelled(&self) -> bool {
        matches!(self, Self::Http(HttpError::Cancelled))
    }

    pub(super) fn reason(&self) -> FailureReason {
        match self {
            Self::File(_) => FailureReason::FileSystem,
            Self::Worker(_) => FailureReason::Internal,
            Self::Http(HttpError::Storage(WriterError::ChecksumMismatch)) => {
                FailureReason::IntegrityCheck
            }
            Self::Http(HttpError::Storage(_)) => FailureReason::FileSystem,
            Self::Http(HttpError::UnsupportedScheme) => FailureReason::UnsupportedSource,
            Self::Http(error) => match error.classification() {
                ErrorClass::Transient => FailureReason::Network,
                ErrorClass::Authentication => FailureReason::AccessDenied,
                ErrorClass::Permanent | ErrorClass::ExpiredLink => FailureReason::SourceUnavailable,
            },
        }
    }
}

pub(super) fn source_url(source: &Source) -> Result<Url, EngineError> {
    let Source::Http { url } = source else {
        return Err(EngineError::UnsupportedSource);
    };
    let mut url = Url::parse(url).map_err(|_| EngineError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(EngineError::InvalidUrl);
    }
    url.set_fragment(None);
    Ok(url)
}

pub(super) fn working_directory(root: &Path, id: DownloadId) -> PathBuf {
    root.join(RECOVERY_FOLDER).join(id.0.to_string())
}

pub(super) async fn run(
    http: HttpDownloader,
    entry: QueueEntry,
    url: Url,
    limits: Limits,
    cancellation: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
) -> Result<Published, TransferError> {
    let (ready, existing) = tokio::task::spawn_blocking({
        let ready = ready_directory(&entry);
        move || {
            fs::create_dir_all(&ready)?;
            let existing = ready_file(&ready)?;
            Ok::<_, io::Error>((ready, existing))
        }
    })
    .await??;
    let staged = match existing {
        Some(staged) => staged,
        None => {
            let options = DownloadOptions {
                directory: entry.working_directory.clone(),
                connections: limits
                    .connections_per_download
                    .min(limits.connections_per_host),
                cancellation: cancellation.clone(),
                retry: RetryPolicy {
                    max_retries: limits.retries,
                    ..RetryPolicy::default()
                },
                progress: Some(progress),
                checkpoint_job: Some(CheckpointJob {
                    id: entry.download.id.0,
                    destination: entry.download.destination.clone(),
                }),
                completion_directory: Some(ready),
                ..DownloadOptions::default()
            };
            Box::pin(http.download(&url, options)).await?
        }
    };
    if cancellation.is_cancelled() {
        return Err(HttpError::Cancelled.into());
    }
    let published = tokio::task::spawn_blocking(move || publish(&entry, &staged)).await??;
    Ok(published)
}

fn ready_directory(entry: &QueueEntry) -> PathBuf {
    entry
        .working_directory
        .join(format!(".ready-{}", entry.download.id.0))
}

fn intent_path(entry: &QueueEntry) -> PathBuf {
    entry
        .working_directory
        .join(format!(".publish-{}", entry.download.id.0))
}

fn ready_file(directory: &Path) -> io::Result<Option<PathBuf>> {
    let mut result = None;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || result.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected staged files",
            ));
        }
        result = Some(entry.path());
    }
    Ok(result)
}

fn publish(entry: &QueueEntry, staged: &Path) -> io::Result<Published> {
    let directory = &entry.download.destination;
    fs::create_dir_all(directory)?;
    let bytes = staged.metadata()?.len();
    let intent = intent_path(entry);
    if let Some(path) = published_target(&intent, directory, staged, bytes)? {
        sync_directory(directory)?;
        return Ok(Published { path, bytes });
    }
    let name = staged
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    for index in 0..10_000 {
        let name = filename_candidate(name, index);
        let path = directory.join(&name);
        if path.try_exists()? {
            continue;
        }
        save_intent(&intent, &name, &entry.working_directory)?;
        match publish_file(staged, &path, directory) {
            Ok(()) => {
                sync_directory(directory)?;
                return Ok(Published { path, bytes });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no available output filename",
    ))
}

fn published_target(
    intent: &Path,
    directory: &Path,
    staged: &Path,
    bytes: u64,
) -> io::Result<Option<PathBuf>> {
    let mut name = String::new();
    match File::open(intent) {
        Ok(file) => {
            file.take(1024).read_to_string(&mut name)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    if Path::new(&name).file_name() != Some(OsStr::new(&name)) || name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid publication record",
        ));
    }
    let target = directory.join(&name);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_file() && metadata.len() == bytes => {
            Ok(same_content(staged, &target)?.then_some(target))
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn same_content(first: &Path, second: &Path) -> io::Result<bool> {
    let mut first = io::BufReader::new(File::open(first)?);
    let mut second = io::BufReader::new(File::open(second)?);
    use std::io::BufRead;
    loop {
        let left = first.fill_buf()?;
        let right = second.fill_buf()?;
        let length = left.len().min(right.len());
        if length == 0 {
            return Ok(left.is_empty() && right.is_empty());
        }
        if left[..length] != right[..length] {
            return Ok(false);
        }
        first.consume(length);
        second.consume(length);
    }
}

fn save_intent(intent: &Path, name: &str, directory: &Path) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(name.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(intent).map_err(|error| error.error)?;
    sync_directory(directory)
}

fn publish_file(staged: &Path, path: &Path, directory: &Path) -> io::Result<()> {
    match fs::hard_link(staged, path) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
        Err(_) => {}
    }
    let mut output = tempfile::NamedTempFile::new_in(directory)?;
    io::copy(&mut File::open(staged)?, output.as_file_mut())?;
    output.as_file().sync_all()?;
    output
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn discard_work(entry: &QueueEntry) -> io::Result<()> {
    let directory = &entry.working_directory;
    let id = entry.download.id.0.to_string();
    if directory.file_name() == Some(OsStr::new(&id))
        && directory
            .parent()
            .is_some_and(|parent| parent.ends_with(RECOVERY_FOLDER))
    {
        return ignore_missing(fs::remove_dir_all(directory));
    }
    ignore_missing(fs::remove_dir_all(ready_directory(entry)))?;
    ignore_missing(fs::remove_file(intent_path(entry)))?;
    // Legacy jobs can share the working folder with files owned by other downloads.
    if let Ok(url) = source_url(&entry.download.source)
        && directory.try_exists()?
    {
        for checkpoint in scan_checkpoints(directory).map_err(io::Error::other)? {
            if checkpoint.matches_url(&url)
                && checkpoint
                    .job
                    .is_some_and(|job| job.id == entry.download.id.0)
            {
                let mut sidecar = checkpoint.partial.as_os_str().to_owned();
                sidecar.push(".ctl");
                ignore_missing(fs::remove_file(&checkpoint.partial))?;
                ignore_missing(fs::remove_file(sidecar))?;
            }
        }
    }
    Ok(())
}

pub(super) fn discard_output(path: &Path) -> io::Result<()> {
    ignore_missing(fs::remove_file(path))
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruscello_core::Download;

    #[test]
    fn restarts_publication_without_creating_a_duplicate_or_overwriting_a_file() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("output");
        let id = DownloadId(1);
        let entry = QueueEntry {
            download: Download::new(
                id,
                Source::Http {
                    url: "https://example.test/file".into(),
                },
                destination.clone(),
            ),
            position: 0,
            working_directory: working_directory(&destination, id),
        };
        let ready = ready_directory(&entry);
        fs::create_dir_all(&ready)?;
        let stage = ready.join("file.bin");
        fs::write(&stage, b"completed data")?;
        fs::write(destination.join("file.bin"), b"existing file")?;
        let first = publish(&entry, &stage)?;
        let restarted = publish(&entry, &stage)?;
        assert_eq!(first.path, destination.join("file (1).bin"));
        assert_eq!(restarted.path, first.path);
        assert_eq!(fs::read(destination.join("file.bin"))?, b"existing file");
        assert!(!destination.join("file (2).bin").exists());
        assert_eq!(fs::read(first.path)?, b"completed data");
        Ok(())
    }
}
