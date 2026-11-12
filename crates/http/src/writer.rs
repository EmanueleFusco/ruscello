use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Seek},
    ops::Range,
    path::{Path, PathBuf},
};

use bytes::Bytes;
use tempfile::TempPath;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::ExpectedChecksum;

mod control;
mod disk;
mod names;

pub use control::CheckpointJob;
pub(crate) use control::{Recovery, TransferKey};
pub use names::candidate as filename_candidate;
pub use names::sanitize_filename;

const BATCH_BYTES: usize = 1024 * 1024;
const BUFFER_BYTES: usize = 4 * BATCH_BYTES;
const MAX_BUFFERS: usize = 256;
const CANDIDATES: u32 = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum WriterError {
    #[error("download file I/O failed")]
    Io(#[from] io::Error),
    #[error("the writer stopped")]
    Stopped,
    #[error("write is outside the declared file size")]
    Bounds,
    #[error("the file has gaps and cannot be finalized")]
    Incomplete,
    #[error("no available filename")]
    NoFilename,
    #[error("another process is writing this download")]
    Busy,
    #[error("the downloaded file did not match the expected checksum")]
    ChecksumMismatch,
    #[error("file verification was cancelled")]
    Cancelled,
}

/// Owns a bounded channel to a dedicated file writer. Dropping it leaves the partial file in place.
#[derive(Debug)]
pub struct FileWriter {
    commands: mpsc::Sender<Command>,
    partial: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl FileWriter {
    /// Starts a new download beside its eventual output file.
    pub async fn create(
        directory: &Path,
        filename: &str,
        size: Option<u64>,
    ) -> Result<Self, WriterError> {
        Ok(Self::start(directory, filename, size, None, None).await?.0)
    }

    pub(crate) async fn open_download(
        directory: &Path,
        filename: &str,
        key: TransferKey,
        recovery: Option<Recovery>,
    ) -> Result<(Self, Vec<Range<u64>>, bool), WriterError> {
        Self::start(directory, filename, key.size, Some(key), recovery).await
    }

    async fn start(
        directory: &Path,
        filename: &str,
        size: Option<u64>,
        key: Option<TransferKey>,
        recovery: Option<Recovery>,
    ) -> Result<(Self, Vec<Range<u64>>, bool), WriterError> {
        let directory = directory.to_owned();
        let filename = sanitize_filename(filename);
        let (commands, receiver) = mpsc::channel(16);
        let (ready, started) = oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            match Worker::create(&directory, filename, size, key, recovery) {
                Ok((worker, restarted)) => {
                    if ready
                        .send(Ok((
                            worker.partial.clone(),
                            worker.covered.ranges(),
                            restarted,
                        )))
                        .is_ok()
                    {
                        worker.run(receiver);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            }
        });
        let (partial, ranges, restarted) = started.await.map_err(|_| WriterError::Stopped)??;
        Ok((
            Self {
                commands,
                partial,
                task,
            },
            ranges,
            restarted,
        ))
    }

    pub fn partial_path(&self) -> &Path {
        &self.partial
    }

    /// Stages a positional write. Call [Self::sync_data] before relying on its durability.
    pub async fn write_at(&mut self, offset: u64, bytes: Bytes) -> Result<(), WriterError> {
        self.send(|reply| Command::Write {
            offset,
            bytes,
            reply: Some(reply),
        })
        .await
    }

    /// The checkpoint and finalization commands run after all queued writes.
    pub(crate) async fn queue_at(&mut self, offset: u64, bytes: Bytes) -> Result<(), WriterError> {
        self.commands
            .send(Command::Write {
                offset,
                bytes,
                reply: None,
            })
            .await
            .map_err(|_| WriterError::Stopped)
    }

    pub async fn sync_data(&mut self) -> Result<(), WriterError> {
        self.send(Command::Sync).await
    }

    pub(crate) async fn checkpoint(&mut self) -> Result<(), WriterError> {
        self.send(Command::Checkpoint).await
    }

    pub(crate) async fn invalidate_checkpoint(&mut self) -> Result<(), WriterError> {
        self.send(Command::Invalidate).await
    }

    pub(crate) async fn close(self) {
        drop(self.commands);
        let _ = self.task.await;
    }

    /// Syncs and publishes a complete file without overwriting an existing destination.
    pub async fn finish(self) -> Result<PathBuf, WriterError> {
        self.finish_into(None).await
    }

    pub(crate) async fn finish_into(
        self,
        directory: Option<PathBuf>,
    ) -> Result<PathBuf, WriterError> {
        self.send(|reply| Command::Finish {
            verification: None,
            directory,
            reply,
        })
        .await
    }

    pub(crate) async fn finish_verified(
        self,
        checksum: ExpectedChecksum,
        cancellation: CancellationToken,
        directory: Option<PathBuf>,
    ) -> Result<PathBuf, WriterError> {
        self.send(|reply| Command::Finish {
            verification: Some(Verification {
                checksum,
                cancellation,
            }),
            directory,
            reply,
        })
        .await
    }

    async fn send<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, WriterError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| WriterError::Stopped)?;
        response.await.map_err(|_| WriterError::Stopped)?
    }
}

