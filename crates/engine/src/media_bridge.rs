use ruscello_core::Source;
use ruscello_http::Url;
use ruscello_media::{
    FormatPreset, HeaderMap, MediaKind, MediaRequest, SidecarSettings, is_dash_url, is_hls_url,
};
use ruscello_storage::{MediaQueueState, Snapshot};
use tokio_util::sync::CancellationToken;

use crate::EngineError;

pub(super) const SIDECAR_SETTINGS_KEY: &str = "sidecar_settings";

/// Parses and validates an HTTP(S) media source URL.
pub(super) fn parse_media_url(raw: &str) -> Result<Url, EngineError> {
    let mut url = Url::parse(raw.trim()).map_err(|_| EngineError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(EngineError::InvalidUrl);
    }
    url.set_fragment(None);
    Ok(url)
}

/// Extracts and validates the URL inside a `Source::Media`.
pub(super) fn media_source_url(source: &Source) -> Result<Url, EngineError> {
    let Source::Media { url } = source else {
        return Err(EngineError::UnsupportedSource);
    };
    parse_media_url(url)
}

/// Converts a high-level `MediaRequest` into decoupled persistent `MediaQueueState`.
pub(super) fn to_queue_state(request: &MediaRequest) -> MediaQueueState {
    MediaQueueState {
        kind: kind_to_str(request.kind).to_owned(),
        format_selector: request.format_selector.clone(),
        title: request.title.clone(),
        headers: request.headers.clone(),
        variant_uri: request.variant_uri.clone(),
        dash_video_id: request.dash_video_id.clone(),
        dash_audio_id: request.dash_audio_id.clone(),
    }
}

/// Restores a `MediaRequest` from persisted queue metadata or defaults based on the URL.
pub(super) fn from_queue_state(state: Option<&MediaQueueState>, url: &Url) -> MediaRequest {
    match state {
        Some(saved) => MediaRequest {
            kind: str_to_kind(&saved.kind, url),
            format_selector: saved.format_selector.clone(),
            title: saved.title.clone(),
            headers: saved.headers.clone(),
            variant_uri: saved.variant_uri.clone(),
            dash_video_id: saved.dash_video_id.clone(),
            dash_audio_id: saved.dash_audio_id.clone(),
        },
        None => MediaRequest {
            kind: infer_kind_from_url(url),
            ..MediaRequest::default()
        },
    }
}

/// Loads persisted `SidecarSettings` from the snapshot or falls back to initial engine options.
pub(super) fn load_sidecar_settings(
    snapshot: &Snapshot,
    fallback: SidecarSettings,
) -> Result<SidecarSettings, serde_json::Error> {
    match snapshot.settings.get(SIDECAR_SETTINGS_KEY) {
        Some(value) => serde_json::from_value(value.clone()),
        None => Ok(fallback),
    }
}

/// Fetches browser session headers/cookies from `Resolver` without invoking hoster scrapers.
pub(super) async fn session_headers(
    resolver: &crate::Resolver,
    url: &Url,
    cancellation: &CancellationToken,
) -> HeaderMap {
    if resolver.builtin(url).is_some() {
        return HeaderMap::new();
    }
    resolver
        .resolve(url, cancellation)
        .await
        .ok()
        .flatten()
        .map(|resolution| resolution.headers)
        .unwrap_or_default()
}

/// Returns whether a media format selector targets audio-only output.
pub(super) fn is_audio_selector(selector: Option<&str>) -> bool {
    selector.is_some_and(|raw| matches!(FormatPreset::parse(raw), FormatPreset::AudioOnly { .. }))
}

fn kind_to_str(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Hls => "hls",
        MediaKind::Dash => "dash",
        MediaKind::YtDlp => "yt-dlp",
        MediaKind::Direct => "direct",
    }
}

fn str_to_kind(value: &str, url: &Url) -> MediaKind {
    match value.trim().to_ascii_lowercase().as_str() {
        "hls" => MediaKind::Hls,
        "dash" => MediaKind::Dash,
        "yt-dlp" | "ytdlp" => MediaKind::YtDlp,
        "direct" => MediaKind::Direct,
        _ => infer_kind_from_url(url),
    }
}

fn infer_kind_from_url(url: &Url) -> MediaKind {
    if is_hls_url(url) {
        MediaKind::Hls
    } else if is_dash_url(url) {
        MediaKind::Dash
    } else {
        MediaKind::YtDlp
    }
}
