//! ruscello-torrent
//!
//! BitTorrent support: thin wrapper around `librqbit` exposing Ruscello domain types (see ADR-0005).
//!
//! See `docs/ARCHITECTURE.md` and `docs/PLAN.md` for responsibilities and milestones.

mod error;
mod manager;
mod types;

pub use error::TorrentError;
pub use manager::{StartedTorrent, TorrentManager, inspect_torrent_bytes};
pub use types::{
    AddTorrentRequest, PieceBucketState, TorrentDetailsSnapshot, TorrentFilePriority,
    TorrentFileProgress, TorrentInspectFile, TorrentInspection, TorrentPeerInfo, TorrentPeerState,
    TorrentPeerTransport, TorrentPhase, TorrentPieceMap, TorrentProgressSnapshot,
    TorrentSessionTelemetry, TorrentSettings, TorrentTrackerInfo, TorrentTrackerStatus,
    compute_ratio,
};