type Reply<T> = oneshot::Sender<Result<T, WriterError>>;

struct Verification {
    checksum: ExpectedChecksum,
    cancellation: CancellationToken,
}

enum Command {
    Write {
        offset: u64,
        bytes: Bytes,
        reply: Option<Reply<()>>,
    },
    Sync(Reply<()>),
    Checkpoint(Reply<()>),
    Invalidate(Reply<()>),
    Finish {
        verification: Option<Verification>,
        directory: Option<PathBuf>,
        reply: Reply<PathBuf>,
    },
}

struct Worker {
    file: File,
    partial: PathBuf,
    name: String,
    directory: PathBuf,
    size: Option<u64>,
    writes: BTreeMap<u64, Vec<u8>>,
    buffered: usize,
    covered: Coverage,
    control: Option<control::Control>,
}

impl Worker {
    fn create(
        directory: &Path,
        name: String,
        size: Option<u64>,
        key: Option<TransferKey>,
        recovery: Option<Recovery>,
    ) -> Result<(Self, bool), WriterError> {
        let directory = fs::canonicalize(directory)?;
        for index in 0..CANDIDATES {
            let filename = names::candidate(&name, index);
            let partial = directory.join(format!("{filename}.rscpart"));
            let sidecar = control::path(&partial);
            if let Some(key) = &key
                && let Some(resumed) =
                    Self::reopen(&directory, &name, &partial, key, recovery.as_ref())?
            {
                return Ok(resumed);
            }
            if directory.join(&filename).try_exists()? {
                continue;
            }
            if sidecar.try_exists()? {
                continue;
            }
            let file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&partial)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            };
            if let Some(size) = size
                && let Err(error) = disk::prepare(&file, size)
            {
                drop(file);
                let _ = fs::remove_file(&partial);
                return Err(error.into());
            }
            let mut worker = Self::new(file, partial, name, directory, size);
            if let Some(key) = key {
                lock_partial(&worker.file)?;
                worker.control = Some(control::Control::empty(key, recovery));
                worker.checkpoint()?;
            }
            return Ok((worker, false));
        }
        Err(WriterError::NoFilename)
    }

    fn new(
        file: File,
        partial: PathBuf,
        name: String,
        directory: PathBuf,
        size: Option<u64>,
    ) -> Self {
        Self {
            file,
            partial,
            name,
            directory,
            size,
            writes: BTreeMap::new(),
            buffered: 0,
            covered: Coverage::default(),
            control: None,
        }
    }

    fn reopen(
        directory: &Path,
        name: &str,
        partial: &Path,
        key: &TransferKey,
        recovery: Option<&Recovery>,
    ) -> Result<Option<(Self, bool)>, WriterError> {
        let path = control::path(partial);
        if matching_checkpoint(&path, key, recovery)?.is_none() {
            return Ok(None);
        }
        match fs::symlink_metadata(partial) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let file = match OpenOptions::new().read(true).write(true).open(partial) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        lock_partial(&file)?;
        let Some(previous) = matching_checkpoint(&path, key, recovery)? else {
            return Ok(None);
        };
        let length = file.metadata()?.len();
        let resumable = key.can_resume(&previous.key) && Some(length) == key.size;
        let mut worker = Self::new(
            file,
            partial.to_owned(),
            name.to_owned(),
            directory.to_owned(),
            key.size,
        );
        worker.control = Some(control::Control::empty(key.clone(), recovery.cloned()));
        if resumable {
            for range in previous.completed {
                worker.covered.add(range);
            }
        } else {
            // Publish an empty checkpoint before truncation. An interruption after
            // truncation must never leave the old record claiming those bytes.
            worker.checkpoint()?;
            worker.file.set_len(0)?;
            if let Some(size) = key.size {
                disk::prepare(&worker.file, size)?;
            }
        }
        Ok(Some((worker, !resumable)))
    }

    fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.blocking_recv() {
            let (result, reply) = match command {
                Command::Write {
                    offset,
                    bytes,
                    reply,
                } => (self.write(offset, &bytes), reply),
                Command::Sync(reply) => (self.sync(), Some(reply)),
                Command::Checkpoint(reply) => (self.checkpoint(), Some(reply)),
                Command::Invalidate(reply) => (self.invalidate_checkpoint(), Some(reply)),
                Command::Finish {
                    verification,
                    directory,
                    reply,
                } => {
                    let _ = reply.send(self.finish(verification, directory));
                    return;
                }
            };
            let failed = result.is_err();
            if let Some(reply) = reply {
                let _ = reply.send(result);
            } else if let Err(error) = result {
                tracing::debug!(%error, "Queued download write failed");
            }
            if failed {
                return;
            }
        }
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<(), WriterError> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(WriterError::Bounds)?;
        if self.size.is_some_and(|size| end > size) {
            return Err(WriterError::Bounds);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let preceding = self
            .writes
            .range(..=offset)
            .next_back()
            .map(|(&start, bytes)| (start, start + bytes.len() as u64));
        let overlap = preceding.is_some_and(|(_, end)| end > offset)
            || self.writes.range(offset..end).next().is_some();
        if overlap {
            self.flush()?;
        }
        if bytes.len() >= BATCH_BYTES {
            disk::write_at(&self.file, bytes, offset)?;
        } else {
            let start = preceding
                .filter(|&(_, end)| !overlap && end == offset)
                .map_or(offset, |(start, _)| start);
            let buffer = self.writes.entry(start).or_default();
            buffer.extend_from_slice(bytes);
            self.buffered += bytes.len();
            if buffer.len() >= BATCH_BYTES
                && let Some(buffer) = self.writes.remove(&start)
            {
                self.buffered -= buffer.len();
                disk::write_at(&self.file, &buffer, start)?;
            }
            while self.buffered >= BUFFER_BYTES || self.writes.len() >= MAX_BUFFERS {
                self.flush_one()?;
            }
        }
        self.covered.add(offset..end);
        Ok(())
    }

    fn flush_one(&mut self) -> io::Result<()> {
        if let Some((start, bytes)) = self.writes.pop_first() {
            self.buffered -= bytes.len();
            disk::write_at(&self.file, &bytes, start)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        while !self.writes.is_empty() {
            self.flush_one()?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), WriterError> {
        self.flush()?;
        self.file.sync_data()?;
        tracing::debug!("Download data synchronized");
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), WriterError> {
        self.sync()?;
        if let Some(control) = &mut self.control {
            control.completed = self.covered.ranges();
            control::write(&self.directory, &control::path(&self.partial), control)?;
        }
        Ok(())
    }

    fn invalidate_checkpoint(&mut self) -> Result<(), WriterError> {
        self.covered = Coverage::default();
        self.checkpoint()
    }

    fn finish(
        mut self,
        verification: Option<Verification>,
        directory: Option<PathBuf>,
    ) -> Result<PathBuf, WriterError> {
        if !self.covered.complete(self.size) {
            return Err(WriterError::Incomplete);
        }
        if let Some(verification) = verification {
            self.checkpoint()?;
            self.file.rewind()?;
            let verified = verification
                .checksum
                .verify(&mut self.file, &verification.cancellation);
            if verification.cancellation.is_cancelled() {
                return Err(WriterError::Cancelled);
            }
            if !verified? {
                self.invalidate_checkpoint()?;
                tracing::warn!("Download checksum did not match");
                return Err(WriterError::ChecksumMismatch);
            }
            tracing::debug!("Download checksum verified");
        } else {
            self.sync()?;
        }
        let control_path = control::path(&self.partial);
        let mut partial = TempPath::try_from_path(self.partial)?;
        partial.disable_cleanup(true);
        let directory = directory.as_deref().unwrap_or(&self.directory);
        for index in 0..CANDIDATES {
            let destination = directory.join(names::candidate(&self.name, index));
            match partial.persist_noclobber(&destination) {
                Ok(()) => {
                    if directory != self.directory {
                        disk::sync_directory(directory)?;
                    }
                    disk::sync_directory(&self.directory)?;
                    if self.control.is_some()
                        && let Err(error) = fs::remove_file(&control_path)
                        && error.kind() != io::ErrorKind::NotFound
                    {
                        tracing::warn!(kind = ?error.kind(), "Could not remove completed checkpoint");
                    }
                    tracing::debug!("Published completed download");
                    return Ok(destination);
                }
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                    partial = error.path
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        Err(WriterError::NoFilename)
    }
}

fn matching_checkpoint(
    path: &Path,
    key: &TransferKey,
    recovery: Option<&Recovery>,
) -> Result<Option<control::Control>, WriterError> {
    match control::read(path) {
        Ok(control) => {
            let previous = control.recovery.as_ref().map(|recovery| recovery.job.id);
            let current = recovery.map(|recovery| recovery.job.id);
            Ok(
                (control.key.source == key.source && (previous.is_none() || previous == current))
                    .then_some(control),
            )
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            tracing::warn!("Ignoring an invalid download checkpoint");
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn lock_partial(file: &File) -> Result<(), WriterError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(fs::TryLockError::WouldBlock) => Err(WriterError::Busy),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Reads the byte count recorded by the most recent valid checkpoint.
pub fn persisted_bytes(partial: &Path) -> Result<u64, WriterError> {
    let control = control::read(&control::path(partial))?;
    Ok(control
        .completed
        .iter()
        .map(|range| range.end - range.start)
        .sum())
}

/// Validated checkpoint metadata for startup reconciliation.
#[derive(Debug)]
pub struct Checkpoint {
    pub partial: PathBuf,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub job: Option<CheckpointJob>,
    source: [u8; 32],
    recovery_url: Option<reqwest::Url>,
}

impl Checkpoint {
    pub fn matches_url(&self, url: &reqwest::Url) -> bool {
        self.source == control::url_identity(url)
    }

    pub fn recovery_url(&self) -> Option<&reqwest::Url> {
        self.recovery_url.as_ref()
    }
}

/// Enumerates regular partial files backed by valid checkpoints in one directory.
pub fn scan_checkpoints(directory: &Path) -> Result<Vec<Checkpoint>, WriterError> {
    let mut checkpoints = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(filename) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".rscpart.ctl"))
        else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        let partial = directory.join(format!("{filename}.rscpart"));
        match inspect(&partial, &entry.path()) {
            Ok(checkpoint) => checkpoints.push(checkpoint),
            Err(WriterError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                ) =>
            {
                tracing::debug!(kind = ?error.kind(), "Ignoring an incomplete checkpoint pair");
            }
            Err(error) => return Err(error),
        }
    }
    checkpoints.sort_unstable_by(|left, right| left.partial.cmp(&right.partial));
    Ok(checkpoints)
}

fn inspect(partial: &Path, sidecar: &Path) -> Result<Checkpoint, WriterError> {
    let metadata = fs::symlink_metadata(partial)?;
    let control = control::read(sidecar)?;
    let length = metadata.len();
    if !metadata.is_file()
        || control.key.size.is_some_and(|size| size != length)
        || control
            .completed
            .last()
            .is_some_and(|range| range.end > length)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid partial file").into());
    }
    let recovery_url = control
        .recovery
        .as_ref()
        .and_then(|record| record.source.as_deref())
        .and_then(|url| reqwest::Url::parse(url).ok())
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && control::url_identity(url) == control.key.source
        });
    Ok(Checkpoint {
        partial: partial.to_owned(),
        total_bytes: control.key.size,
        downloaded_bytes: control
            .completed
            .iter()
            .map(|range| range.end - range.start)
            .sum(),
        job: control.recovery.map(|record| record.job),
        source: control.key.source,
        recovery_url,
    })
}

#[derive(Default)]
struct Coverage(BTreeMap<u64, u64>);

impl Coverage {
    fn ranges(&self) -> Vec<Range<u64>> {
        self.0.iter().map(|(&start, &end)| start..end).collect()
    }

    fn add(&mut self, range: Range<u64>) {
        let (mut start, mut end) = (range.start, range.end);
        if let Some((&earlier, &earlier_end)) = self.0.range(..=start).next_back()
            && earlier_end >= start
        {
            start = earlier;
            end = end.max(earlier_end);
            self.0.remove(&earlier);
        }
        while let Some((&next, &next_end)) = self.0.range(start..=end).next() {
            self.0.remove(&next);
            end = end.max(next_end);
        }
        self.0.insert(start, end);
    }

    fn complete(&self, size: Option<u64>) -> bool {
        let end = self.0.last_key_value().map_or(0, |(_, &end)| end);
        if let Some(size) = size
            && end != size
        {
            return false;
        }
        end == 0 || self.0.len() == 1 && self.0.get(&0) == Some(&end)
    }
}
