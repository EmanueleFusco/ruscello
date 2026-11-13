use std::{
    io,
    net::SocketAddr,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::Arc,
};

use ruscello_core::{
    CategoryId, Download, DownloadId, FailureReason, Package, PackageId, Priority, Source, Status,
    Tag, TorrentSource,
};
use ruscello_storage::{DatabaseRecovery, Storage, StorageError};
use ruscello_torrent::{
    TorrentDetailsSnapshot, TorrentError, TorrentInspection, TorrentManager,
    TorrentSessionTelemetry, TorrentSettings,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    CategorySettings, DiskPolicy, DownloadProgress, HttpError, StartupError,
    categories::CategoryBook, recover_queue, scheduler::Scheduler,
};

pub(super) const LIMITS_KEY: &str = "scheduler_limits";
pub(super) const TORRENT_SETTINGS_KEY: &str = "torrent_settings";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub concurrent_downloads: NonZeroUsize,
    pub connections_per_host: NonZeroUsize,
    pub connections_per_download: NonZeroUsize,
    pub bandwidth: Option<NonZeroU64>,
    pub retries: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            concurrent_downloads: NonZeroUsize::MIN.saturating_add(2),
            connections_per_host: NonZeroUsize::MIN.saturating_add(7),
            connections_per_download: NonZeroUsize::MIN.saturating_add(3),
            bandwidth: None,
            retries: 8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub database: PathBuf,
    pub destination: PathBuf,
    pub recovery_directories: Vec<PathBuf>,
    /// Uses a separate working volume for new downloads when configured.
    pub temporary_directory: Option<PathBuf>,
    pub disk: DiskPolicy,
    /// Initial limits for a database without saved limits.
    pub limits: Limits,
    /// Root directory for BitTorrent session persistence and cached metadata.
    pub data_dir: Option<PathBuf>,
    /// Initial BitTorrent session settings when not yet persisted in storage.
    pub torrent: TorrentSettings,
    /// Whether to initialize the BitTorrent session eagerly on startup.
    pub eager_torrent_session: bool,
}

