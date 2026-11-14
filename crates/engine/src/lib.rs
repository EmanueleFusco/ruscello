//! ruscello-engine
//!
//! Orchestrator: queue, scheduler, EngineHandle (commands/events), categorisation and post-processing.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod archive;
mod automation;
mod categories;
mod checksum_verify;
mod disk;
mod file_types;
mod handle;
mod media_bridge;
mod post_process;
mod resolution;
mod scheduler;
mod startup;
mod torrent_bridge;
mod transfer;
mod watch;

pub use automation::{
    AutomationSettings, Clock, OutsideHoursAction, PostProcessingSettings, ScheduleAction,
    ScheduleDecision, ScheduleRule, ScheduleSettings, SystemClock, WatchFolderRule,
    WatchFolderSettings, WatchProcessedBehavior, WeekdayMask, format_minutes, parse_hhmm,
};
pub use handle::{
    AddDownload, EngineError, EngineEvent, EngineHandle, EngineLog, EngineOptions, EngineSnapshot,
    Limits, ProgressUpdate, TorrentRuntimeProgress,
};
pub use post_process::PostProcessStage;
pub use startup::{RECOVERY_FOLDER, StartupError, recover_queue};

pub use categories::{
    CategoryLimits, CategoryMatch, CategoryPolicy, CategoryRule, CategorySettings,
};
pub use disk::{DiskPolicy, DiskSpaceProbe};
pub use file_types::file_type_hint;
pub use resolution::{ResolvedDownload, prepare_http_download, resolve_download};
/// Direct HTTP entry point for developer commands and benchmarks.
pub use ruscello_http::{
    ChecksumAlgorithm, ChecksumError, DownloadOptions, DownloadProgress, ErrorClass,
    ExpectedChecksum, FtpCredentials, HttpDownloader, HttpError, Probe, SegmentProgress, Url,
    WriterError, sanitize_ftp_url,
};
pub use ruscello_media::{
    BandwidthContext, FormatPreset, HeaderMap, MediaEngine, MediaError, MediaFormat,
    MediaInspection, MediaKind, MediaPhase, MediaRequest, MediaTransferJob, PlaylistItem,
    SidecarBinaryStatus, SidecarEndpoints, SidecarKind, SidecarSettings, SidecarsStatus,
    is_dash_url, is_hls_url, is_manifest_url,
};
pub use ruscello_platform::power::{
    NoopSleepInhibitor, PowerController, PowerError, QueueCompletionAction, SleepInhibitor,
    SystemPowerController, SystemSleepInhibitor,
};
pub use ruscello_resolver::{
    BrowserFailure, BrowserLocale, BrowserOptions, BrowserSession, Builtin, Candidate,
    CandidateGroup, ContainerFile, DlcKeyService, HosterAccount, HosterCredentials, ImportedLink,
    ImportedPackage, InstalledBrowser, KeyringAccounts, PageAnalysis, ResolveError, Resolver,
    ScoreReason, import_dlc, links,
};
pub use ruscello_torrent::{
    PieceBucketState, TorrentDetailsSnapshot, TorrentError, TorrentFilePriority,
    TorrentFileProgress, TorrentInspectFile, TorrentInspection, TorrentPeerInfo, TorrentPeerState,
    TorrentPeerTransport, TorrentPhase, TorrentPieceMap, TorrentProgressSnapshot,
    TorrentSessionTelemetry, TorrentSettings, TorrentTrackerInfo, TorrentTrackerStatus,
    inspect_torrent_bytes,
};

/// Probes a candidate URL with the same metadata logic used by downloads.
pub async fn inspect_http(url: Url) -> Result<Probe, HttpError> {
    let client = ruscello_http::client()?;
    ruscello_http::probe(&client, &url).await
}

/// Parses a checksum as an algorithm followed by a colon and hexadecimal digest.
pub fn parse_checksum(input: &str) -> Result<ExpectedChecksum, ChecksumError> {
    let (algorithm, digest) = input.split_once(':').ok_or(ChecksumError::Algorithm)?;
    ExpectedChecksum::from_hex(algorithm.trim().parse()?, digest.trim())
}
