use std::{net::SocketAddr, num::NonZeroU64, path::PathBuf};

use ruscello_core::{Download, FailureReason, FileType, Source, Status, TorrentSource};
use ruscello_engine::Candidate;
use serde::{Deserialize, Serialize};

/// Health and session status returned by `GET /api/v1/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
    pub locale: String,
    pub auth_required: bool,
    pub authenticated: bool,
}

/// Live serialized download entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadDto {
    pub id: u64,
    pub package_id: Option<u64>,
    pub filename: String,
    pub source_summary: String,
    pub file_type: String,
    pub status: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub progress_percent: f64,
    pub download_bps: u64,
    pub upload_bps: u64,
    pub eta_secs: Option<u64>,
    pub failure_reason: Option<String>,
}

/// Live serialized package entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDto {
    pub id: u64,
    pub name: String,
}

/// Live queue snapshot including telemetry rates and speed limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueSnapshotDto {
    pub downloads: Vec<DownloadDto>,
    pub packages: Vec<PackageDto>,
    pub total_download_bps: u64,
    pub total_upload_bps: u64,
    pub alternate_speed_enabled: bool,
    pub alternate_bandwidth_bps: Option<u64>,
    pub global_bandwidth_bps: Option<u64>,
}

/// Real-time envelope emitted over WebSocket (`/api/v1/ws`) and SSE (`/api/v1/events`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StreamEnvelope {
    Snapshot(QueueSnapshotDto),
}

/// Request payload for `POST /api/v1/auth/login`.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

/// Response returned by `POST /api/v1/auth/login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub ok: bool,
    pub token: String,
}

/// Request payload for `POST /api/v1/downloads`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddDownloadsRequest {
    #[serde(default)]
    pub urls: Vec<String>,
    pub package_name: Option<String>,
    pub destination: Option<PathBuf>,
    pub start_immediately: Option<bool>,
    pub cookies: Option<Vec<String>>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub filename_hint: Option<String>,
    pub torrent_base64: Option<String>,
    pub torrent_filename: Option<String>,
    pub dlc_content: Option<String>,
    pub dlc_filename: Option<String>,
    pub open_in_ui: Option<bool>,
}

/// Response returned by `POST /api/v1/downloads`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddDownloadsResponse {
    pub added_ids: Vec<u64>,
    pub count: usize,
}

/// Query parameters accepted by `DELETE /api/v1/downloads/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    pub(crate) delete_files: bool,
}

/// Response returned by bulk pause/resume routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkActionResponse {
    pub affected: usize,
}

/// Request payload for `POST /api/v1/speed/alternate`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AltSpeedRequest {
    pub enabled: Option<bool>,
}

/// Response returned by `POST /api/v1/speed/alternate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AltSpeedResponse {
    pub enabled: bool,
    pub bandwidth_bps: Option<u64>,
}

/// Request payload for `POST /api/v1/cookies`.
#[derive(Debug, Clone, Deserialize)]
pub struct ForwardCookiesRequest {
    pub url: String,
    #[serde(default)]
    pub cookies: Vec<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
}

/// Response returned by `POST /api/v1/cookies`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardCookiesResponse {
    pub stored: usize,
}

/// Request payload for `POST /api/v1/linkgrabber/analyze`.
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyzeLinkRequest {
    pub url: String,
    pub cookies: Option<Vec<String>>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub open_in_ui: Option<bool>,
}

/// Candidate item returned by `POST /api/v1/linkgrabber/analyze`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzedCandidateDto {
    pub url: String,
    pub filename: String,
    pub size: Option<u64>,
    pub score: i16,
}

/// Response returned by `POST /api/v1/linkgrabber/analyze`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeLinkResponse {
    pub title: Option<String>,
    pub source_url: String,
    pub candidates: Vec<AnalyzedCandidateDto>,
}

/// Request body for `POST /api/v1/pair/request`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PairingRequestBody {
    pub client_name: Option<String>,
    pub pin: Option<String>,
}

/// Response returned by `POST /api/v1/pair/request` and `GET /api/v1/pair/status/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingResponse {
    pub status: String,
    pub request_id: Option<String>,
    pub token: Option<String>,
    pub local_port: u16,
}

