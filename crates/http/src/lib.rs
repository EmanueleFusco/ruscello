//! ruscello-http
//!
//! Segmented, resumable, crash-safe HTTP/HTTPS (and later FTP) download engine: probe, segment allocator with work stealing, positional writer, control file, retries, rate limiting, mirrors.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod filename;
mod probe;
mod segments;
mod writer;

pub use probe::{HttpError, Probe, Validators, client, probe};
pub use segments::{
    ConnectionController, DEFAULT_MIN_SEGMENT_SIZE, SegmentAllocator, SegmentError, Work, WorkId,
};
pub use writer::{FileWriter, WriterError, sanitize_filename};
