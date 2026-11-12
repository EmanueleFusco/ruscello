//! ruscello-storage
//!
//! SQLite persistence: schema migrations, downloads/packages/categories/settings repositories (see ADR-0006).
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod database;

use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc};

use ruscello_core::{Category, CategoryId, Download, DownloadId, Package, PackageId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

const MAILBOX: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("queue database I/O failed")]
    Io(#[from] io::Error),
    #[error("queue database operation failed")]
    Sql(#[from] rusqlite::Error),
    #[error("queue record could not be encoded or read")]
    Record(#[from] serde_json::Error),
    #[error("another process owns the queue database")]
    Busy,
    #[error("queue database integrity check failed")]
    Corrupt,
    #[error("queue database uses a newer schema version ({0})")]
    NewerSchema(u32),
    #[error("queue storage has stopped")]
    Stopped,
    #[error("queue write transaction failed")]
    Batch(#[source] Arc<Self>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub download: Download,
    pub position: u64,
    pub working_directory: PathBuf,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    pub queue: Vec<QueueEntry>,
    pub packages: Vec<Package>,
    pub categories: Vec<Category>,
    pub settings: BTreeMap<String, Value>,
}

#[derive(Debug)]
pub enum Change {
    SaveDownload(Box<QueueEntry>),
    RemoveDownload(DownloadId),
    SavePackage(Package),
    RemovePackage(PackageId),
    SaveCategory(Category),
    RemoveCategory(CategoryId),
    SaveSetting { key: String, value: Value },
    FinishCheckpointRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatabaseRecovery {
    Unchanged,
    Created,
    RestoredBackup,
    Rebuilt,
}

type Reply<T> = oneshot::Sender<Result<T, StorageError>>;

enum Command {
    Read(Reply<Snapshot>),
    Write(Vec<Change>, Reply<()>),
    Close(Reply<()>),
}

/// Bounded asynchronous access to one connection on a dedicated blocking worker.
#[derive(Debug, Clone)]
pub struct Storage {
    sender: mpsc::Sender<Command>,
}

impl Storage {
    pub async fn open(path: PathBuf) -> Result<(Self, DatabaseRecovery), StorageError> {
        let (sender, receiver) = mpsc::channel(MAILBOX);
        let (ready, started) = oneshot::channel();
        tokio::task::spawn_blocking(move || database::run(&path, receiver, ready));
        let recovery = started.await.map_err(|_| StorageError::Stopped)??;
        Ok((Self { sender }, recovery))
    }

    pub async fn snapshot(&self) -> Result<Snapshot, StorageError> {
        self.request(Command::Read).await
    }

    /// Applies the given changes atomically. Concurrent writes share a SQLite commit.
    pub async fn apply(&self, changes: Vec<Change>) -> Result<(), StorageError> {
        if changes.is_empty() {
            return Ok(());
        }
        self.request(|reply| Command::Write(changes, reply)).await
    }

    /// Flushes outstanding commands and releases the database lock for all handles.
    pub async fn close(self) -> Result<(), StorageError> {
        self.request(Command::Close).await
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, StorageError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(command(reply))
            .await
            .map_err(|_| StorageError::Stopped)?;
        response.await.map_err(|_| StorageError::Stopped)?
    }
}