/// Live LAN listener status reported to the desktop GUI.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum LanStatus {
    #[default]
    Disabled,
    Active {
        bind_addr: SocketAddr,
        display_url: String,
    },
    Error(String),
}

/// Pending extension pairing request awaiting 1-click user approval in desktop Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPairingRequest {
    pub request_id: String,
    pub client_name: String,
    pub origin: String,
}

/// Current pairing summary displayed in desktop Settings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PairingOverview {
    pub window_active: bool,
    pub remaining_secs: u64,
    pub pin: Option<String>,
    pub pending: Vec<PendingPairingRequest>,
    pub paired_count: usize,
}

/// Events forwarded from the local API server to an interactive host (desktop GUI).
#[derive(Debug, Clone)]
pub enum ApiEvent {
    LanStatusChanged(LanStatus),
    PairingChanged(PairingOverview),
    AnalyzePage {
        url: String,
        cookies: Vec<String>,
        user_agent: Option<String>,
        referer: Option<String>,
    },
    QueueRequested {
        urls: Vec<String>,
    },
    AlternateSpeedChanged(Option<NonZeroU64>),
}

pub(crate) fn status_name(status: Status) -> &'static str {
    match status {
        Status::Queued => "Queued",
        Status::Resolving => "Resolving",
        Status::WaitingUser => "WaitingUser",
        Status::Downloading => "Downloading",
        Status::Paused => "Paused",
        Status::Seeding => "Seeding",
        Status::PostProcessing => "PostProcessing",
        Status::Completed => "Completed",
        Status::Failed { .. } => "Failed",
    }
}

pub(crate) fn failure_name(reason: FailureReason) -> &'static str {
    match reason {
        FailureReason::Network => "Network",
        FailureReason::AccessDenied => "AccessDenied",
        FailureReason::SourceUnavailable => "SourceUnavailable",
        FailureReason::UnsupportedSource => "UnsupportedSource",
        FailureReason::IntegrityCheck => "IntegrityCheck",
        FailureReason::FileSystem => "FileSystem",
        FailureReason::Internal => "Internal",
    }
}

pub(crate) fn file_type_name(ft: FileType) -> &'static str {
    match ft {
        FileType::Video => "Video",
        FileType::Audio => "Audio",
        FileType::Archives => "Archives",
        FileType::Programs => "Programs",
        FileType::Documents => "Documents",
        FileType::Images => "Images",
        FileType::Torrents => "Torrents",
        FileType::Other => "Other",
    }
}

pub(crate) fn source_summary(source: &Source) -> String {
    match source {
        Source::Http { url } | Source::Hoster { url } | Source::Media { url } => url.clone(),
        Source::Torrent(TorrentSource::Magnet(uri)) => uri.clone(),
        Source::Torrent(TorrentSource::Url(url)) => url.clone(),
        Source::Torrent(TorrentSource::File(path)) => path.to_string_lossy().into_owned(),
    }
}

pub(crate) fn display_name(download: &Download) -> String {
    if let Some(name) = download.filename.as_deref().filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    if let Source::Torrent(TorrentSource::File(path)) = &download.source
        && let Some(name) = path.file_name()
    {
        return name.to_string_lossy().into_owned();
    }
    let summary = source_summary(&download.source);
    if let Ok(url) = ruscello_engine::Url::parse(&summary) {
        if url.scheme() == "magnet"
            && let Some((_, name)) = url.query_pairs().find(|(k, _)| k == "dn")
        {
            return name.into_owned();
        }
        if let Some(seg) = url
            .path_segments()
            .and_then(|mut parts| parts.rfind(|part| !part.is_empty()))
            .or_else(|| url.host_str())
        {
            return seg.to_owned();
        }
    }
    if !summary.is_empty() {
        summary
    } else {
        format!("#{}", download.id.0)
    }
}

pub(crate) fn can_pause(status: Status) -> bool {
    matches!(
        status,
        Status::Queued
            | Status::Resolving
            | Status::WaitingUser
            | Status::Downloading
            | Status::Seeding
    )
}

pub(crate) fn can_resume(status: Status) -> bool {
    matches!(status, Status::Paused | Status::Failed { .. })
}

pub(crate) fn map_candidate(candidate: Candidate) -> AnalyzedCandidateDto {
    AnalyzedCandidateDto {
        url: candidate.url.to_string(),
        filename: candidate.filename,
        size: candidate.size,
        score: candidate.score,
    }
}
