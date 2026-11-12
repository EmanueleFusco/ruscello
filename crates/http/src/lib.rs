//! ruscello-http
//!
//! Segmented, resumable, crash-safe HTTP/HTTPS (and later FTP) download engine: probe, segment allocator with work stealing, positional writer, control file, retries, rate limiting, mirrors.
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod checksum;
mod download;
mod filename;
mod limit;
mod probe;
mod retry;
mod segments;
mod transport;
mod writer;

pub use checksum::{ChecksumAlgorithm, ChecksumError, ExpectedChecksum};
pub use download::{DownloadOptions, DownloadProgress, HttpDownloader};
pub use limit::BandwidthLimit;
pub use probe::{HttpError, Probe, Validators, client, probe};
pub use reqwest::Url;
pub use retry::{Connectivity, ErrorClass, ExpiredLinkResolver, ResolvedLink, RetryPolicy};
pub use segments::{
    ConnectionController, DEFAULT_MIN_SEGMENT_SIZE, SegmentAllocator, SegmentError, Work, WorkId,
};
pub use transport::HostConnections;
pub use writer::{
    Checkpoint, CheckpointJob, FileWriter, WriterError, filename_candidate, persisted_bytes,
    sanitize_filename, scan_checkpoints,
};