impl EngineOptions {
    pub fn new(database: PathBuf, destination: PathBuf) -> Self {
        Self {
            database,
            destination,
            recovery_directories: Vec::new(),
            temporary_directory: None,
            disk: DiskPolicy::default(),
            limits: Limits::default(),
            data_dir: None,
            torrent: TorrentSettings::offline_ephemeral(),
            eager_torrent_session: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AddDownload {
    pub source: Source,
    pub destination: Option<PathBuf>,
    pub package: Option<PackageId>,
    pub category: Option<CategoryId>,
    /// A filename supplied by a resolver or the add dialog for rule matching.
    pub filename_hint: Option<String>,
    pub tags: Vec<Tag>,
    pub priority: Priority,
    pub start_immediately: bool,
    pub max_connections: Option<NonZeroUsize>,
    pub bandwidth: Option<NonZeroU64>,
    pub checksum: Option<String>,
    /// Selected 0-based file indices for BitTorrent downloads (`None` downloads all files).
    pub only_files: Option<Vec<usize>>,
    /// Pre-fetched canonical `.torrent` bytes from pre-add inspection.
    pub cached_torrent_bytes: Option<Vec<u8>>,
    /// Explicit resolved output folder preserved across resume/recheck.
    pub resolved_output_folder: Option<PathBuf>,
    /// Optional per-torrent seed ratio override (`millis / 1000.0`).
    pub seed_ratio_millis: Option<u32>,
    /// Optional per-torrent seeding time limit in minutes.
    pub seed_time_limit_minutes: Option<u64>,
    /// Optional initial peer addresses (primarily for direct peers or tests).
    pub initial_peers: Vec<SocketAddr>,
}

impl AddDownload {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            destination: None,
            package: None,
            category: None,
            filename_hint: None,
            tags: Vec::new(),
            priority: Priority::default(),
            start_immediately: true,
            max_connections: None,
            bandwidth: None,
            checksum: None,
            only_files: None,
            cached_torrent_bytes: None,
            resolved_output_folder: None,
            seed_ratio_millis: None,
            seed_time_limit_minutes: None,
            initial_peers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    pub downloads: Vec<Download>,
    pub packages: Vec<Package>,
    pub limits: Limits,
    pub recovery: DatabaseRecovery,
    pub categories: CategorySettings,
    pub torrent: TorrentSettings,
    pub telemetry: TorrentSessionTelemetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressUpdate {
    pub id: DownloadId,
    pub progress: DownloadProgress,
}

/// Runtime upload/peer/seed telemetry reported per active BitTorrent download.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TorrentRuntimeProgress {
    pub id: DownloadId,
    pub download_rate_bps: u64,
    pub upload_rate_bps: u64,
    pub uploaded_bytes: u64,
    pub seed_ratio: f64,
    pub seed_elapsed_secs: u64,
    pub peers_connected: u32,
    pub peers_total: u32,
    pub seeders_connected: u32,
    pub eta_secs: Option<u64>,
    pub piece_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineLog {
    DownloadFailed(FailureReason),
    DatabaseWriteFailed,
    CleanupFailed,
    DiskSpaceLow,
    TorrentSessionInitFailed,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Added(Box<Download>),
    Removed(DownloadId),
    Progress(Vec<ProgressUpdate>),
    TorrentProgress(Vec<TorrentRuntimeProgress>),
    TorrentTelemetry(TorrentSessionTelemetry),
    StatusChanged {
        id: DownloadId,
        status: Status,
    },
    QueueChanged,
    Log {
        id: Option<DownloadId>,
        message: EngineLog,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("the download engine has stopped")]
    Stopped,
    #[error("download ID does not exist")]
    UnknownDownload,
    #[error("another operation is stopping this download")]
    DownloadBusy,
    #[error("category ID does not exist")]
    UnknownCategory,
    #[error("category names and IDs must be valid and IDs must be unique")]
    InvalidCategory,
    #[error("category rule {0} is invalid")]
    InvalidCategoryRule(usize),
    #[error("this source is not supported by the download engine")]
    UnsupportedSource,
    #[error("the HTTP source is not a valid URL")]
    InvalidUrl,
    #[error("the expected checksum is invalid")]
    InvalidChecksum(#[from] crate::ChecksumError),
    #[error("the engine could not allocate a download ID")]
    Identifier,
    #[error("a download file operation failed")]
    File(#[from] io::Error),
    #[error("saved engine settings are invalid")]
    Settings(#[from] serde_json::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Startup(#[from] StartupError),
    #[error(transparent)]
    Transition(#[from] ruscello_core::InvalidTransition),
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Torrent(#[from] TorrentError),
    #[error("a file operation stopped unexpectedly")]
    Worker(#[from] tokio::task::JoinError),
}

pub(super) type Reply<T> = oneshot::Sender<Result<T, EngineError>>;

pub(super) enum Command {
    Add(Vec<AddDownload>, Option<String>, Reply<Vec<DownloadId>>),
    Update(Update, Reply<()>),
    Snapshot(Reply<EngineSnapshot>),
    TorrentManager(Reply<Arc<TorrentManager>>),
    Shutdown(Reply<()>),
}

pub(super) enum Update {
    Pause(DownloadId),
    Resume(DownloadId),
    ForceStart(DownloadId),
    ForceRecheck(DownloadId),
    SetTorrentFiles(DownloadId, Vec<usize>),
    Cancel(DownloadId, bool),
    Priority(DownloadId, Priority),
    Move(DownloadId, Option<DownloadId>),
    Limits(Limits),
    TemporaryBandwidth(Option<NonZeroU64>),
    Categories(CategorySettings),
    TorrentSettings(TorrentSettings),
}

/// Asynchronous queue control through a bounded mailbox and broadcast events.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<EngineEvent>,
}

impl EngineHandle {
    pub async fn start(mut options: EngineOptions) -> Result<Self, EngineError> {
        options.destination = std::path::absolute(&options.destination)?;
        options.temporary_directory = options
            .temporary_directory
            .as_deref()
            .map(std::path::absolute)
            .transpose()?;
        let (storage, recovery) = Storage::open(options.database.clone()).await?;
        options
            .recovery_directories
            .push(options.destination.clone());
        options
            .recovery_directories
            .extend(options.temporary_directory.clone());
        let mut directories = std::mem::take(&mut options.recovery_directories);
        for directory in &mut directories {
            *directory = std::path::absolute(&directory)?;
        }
        let snapshot = recover_queue(&storage, recovery, directories).await?;
        options.limits = snapshot
            .settings
            .get(LIMITS_KEY)
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or(options.limits);
        options.torrent = snapshot
            .settings
            .get(TORRENT_SETTINGS_KEY)
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or(options.torrent);
        let categories = CategoryBook::load(&storage, &snapshot).await?;
        let (commands, receiver) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let scheduler = Scheduler::new(
            storage,
            snapshot,
            recovery,
            options,
            categories,
            events.clone(),
        )
        .await?;
        tokio::spawn(Box::pin(scheduler.run(receiver)));
        Ok(Self { commands, events })
    }

    /// After lagging, obtain a snapshot before using further incremental events.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    pub async fn snapshot(&self) -> Result<EngineSnapshot, EngineError> {
        self.request(Command::Snapshot).await
    }

    pub async fn add(&self, request: AddDownload) -> Result<DownloadId, EngineError> {
        self.add_many(vec![request])
            .await?
            .into_iter()
            .next()
            .ok_or(EngineError::Stopped)
    }

    pub async fn add_many(
        &self,
        requests: Vec<AddDownload>,
    ) -> Result<Vec<DownloadId>, EngineError> {
        self.request(|reply| Command::Add(requests, None, reply))
            .await
    }

    /// Adds a named group and its downloads in the same queue transaction.
    pub async fn add_package(
        &self,
        name: String,
        requests: Vec<AddDownload>,
    ) -> Result<Vec<DownloadId>, EngineError> {
        self.request(|reply| Command::Add(requests, Some(name), reply))
            .await
    }

    pub async fn pause(&self, id: DownloadId) -> Result<(), EngineError> {
        self.update(Update::Pause(id)).await
    }

    pub async fn resume(&self, id: DownloadId) -> Result<(), EngineError> {
        self.update(Update::Resume(id)).await
    }

    /// Promotes a download to the front of the queue while respecting resource limits.
    pub async fn force_start(&self, id: DownloadId) -> Result<(), EngineError> {
        self.update(Update::ForceStart(id)).await
    }

    pub async fn cancel(&self, id: DownloadId, delete_files: bool) -> Result<(), EngineError> {
        self.update(Update::Cancel(id, delete_files)).await
    }

    pub async fn set_priority(
        &self,
        id: DownloadId,
        priority: Priority,
    ) -> Result<(), EngineError> {
        self.update(Update::Priority(id, priority)).await
    }

    /// Moves a download before another entry, or to the end of the queue.
    pub async fn move_before(
        &self,
        id: DownloadId,
        before: Option<DownloadId>,
    ) -> Result<(), EngineError> {
        self.update(Update::Move(id, before)).await
    }

    pub async fn set_limits(&self, limits: Limits) -> Result<(), EngineError> {
        self.update(Update::Limits(limits)).await
    }

    /// Overrides the effective global bandwidth limit for this engine session.
    pub async fn set_temporary_bandwidth(
        &self,
        rate: Option<NonZeroU64>,
    ) -> Result<(), EngineError> {
        self.update(Update::TemporaryBandwidth(rate)).await
    }

    pub async fn set_categories(&self, settings: CategorySettings) -> Result<(), EngineError> {
        self.update(Update::Categories(settings)).await
    }

    /// Triggers a full SHA-1 piece recheck for a torrent download (clears fastresume bitmap).
    pub async fn force_recheck(&self, id: DownloadId) -> Result<(), EngineError> {
        self.update(Update::ForceRecheck(id)).await
    }

    /// Updates the included 0-based file indices (`Download` vs `Skip`) for a torrent download.
    pub async fn set_torrent_files(
        &self,
        id: DownloadId,
        only_files: Vec<usize>,
    ) -> Result<(), EngineError> {
        self.update(Update::SetTorrentFiles(id, only_files)).await
    }

    /// Persists and applies global BitTorrent session settings (ports, DHT, limits, interface, blocklist).
    pub async fn set_torrent_settings(&self, settings: TorrentSettings) -> Result<(), EngineError> {
        self.update(Update::TorrentSettings(settings)).await
    }

    /// Resolves metadata for a `.torrent` file, `.torrent` URL, or magnet URI before adding.
    pub async fn inspect_torrent(
        &self,
        source: TorrentSource,
        initial_peers: Vec<SocketAddr>,
        cancellation: CancellationToken,
    ) -> Result<TorrentInspection, EngineError> {
        let manager = self.request(Command::TorrentManager).await?;
        Ok(manager
            .inspect(&source, initial_peers, &cancellation)
            .await?)
    }

    /// Returns detailed live state (pieces, files, peers, trackers) for a managed torrent, or `None` if not active.
    pub async fn torrent_details(
        &self,
        id: DownloadId,
    ) -> Result<Option<TorrentDetailsSnapshot>, EngineError> {
        let manager = self.request(Command::TorrentManager).await?;
        match manager.details(id).await {
            Ok(details) => Ok(Some(details)),
            Err(TorrentError::NotFound | TorrentError::NotResolved) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Queries aggregate BitTorrent session telemetry (global rates, DHT nodes, listen port).
    pub async fn torrent_telemetry(&self) -> Result<TorrentSessionTelemetry, EngineError> {
        let manager = self.request(Command::TorrentManager).await?;
        Ok(manager.session_telemetry().await)
    }

    /// Packages and begins seeding a local file or folder in the engine's BitTorrent session.
    pub async fn create_and_serve_local_torrent(
        &self,
        source_path: &Path,
        trackers: Vec<String>,
        piece_length: Option<u32>,
    ) -> Result<(Vec<u8>, String, SocketAddr), EngineError> {
        let manager = self.request(Command::TorrentManager).await?;
        Ok(manager
            .create_and_serve_local(source_path, trackers, piece_length)
            .await?)
    }

    /// Stops transfers after saving their checkpoints and releases the queue database.
    pub async fn shutdown(&self) -> Result<(), EngineError> {
        self.request(Command::Shutdown).await
    }

    async fn update(&self, update: Update) -> Result<(), EngineError> {
        self.request(|reply| Command::Update(update, reply)).await
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, EngineError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| EngineError::Stopped)?;
        response.await.map_err(|_| EngineError::Stopped)?
    }
}
