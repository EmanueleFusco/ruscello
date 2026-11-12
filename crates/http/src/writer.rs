use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    ops::Range,
    path::{Path, PathBuf},
};

use bytes::Bytes;
use tempfile::TempPath;
use tokio::sync::{mpsc, oneshot};

mod disk;
mod names;

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
}

/// Owns a bounded channel to a dedicated file writer. Dropping it leaves the partial file in place.
#[derive(Debug)]
pub struct FileWriter {
    commands: mpsc::Sender<Command>,
    partial: PathBuf,
}

impl FileWriter {
    /// Starts a new download beside its eventual output file.
    pub async fn create(
        directory: &Path,
        filename: &str,
        size: Option<u64>,
    ) -> Result<Self, WriterError> {
        let directory = directory.to_owned();
        let filename = sanitize_filename(filename);
        let (commands, receiver) = mpsc::channel(16);
        let (ready, started) = oneshot::channel();
        tokio::task::spawn_blocking(move || match Worker::create(&directory, filename, size) {
            Ok(worker) => {
                if ready.send(Ok(worker.partial.clone())).is_ok() {
                    worker.run(receiver);
                }
            }
            Err(error) => {
                let _ = ready.send(Err(error));
            }
        });
        let partial = started.await.map_err(|_| WriterError::Stopped)??;
        Ok(Self { commands, partial })
    }

    pub fn partial_path(&self) -> &Path {
        &self.partial
    }

    /// Stages a positional write. Call [Self::sync_data] before relying on its durability.
    pub async fn write_at(&mut self, offset: u64, bytes: Bytes) -> Result<(), WriterError> {
        self.send(|reply| Command::Write {
            offset,
            bytes,
            reply,
        })
        .await
    }

    pub async fn sync_data(&mut self) -> Result<(), WriterError> {
        self.send(Command::Sync).await
    }

    /// Syncs and publishes a complete file without overwriting an existing destination.
    pub async fn finish(self) -> Result<PathBuf, WriterError> {
        self.send(Command::Finish).await
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

enum Command {
    Write {
        offset: u64,
        bytes: Bytes,
        reply: Reply<()>,
    },
    Sync(Reply<()>),
    Finish(Reply<PathBuf>),
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
}

impl Worker {
    fn create(directory: &Path, name: String, size: Option<u64>) -> Result<Self, WriterError> {
        let directory = fs::canonicalize(directory)?;
        for index in 0..CANDIDATES {
            let filename = names::candidate(&name, index);
            if directory.join(&filename).try_exists()? {
                continue;
            }
            let partial = directory.join(format!("{filename}.rscpart"));
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
            return Ok(Self {
                file,
                partial,
                name,
                directory,
                size,
                writes: BTreeMap::new(),
                buffered: 0,
                covered: Coverage::default(),
            });
        }
        Err(WriterError::NoFilename)
    }

    fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        while let Some(command) = commands.blocking_recv() {
            let (result, reply) = match command {
                Command::Write {
                    offset,
                    bytes,
                    reply,
                } => (self.write(offset, &bytes), reply),
                Command::Sync(reply) => (self.sync(), reply),
                Command::Finish(reply) => {
                    let _ = reply.send(self.finish());
                    return;
                }
            };
            let failed = result.is_err();
            let _ = reply.send(result);
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

    fn finish(mut self) -> Result<PathBuf, WriterError> {
        if !self.covered.complete(self.size) {
            return Err(WriterError::Incomplete);
        }
        self.sync()?;
        drop(self.file);
        let mut partial = TempPath::try_from_path(self.partial)?;
        partial.disable_cleanup(true);
        for index in 0..CANDIDATES {
            let destination = self.directory.join(names::candidate(&self.name, index));
            match partial.persist_noclobber(&destination) {
                Ok(()) => {
                    disk::sync_directory(&self.directory)?;
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

#[derive(Default)]
struct Coverage(BTreeMap<u64, u64>);

impl Coverage {
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
