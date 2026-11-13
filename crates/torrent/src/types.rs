use std::{
    net::SocketAddr,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

use ruscello_core::{DownloadId, TorrentSource};
use serde::{Deserialize, Serialize};

/// Global configuration for the BitTorrent session managed by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentSettings {
    /// TCP listening port (`0` selects an OS-assigned ephemeral port).
    pub listen_port: u16,
    pub enable_dht: bool,
    pub enable_pex: bool,
    pub enable_upnp: bool,
    pub enable_lsd: bool,
    pub fastresume: bool,
    pub download_limit_bps: Option<NonZeroU64>,
    pub upload_limit_bps: Option<NonZeroU64>,
    /// Stop seeding automatically once ratio reaches `seed_ratio_millis / 1000.0`.
    pub seed_ratio_millis: Option<u32>,
    /// Stop seeding automatically once active seeding time reaches this many minutes.
    pub seed_time_limit_minutes: Option<u64>,
    pub max_peers_per_torrent: Option<NonZeroUsize>,
    /// Optional network interface name to bind all BitTorrent/DHT/tracker traffic to.
    pub bind_interface: Option<String>,
    /// Optional eMule/P2P IP blocklist (`http://`, `https://`, `file://`, or local path).
    pub blocklist_source: Option<String>,
    /// Optional explicit DHT bootstrap nodes (`None` uses built-in defaults).
    #[serde(skip)]
    pub dht_bootstrap_addrs: Option<Vec<String>>,
}

impl Default for TorrentSettings {
    fn default() -> Self {
        Self {
            listen_port: 6881,
            enable_dht: true,
            enable_pex: true,
            enable_upnp: true,
            enable_lsd: true,
            fastresume: true,
            download_limit_bps: None,
            upload_limit_bps: None,
            seed_ratio_millis: None,
            seed_time_limit_minutes: None,
            max_peers_per_torrent: None,
            bind_interface: None,
            blocklist_source: None,
            dht_bootstrap_addrs: None,
        }
    }
}

impl TorrentSettings {
    /// Offline-friendly defaults used for local tests and fallback initialization.
    pub fn offline_ephemeral() -> Self {
        Self {
            listen_port: 0,
            enable_dht: false,
            enable_pex: true,
            enable_upnp: false,
            enable_lsd: false,
            fastresume: true,
            download_limit_bps: None,
            upload_limit_bps: None,
            seed_ratio_millis: None,
            seed_time_limit_minutes: None,
            max_peers_per_torrent: None,
            bind_interface: None,
            blocklist_source: None,
            dht_bootstrap_addrs: Some(Vec::new()),
        }
    }

    /// Returns whether changing from `self` to `other` requires recreating the underlying session.
    pub fn needs_session_restart(&self, other: &Self) -> bool {
        self.listen_port != other.listen_port
            || self.enable_dht != other.enable_dht
            || self.enable_upnp != other.enable_upnp
            || self.enable_lsd != other.enable_lsd
            || self.enable_pex != other.enable_pex
            || self.fastresume != other.fastresume
            || self.max_peers_per_torrent != other.max_peers_per_torrent
            || self.bind_interface != other.bind_interface
            || self.blocklist_source != other.blocklist_source
            || self.dht_bootstrap_addrs != other.dht_bootstrap_addrs
    }

    pub fn seed_ratio_limit(&self) -> Option<f64> {
        self.seed_ratio_millis
            .map(|millis| f64::from(millis) / 1000.0)
    }
}

/// Request to add or resume a managed torrent download.
#[derive(Debug, Clone)]
pub struct AddTorrentRequest {
    pub id: DownloadId,
    pub source: TorrentSource,
    pub destination: PathBuf,
    /// Optional resolved output folder preserved across restarts or user confirmation.
    pub resolved_output_folder: Option<PathBuf>,
    /// Optional cached canonical `.torrent` bytes from pre-add inspection or earlier magnet resolution.
    pub cached_torrent_bytes: Option<Vec<u8>>,
    /// Selected 0-based file indices (`None` downloads all files).
    pub only_files: Option<Vec<usize>>,
    pub paused: bool,
    pub force_recheck: bool,
    pub prior_uploaded_bytes: u64,
    pub prior_seed_elapsed_secs: u64,
    pub seed_ratio_millis: Option<u32>,
    pub seed_time_limit_minutes: Option<u64>,
    pub download_limit_bps: Option<NonZeroU64>,
    pub initial_peers: Vec<SocketAddr>,
}

/// Single file entry returned when inspecting a torrent before adding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentInspectFile {
    pub index: usize,
    pub path: PathBuf,
    pub length: u64,
    pub selected: bool,
}

/// Metadata preview produced for the pre-add dialog (M4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentInspection {
    pub info_hash: String,
    pub name: String,
    pub total_bytes: u64,
    pub selected_bytes: u64,
    pub piece_length: u32,
    pub piece_count: u32,
    pub is_private: bool,
    pub files: Vec<TorrentInspectFile>,
    pub trackers: Vec<String>,
    pub suggested_subfolder: Option<PathBuf>,
    pub torrent_bytes: Option<Vec<u8>>,
}

