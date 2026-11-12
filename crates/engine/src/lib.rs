//! ruscello-engine
//!
//! Orchestrator: queue, scheduler, EngineHandle (commands/events), categorisation and post-processing.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod handle;
mod scheduler;
mod startup;
mod transfer;

pub use handle::{
    AddDownload, EngineError, EngineEvent, EngineHandle, EngineLog, EngineOptions, EngineSnapshot,
    Limits, ProgressUpdate,
};
pub use startup::{RECOVERY_FOLDER, StartupError, recover_queue};

/// Direct HTTP entry point for developer commands and benchmarks.
pub use ruscello_http::{
    ChecksumAlgorithm, DownloadOptions, DownloadProgress, ErrorClass, ExpectedChecksum,
    HttpDownloader, HttpError, Url, WriterError,
};
