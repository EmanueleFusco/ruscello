use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use ruscello_core::{DownloadId, FailureReason, FileType, Source};
use ruscello_http::{
    BandwidthLimit, CheckpointJob, DownloadOptions, DownloadProgress, ErrorClass, HttpDownloader,
    HttpError, RetryPolicy, Url, WriterError, filename_candidate, scan_checkpoints,
};
use ruscello_storage::QueueEntry;
use tokio::{
    sync::watch,
    time::{self, Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use crate::{
    BandwidthContext, DiskPolicy, EngineError, Limits, MediaEngine, MediaError, MediaPhase,
    MediaTransferJob, RECOVERY_FOLDER, SidecarSettings, disk::Preparation, file_types,
    media_bridge,
};

#[derive(Debug)]
pub(super) struct Published {
    pub path: PathBuf,
    pub bytes: u64,
    pub file_type: FileType,
}

pub(super) struct Context {
    pub http: HttpDownloader,
    pub media: MediaEngine,
    pub sidecars: SidecarSettings,
    pub resolver: crate::Resolver,
    pub limits: Limits,
    pub bandwidth: BandwidthLimit,
    pub disk: DiskPolicy,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TransferError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error("download file operation failed")]
    File(#[from] io::Error),
    #[error("a file operation stopped unexpectedly")]
    Worker(#[from] tokio::task::JoinError),
    #[error("the saved checksum is invalid")]
    Checksum(#[from] crate::ChecksumError),
    #[error(transparent)]
    Resolver(#[from] crate::ResolveError),
}

impl TransferError {
    pub(super) fn cancelled(&self) -> bool {
        matches!(
            self,
            Self::Http(HttpError::Cancelled)
                | Self::Resolver(crate::ResolveError::Cancelled)
                | Self::Media(MediaError::Cancelled)
        )
    }

    pub(super) fn verification_required(&self) -> bool {
        matches!(
            self,
            Self::Http(HttpError::VerificationRequired)
                | Self::Resolver(crate::ResolveError::VerificationRequired)
        )
    }

    pub(super) fn low_disk(&self) -> bool {
        matches!(self, Self::File(error) | Self::Http(HttpError::Storage(WriterError::Io(error)))
            if matches!(error.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded))
            || matches!(self, Self::Media(error) if error.is_low_disk())
    }

    pub(super) fn reason(&self) -> FailureReason {
        match self {
            Self::File(_) => FailureReason::FileSystem,
            Self::Worker(_) => FailureReason::Internal,
            Self::Checksum(_) => FailureReason::IntegrityCheck,
            Self::Media(error) => {
                if error.is_low_disk() {
                    FailureReason::FileSystem
                } else if error.is_access_denied() {
                    FailureReason::AccessDenied
                } else if error.is_network() {
                    FailureReason::Network
                } else if matches!(
                    error,
                    MediaError::InvalidUrl(_) | MediaError::UnsupportedManifest(_)
                ) {
                    FailureReason::UnsupportedSource
                } else if matches!(error, MediaError::ChecksumMismatch { .. }) {
                    FailureReason::IntegrityCheck
                } else if matches!(error, MediaError::Io(_)) {
                    FailureReason::FileSystem
                } else {
                    FailureReason::SourceUnavailable
                }
            }
            Self::Resolver(crate::ResolveError::Request(_)) => FailureReason::Network,
            Self::Resolver(crate::ResolveError::InvalidLink) => FailureReason::UnsupportedSource,
            Self::Resolver(crate::ResolveError::AccountRequired | crate::ResolveError::Keyring) => {
                FailureReason::AccessDenied
            }
            Self::Resolver(_) => FailureReason::SourceUnavailable,
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
    let (Source::Http { url } | Source::Hoster { url }) = source else {
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
    context: Context,
    entry: QueueEntry,
    url: Url,
    cancellation: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
) -> Result<Published, TransferError> {
    let (ready, existing) = tokio::task::spawn_blocking({
        let ready = ready_directory(&entry);
        let working = entry.working_directory.clone();
        let disk = context.disk.clone();
        move || {
            fs::create_dir_all(&ready)?;
            let existing = ready_file(&ready)?;
            if existing.is_none() {
                disk.check(&working, 0)?;
            }
            Ok::<_, io::Error>((ready, existing))
        }
    })
    .await??;
    let staged = match existing {
        Some(staged) => staged,
        None => {
            let transfer_cancellation = cancellation.child_token();
            let default_connections = context.limits.connections_per_download;
            let mut options = DownloadOptions {
                directory: entry.working_directory.clone(),
                connections: entry
                    .download
                    .max_connections
                    .unwrap_or(default_connections)
                    .min(default_connections)
                    .min(context.limits.connections_per_host),
                expected_checksum: entry
                    .download
                    .checksum
                    .as_deref()
                    .map(crate::parse_checksum)
                    .transpose()?,
                cancellation: transfer_cancellation.clone(),
                retry: RetryPolicy {
                    max_retries: context.limits.retries,
                    ..RetryPolicy::default()
                },
                bandwidth: context.bandwidth,
                download_bandwidth: entry.download.bandwidth,
                progress: Some(progress),
                filename_hint: entry.http.output_filename.clone(),
                mirrors: entry
                    .http
                    .mirrors
                    .iter()
                    .filter_map(|url| Url::parse(url).ok())
                    .collect(),
                checkpoint_job: Some(CheckpointJob {
                    id: entry.download.id.0,
                    destination: entry.download.destination.clone(),
                }),
                completion_directory: Some(ready),
                preparation: Some(Arc::new(Preparation {
                    policy: context.disk.clone(),
                    directory: entry.working_directory.clone(),
                    mime_path: mime_path(&entry),
                    id: entry.download.id.0,
                    url: url.clone(),
                })),
                ..DownloadOptions::default()
            };
            crate::resolution::prepare_http_download(
                &context.resolver,
                &url,
                &mut options,
                matches!(entry.download.source, Source::Hoster { .. }),
                None,
            )
            .await?;
            monitored_download(
                &context.http,
                &url,
                options,
                &context.disk,
                transfer_cancellation,
            )
            .await?
        }
    };
    if cancellation.is_cancelled() {
        return Err(HttpError::Cancelled.into());
    }
    let published =
        tokio::task::spawn_blocking(move || publish(&entry, &staged, &context.disk)).await??;
    Ok(published)
}

pub(super) async fn run_media(
    context: Context,
    entry: QueueEntry,
    url: Url,
    cancellation: CancellationToken,
    progress: watch::Sender<DownloadProgress>,
    phase: watch::Sender<MediaPhase>,
) -> Result<Published, TransferError> {
    let (ready, existing) = tokio::task::spawn_blocking({
        let ready = ready_directory(&entry);
        let working = entry.working_directory.clone();
        let disk = context.disk.clone();
        move || {
            fs::create_dir_all(&ready)?;
            let existing = ready_file(&ready)?;
            if existing.is_none() {
                disk.check(&working, 0)?;
            }
            Ok::<_, io::Error>((ready, existing))
        }
    })
    .await??;

    let staged = match existing {
        Some(staged) => staged,
        None => {
            let transfer_cancellation = cancellation.child_token();
            let request = media_bridge::from_queue_state(entry.media.as_ref(), &url);
            let session_headers =
                media_bridge::session_headers(&context.resolver, &url, &transfer_cancellation)
                    .await;
            let default_connections = context.limits.connections_per_download;
            let concurrency = entry
                .download
                .max_connections
                .unwrap_or(default_connections)
                .min(default_connections)
                .min(context.limits.connections_per_host)
                .get();
            let bandwidth = BandwidthContext {
                category: context.bandwidth.clone(),
                global: context.http.global_bandwidth(),
                individual: entry
                    .download
                    .bandwidth
                    .map(|rate| BandwidthLimit::new(Some(rate))),
            };
            let phase_tx = phase.clone();
            let on_phase = move |next_phase| {
                let _ = phase_tx.send(next_phase);
            };
            let filename_hint = entry
                .http
                .output_filename
                .as_deref()
                .or(entry.download.filename.as_deref());
            let working = entry.working_directory.clone();
            let job = MediaTransferJob {
                url: &url,
                request: &request,
                session_headers: &session_headers,
                working_dir: &working,
                ready_dir: &ready,
                filename_hint,
                concurrency,
                bandwidth: &bandwidth,
                settings: &context.sidecars,
                cancellation: &transfer_cancellation,
                progress: Some(&progress),
                on_phase: &on_phase,
            };
            monitored_media_download(
                &context.media,
                job,
                &working,
                &context.disk,
                &transfer_cancellation,
            )
            .await?
        }
    };
    if cancellation.is_cancelled() {
        return Err(MediaError::Cancelled.into());
    }
    let published =
        tokio::task::spawn_blocking(move || publish(&entry, &staged, &context.disk)).await??;
    Ok(published)
}

async fn monitored_media_download(
    media: &MediaEngine,
    job: MediaTransferJob<'_>,
    working: &Path,
    disk: &DiskPolicy,
    cancellation: &CancellationToken,
) -> Result<PathBuf, TransferError> {
    let mut transfer = Box::pin(media.download(job));
    let period = disk.interval();
    let mut checks = time::interval_at(Instant::now() + period, period);
    checks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut transfer => return Ok(result?),
            _ = checks.tick() => {}
        }
        let working = working.to_path_buf();
        let disk = disk.clone();
        let check = tokio::task::spawn_blocking(move || disk.check(&working, 0));
        let result = tokio::select! {
            result = &mut transfer => return Ok(result?),
            result = check => result.map_err(TransferError::from)
                .and_then(|check| check.map_err(TransferError::from)),
        };
        if let Err(error) = result {
            cancellation.cancel();
            let _ = transfer.await;
            return Err(error);
        }
    }
}

async fn monitored_download(
    http: &HttpDownloader,
    url: &Url,
    options: DownloadOptions,
    disk: &DiskPolicy,
    cancellation: CancellationToken,
) -> Result<PathBuf, TransferError> {
    let working = options.directory.clone();
    let mut transfer = Box::pin(http.download(url, options));
    let period = disk.interval();
    let mut checks = time::interval_at(Instant::now() + period, period);
    checks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut transfer => return Ok(result?),
            _ = checks.tick() => {}
        }
        let working = working.clone();
        let disk = disk.clone();
        let check = tokio::task::spawn_blocking(move || disk.check(&working, 0));
        let result = tokio::select! {
            result = &mut transfer => return Ok(result?),
            result = check => result.map_err(TransferError::from)
                .and_then(|check| check.map_err(TransferError::from)),
        };
        if let Err(error) = result {
            cancellation.cancel();
            let _ = transfer.await;
            return Err(error);
        }
    }
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

fn mime_path(entry: &QueueEntry) -> PathBuf {
    entry
        .working_directory
        .join(format!(".mime-{}", entry.download.id.0))
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

fn publish(entry: &QueueEntry, staged: &Path, disk: &DiskPolicy) -> io::Result<Published> {
    let directory = &entry.download.destination;
    fs::create_dir_all(directory)?;
    let bytes = staged.metadata()?.len();
    let mut mime = String::new();
    let content_type = File::open(mime_path(entry))
        .and_then(|file| file.take(255).read_to_string(&mut mime))
        .ok()
        .map(|_| mime.as_str());
    let file_type = file_types::detect(staged, content_type, entry.download.source.kind());
    let intent = intent_path(entry);
    if let Some(path) = published_target(&intent, directory, staged, bytes)? {
        sync_directory(directory)?;
        return Ok(Published {
            path,
            bytes,
            file_type,
        });
    }
    ignore_missing(fs::remove_file(copy_path(directory, entry.download.id)))?;
    disk.check(directory, 0)?;
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
        write_marker(&intent, &name, &entry.working_directory)?;
        match publish_file(staged, &path, directory, disk, bytes, entry.download.id) {
            Ok(()) => {
                sync_directory(directory)?;
                return Ok(Published {
                    path,
                    bytes,
                    file_type,
                });
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

pub(super) fn write_marker(intent: &Path, name: &str, directory: &Path) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(name.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(intent).map_err(|error| error.error)?;
    sync_directory(directory)
}

fn publish_file(
    staged: &Path,
    path: &Path,
    directory: &Path,
    disk: &DiskPolicy,
    bytes: u64,
    id: DownloadId,
) -> io::Result<()> {
    match fs::hard_link(staged, path) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
        Err(_) => {}
    }
    copy_staged(staged, path, directory, disk, bytes, id)
}

fn copy_path(directory: &Path, id: DownloadId) -> PathBuf {
    directory
        .join(RECOVERY_FOLDER)
        .join(format!(".publish-{}.rscpart", id.0))
}

fn copy_staged(
    staged: &Path,
    path: &Path,
    directory: &Path,
    disk: &DiskPolicy,
    bytes: u64,
    id: DownloadId,
) -> io::Result<()> {
    let scratch = copy_path(directory, id);
    fs::create_dir_all(directory.join(RECOVERY_FOLDER))?;
    // The completed staging file remains available if a previous copy was interrupted.
    ignore_missing(fs::remove_file(&scratch))?;
    disk.check(directory, bytes)?;
    let output = File::create_new(&scratch)?;
    let scratch = tempfile::TempPath::try_from_path(scratch)?;
    {
        let mut output = output;
        io::copy(&mut File::open(staged)?, &mut output)?;
        output.sync_all()?;
    }
    scratch
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
    ignore_missing(fs::remove_file(copy_path(
        &entry.download.destination,
        entry.download.id,
    )))?;
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
    ignore_missing(fs::remove_file(mime_path(entry)))?;
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

    #[derive(Debug)]
    struct FixedSpace(u64);

    impl crate::DiskSpaceProbe for FixedSpace {
        fn available_bytes(&self, _directory: &Path) -> io::Result<u64> {
            Ok(self.0)
        }
    }

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
            torrent: None,
            http: Default::default(),
            media: None,
        };
        let ready = ready_directory(&entry);
        fs::create_dir_all(&ready)?;
        let stage = ready.join("file.bin");
        fs::write(&stage, b"completed data")?;
        fs::write(destination.join("file.bin"), b"existing file")?;
        let disk = DiskPolicy::default().with_probe(Arc::new(FixedSpace(u64::MAX)));
        let first = publish(&entry, &stage, &disk)?;
        let restarted = publish(&entry, &stage, &disk)?;
        assert_eq!(first.path, destination.join("file (1).bin"));
        assert_eq!(restarted.path, first.path);
        assert_eq!(fs::read(destination.join("file.bin"))?, b"existing file");
        assert!(!destination.join("file (2).bin").exists());
        assert_eq!(fs::read(first.path)?, b"completed data");
        Ok(())
    }

    #[test]
    fn retries_an_interrupted_copy_after_space_is_available() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("output");
        let stage = temp.path().join("ready");
        let output = destination.join("file.bin");
        let id = DownloadId(1);
        let contents = b"completed data";
        fs::write(&stage, contents)?;
        let scratch = copy_path(&destination, id);
        fs::create_dir_all(destination.join(RECOVERY_FOLDER))?;
        fs::write(&scratch, b"unfinished copy")?;
        let mut disk = DiskPolicy::default().with_probe(Arc::new(FixedSpace(8)));
        disk.minimum_free_bytes = 0;
        let bytes = contents.len() as u64;
        assert!(matches!(
            copy_staged(&stage, &output, &destination, &disk, bytes, id),
            Err(error) if error.kind() == io::ErrorKind::StorageFull
        ));
        assert!(!scratch.exists());
        assert!(!output.exists());
        assert_eq!(fs::read(&stage)?, contents);
        let disk = disk.with_probe(Arc::new(FixedSpace(bytes)));
        copy_staged(&stage, &output, &destination, &disk, bytes, id)?;
        assert_eq!(fs::read(&output)?, contents);
        assert_eq!(fs::read(&stage)?, contents);
        assert!(!scratch.exists());
        Ok(())
    }
}
