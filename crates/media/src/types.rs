use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Media stream or extractor mechanism handling a download.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    /// Apple HLS playlist (`.m3u8`).
    #[default]
    Hls,
    /// MPEG-DASH manifest (`.mpd`).
    Dash,
    /// Extractor-driven site URL or playlist via `yt-dlp`.
    YtDlp,
    /// Direct progressive media file resolved from a stream/extractor.
    Direct,
}

/// Optional external sidecar binaries managed with explicit user consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidecarKind {
    YtDlp,
    Ffmpeg,
}

impl fmt::Display for SidecarKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::YtDlp => f.write_str("yt-dlp"),
            Self::Ffmpeg => f.write_str("ffmpeg"),
        }
    }
}

/// User-configurable sidecar consent, auto-update policy, and custom binary overrides.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SidecarSettings {
    pub ytdlp_consent: bool,
    pub ytdlp_auto_update: bool,
    pub ytdlp_custom_path: Option<PathBuf>,
    pub ffmpeg_consent: bool,
    pub ffmpeg_custom_path: Option<PathBuf>,
    pub last_ytdlp_update_check_unix: Option<u64>,
}

/// Custom release API URLs used in tests and air-gapped environments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarEndpoints {
    pub ytdlp_release_url: String,
    pub ffmpeg_release_url: String,
    pub auto_update_interval_secs: u64,
}

impl Default for SidecarEndpoints {
    fn default() -> Self {
        Self {
            ytdlp_release_url: "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest"
                .to_owned(),
            ffmpeg_release_url: "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest"
                .to_owned(),
            auto_update_interval_secs: 24 * 60 * 60,
        }
    }
}

/// Observed state for one sidecar binary.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SidecarBinaryStatus {
    pub installed: bool,
    pub managed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub consent_granted: bool,
    pub custom_path: Option<PathBuf>,
    pub latest_known_version: Option<String>,
}

/// Combined status snapshot of all optional media sidecars.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SidecarsStatus {
    pub ytdlp: SidecarBinaryStatus,
    pub ffmpeg: SidecarBinaryStatus,
    pub ytdlp_auto_update: bool,
}

/// High-level format presets surfaced in UI and CLI format selectors.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FormatPreset {
    #[default]
    Best,
    MaxHeight(u32),
    AudioOnly {
        mp3: bool,
    },
    FormatId(String),
}

impl FormatPreset {
    /// Parses a selector string such as `"best"`, `"1080p"`, `"audio"`, `"audio-mp3"`, or a raw id.
    pub fn parse(selector: &str) -> Self {
        let trimmed = selector.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("best") {
            return Self::Best;
        }
        if trimmed.eq_ignore_ascii_case("audio-mp3") || trimmed.eq_ignore_ascii_case("mp3") {
            return Self::AudioOnly { mp3: true };
        }
        if trimmed.eq_ignore_ascii_case("audio") || trimmed.eq_ignore_ascii_case("audio-only") {
            return Self::AudioOnly { mp3: false };
        }
        if let Some(height) = trimmed
            .strip_suffix('p')
            .or_else(|| trimmed.strip_suffix('P'))
            .and_then(|digits| digits.parse::<u32>().ok())
        {
            return Self::MaxHeight(height);
        }
        Self::FormatId(trimmed.to_owned())
    }

    /// Canonical wire/persistence identifier for this preset.
    pub fn as_selector(&self) -> String {
        match self {
            Self::Best => "best".to_owned(),
            Self::MaxHeight(height) => format!("{height}p"),
            Self::AudioOnly { mp3: true } => "audio-mp3".to_owned(),
            Self::AudioOnly { mp3: false } => "audio".to_owned(),
            Self::FormatId(id) => id.clone(),
        }
    }
}

/// Selectable format/quality choice returned by media inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFormat {
    pub id: String,
    pub label: String,
    pub preset: FormatPreset,
    pub ext: String,
    pub height: Option<u32>,
    pub width: Option<u32>,
    pub bandwidth_bps: Option<u64>,
    pub filesize: Option<u64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub has_video: bool,
    pub has_audio: bool,
    pub needs_merge: bool,
    pub url: Option<String>,
    pub audio_url: Option<String>,
    pub audio_format_id: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// One item discovered in a media playlist or channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub index: usize,
    pub id: String,
    pub title: String,
    pub url: String,
    pub duration_secs: Option<u64>,
    pub filename: String,
}

/// Inspection summary for an HLS playlist, DASH MPD, or yt-dlp-supported page/playlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaInspection {
    pub source_url: String,
    pub kind: MediaKind,
    pub title: String,
    pub suggested_filename: String,
    pub ffmpeg_available: bool,
    pub formats: Vec<MediaFormat>,
    pub playlist_title: Option<String>,
    pub playlist_items: Vec<PlaylistItem>,
}

/// Persisted selection and metadata for a queued media download.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaRequest {
    pub kind: MediaKind,
    pub format_selector: Option<String>,
    pub title: Option<String>,
    pub headers: Vec<(String, String)>,
    pub variant_uri: Option<String>,
    pub dash_video_id: Option<String>,
    pub dash_audio_id: Option<String>,
}

impl MediaRequest {
    /// Returns whether all fields equal defaults (for compact storage serialization).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Execution phase reported by active media transfers to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPhase {
    Downloading,
    PostProcessing,
}