/// Lifecycle phase reported by the BitTorrent backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentPhase {
    FetchingMetadata,
    Checking {
        checked_bytes: u64,
        total_bytes: u64,
    },
    Downloading,
    Seeding,
    Paused,
    Failed(String),
}

/// Live progress telemetry sampled for a managed torrent.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentProgressSnapshot {
    pub phase: TorrentPhase,
    pub name: Option<String>,
    pub info_hash: Option<String>,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub download_speed_bps: u64,
    pub upload_speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub seed_ratio: f64,
    pub seed_elapsed_secs: u64,
    pub peers_connected: u32,
    pub peers_total: u32,
    pub seeders_connected: u32,
    pub completed_path: Option<PathBuf>,
    pub output_folder: Option<PathBuf>,
    pub only_files: Option<Vec<usize>>,
    pub torrent_bytes: Option<Vec<u8>>,
    pub piece_count: u32,
    pub pieces_have: u32,
    pub seed_goal_reached: bool,
}

/// File selection / priority policy supported by the BitTorrent backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TorrentFilePriority {
    Skip,
    Download,
}

/// Per-file status shown in the GUI Details -> Files tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFileProgress {
    pub index: usize,
    pub path: PathBuf,
    pub length: u64,
    pub downloaded_bytes: u64,
    pub priority: TorrentFilePriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentPeerTransport {
    Tcp,
    Utp,
    Socks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentPeerState {
    Live,
    Connecting,
    Queued,
    Dead,
    NotNeeded,
}

/// Live peer status shown in the GUI Details -> Peers tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentPeerInfo {
    pub address: String,
    pub client: Option<String>,
    pub transport: Option<TorrentPeerTransport>,
    pub state: TorrentPeerState,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed_bps: u64,
    pub upload_speed_bps: u64,
    pub errors: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentTrackerStatus {
    Announcing,
    Standby,
    DisabledPrivate,
}

/// Tracker entry shown in the GUI Details -> Trackers tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentTrackerInfo {
    pub url: String,
    pub protocol: String,
    pub status: TorrentTrackerStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceBucketState {
    Missing,
    Partial,
    Complete,
}

/// Compact piece completion bitmap for a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentPieceMap {
    pub piece_count: u32,
    pub piece_length: u32,
    pub have_count: u32,
    /// Packed MSB-first bits (`(bytes[i / 8] >> (7 - (i % 8))) & 1`).
    pub have_bitmap: Vec<u8>,
}

impl TorrentPieceMap {
    pub fn has_piece(&self, piece_idx: u32) -> bool {
        if piece_idx >= self.piece_count {
            return false;
        }
        let byte_idx = (piece_idx / 8) as usize;
        let bit_shift = 7 - (piece_idx % 8);
        self.have_bitmap
            .get(byte_idx)
            .is_some_and(|byte| (byte >> bit_shift) & 1 == 1)
    }

    /// Downsamples the piece map into `buckets.min(piece_count)` buckets for fast rendering.
    pub fn buckets(&self, max_buckets: usize) -> Vec<PieceBucketState> {
        if self.piece_count == 0 || max_buckets == 0 {
            return Vec::new();
        }
        let total = self.piece_count as usize;
        let count = max_buckets.min(total);
        let mut out = Vec::with_capacity(count);
        for bucket in 0..count {
            let start = bucket * total / count;
            let end = ((bucket + 1) * total / count).max(start + 1);
            let mut have = 0usize;
            for piece in start..end {
                if self.has_piece(piece as u32) {
                    have += 1;
                }
            }
            let len = end - start;
            let state = if have == 0 {
                PieceBucketState::Missing
            } else if have == len {
                PieceBucketState::Complete
            } else {
                PieceBucketState::Partial
            };
            out.push(state);
        }
        out
    }
}

/// Detailed live state for the selected torrent in the Details panel.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentDetailsSnapshot {
    pub info_hash: String,
    pub is_private: bool,
    pub seed_ratio: f64,
    pub seed_elapsed_secs: u64,
    pub uploaded_bytes: u64,
    pub download_speed_bps: u64,
    pub upload_speed_bps: u64,
    pub piece_map: Option<TorrentPieceMap>,
    pub files: Vec<TorrentFileProgress>,
    pub peers: Vec<TorrentPeerInfo>,
    pub trackers: Vec<TorrentTrackerInfo>,
}

/// Session-wide BitTorrent telemetry (global upload/download speeds & DHT nodes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TorrentSessionTelemetry {
    pub download_speed_bps: u64,
    pub upload_speed_bps: u64,
    pub dht_nodes: Option<usize>,
    pub listen_port: Option<u16>,
}

/// Helper to compute share ratio (`uploaded / downloaded_base`).
pub fn compute_ratio(uploaded_bytes: u64, base_bytes: u64) -> f64 {
    if base_bytes == 0 {
        0.0
    } else {
        uploaded_bytes as f64 / base_bytes as f64
    }
}
