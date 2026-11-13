//! ruscello-engine
//!
//! Orchestrator: queue, scheduler, EngineHandle (commands/events), categorisation and post-processing.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod categories;
mod disk;
mod file_types;
mod handle;
mod scheduler;
mod startup;
mod transfer;

pub use handle::{
    AddDownload, EngineError, EngineEvent, EngineHandle, EngineLog, EngineOptions, EngineSnapshot,
    Limits, ProgressUpdate,
};
pub use startup::{RECOVERY_FOLDER, StartupError, recover_queue};

pub use categories::{
    CategoryLimits, CategoryMatch, CategoryPolicy, CategoryRule, CategorySettings,
};
pub use disk::{DiskPolicy, DiskSpaceProbe};
pub use file_types::file_type_hint;
/// Direct HTTP entry point for developer commands and benchmarks.
pub use ruscello_http::{
    ChecksumAlgorithm, DownloadOptions, DownloadProgress, ErrorClass, ExpectedChecksum,
    HttpDownloader, HttpError, Probe, Url, WriterError,
};

/// Probes a candidate URL with the same metadata logic used by downloads.
pub async fn inspect_http(url: Url) -> Result<Probe, HttpError> {
    let client = ruscello_http::client()?;
    ruscello_http::probe(&client, &url).await
}
