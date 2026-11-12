//! ruscello-core
//!
//! Domain types, download state machine, events and shared errors. No I/O.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod download;
mod status;

pub use download::{
    Category, CategoryId, DefaultCategory, Download, DownloadId, FileType, Package, PackageId,
    Priority, Source, SourceKind, Tag, TorrentSource,
};
pub use status::{FailureReason, InvalidTransition, Status};
