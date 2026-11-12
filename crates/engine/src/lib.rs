//! ruscello-engine
//!
//! Orchestrator: queue, scheduler, EngineHandle (commands/events), categorisation and post-processing.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod startup;

pub use startup::{RECOVERY_FOLDER, StartupError, recover_queue};

/// Headless HTTP entry point while the queue and engine handle are being built.
pub use ruscello_http::{
    ChecksumAlgorithm, DownloadOptions, DownloadProgress, ErrorClass, ExpectedChecksum,
    HttpDownloader, HttpError, Url, WriterError,
};
