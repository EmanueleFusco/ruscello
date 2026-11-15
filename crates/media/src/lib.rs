//! ruscello-media
//!
//! Streaming and extractor media downloads: native HLS (`m3u8-rs`) and MPEG-DASH (`quick-xml`),
//! AES-128 segment decryption, segment-level resume, explicit-consent optional sidecar management
//! (`yt-dlp` and `ffmpeg` with SHA-256 verification), `yt-dlp -J` inspection & fallback execution,
//! and `ffmpeg` audio+video remuxing.

mod dash;
mod error;
mod ffmpeg;
mod hls;
mod net;
mod sidecar;
mod storage;
mod track;
mod ytdlp;

use std::path::Path;

pub use reqwest::header::HeaderMap;
use reqwest::{Client, Url};
use ruscello_http::HttpDownloader;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub use error::MediaError;
pub use net::{BandwidthContext, is_dash_url, is_hls_url, is_manifest_url, split_headers};
pub use sidecar::SidecarManager;
pub use types::{
    FormatPreset, MediaFormat, MediaInspection, MediaKind, MediaPhase, MediaRequest, PlaylistItem,
    SidecarBinaryStatus, SidecarEndpoints, SidecarKind, SidecarSettings, SidecarsStatus,
};
pub use ytdlp::parse_ytdlp_json;

mod types;

/// Context for executing one queued media download.
pub struct MediaTransferJob<'a> {
    pub url: &'a Url,
    pub request: &'a MediaRequest,
    pub session_headers: &'a HeaderMap,
    pub working_dir: &'a Path,
    pub ready_dir: &'a Path,
    pub filename_hint: Option<&'a str>,
    pub concurrency: usize,
    pub bandwidth: &'a BandwidthContext,
    pub settings: &'a SidecarSettings,
    pub cancellation: &'a CancellationToken,
    pub progress: Option<&'a watch::Sender<ruscello_http::DownloadProgress>>,
    pub on_phase: &'a (dyn Fn(MediaPhase) + Send + Sync),
}

impl std::fmt::Debug for MediaTransferJob<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaTransferJob")
            .field("url", &self.url.as_str())
            .field("kind", &self.request.kind)
            .finish_non_exhaustive()
    }
}

/// Top-level media inspector and downloader orchestrating native HLS/DASH and optional sidecars.
#[derive(Debug, Clone)]
pub struct MediaEngine {
    client: Client,
    http: HttpDownloader,
    sidecars: SidecarManager,
}

impl MediaEngine {
    /// Creates a `MediaEngine` sharing `http` downloader and storing sidecars under `data_dir`.
    pub fn new(
        http: HttpDownloader,
        data_dir: impl AsRef<Path>,
        endpoints: SidecarEndpoints,
    ) -> Result<Self, MediaError> {
        Ok(Self {
            client: net::build_client()?,
            http,
            sidecars: SidecarManager::new(data_dir.as_ref(), endpoints),
        })
    }

    pub fn sidecars(&self) -> &SidecarManager {
        &self.sidecars
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Inspects a `.m3u8`, `.mpd`, or extractor/yt-dlp URL and returns selectable formats & playlists.
    pub async fn inspect(
        &self,
        url: &Url,
        session_headers: &HeaderMap,
        settings: &SidecarSettings,
        cancellation: &CancellationToken,
    ) -> Result<MediaInspection, MediaError> {
        let ffmpeg_bin = self
            .sidecars
            .resolve_binary(SidecarKind::Ffmpeg, settings)
            .map(|(path, _)| path);
        let ffmpeg_available = ffmpeg_bin.is_some();

        if is_hls_url(url) {
            return hls::inspect(
                &self.client,
                url,
                session_headers,
                ffmpeg_available,
                cancellation,
            )
            .await;
        }
        if is_dash_url(url) {
            return dash::inspect(
                &self.client,
                url,
                session_headers,
                ffmpeg_available,
                cancellation,
            )
            .await;
        }

        // Check response content-type quickly if URL has no .m3u8/.mpd suffix, else fall back to yt-dlp.
        if let Some((ytdlp_bin, _)) = self.sidecars.resolve_binary(SidecarKind::YtDlp, settings) {
            let _ = self
                .sidecars
                .maybe_auto_update_ytdlp(settings, &self.client, cancellation)
                .await;
            let (_, cookie, ua) = ytdlp::extract_cookie_header(session_headers);
            return ytdlp::inspect(
                &ytdlp_bin,
                url,
                cookie.as_deref(),
                ua.as_deref(),
                ffmpeg_available,
                cancellation,
            )
            .await;
        }

        // Probe URL Content-Type in case a CDN serves .m3u8 or .mpd from an extensionless URL.
        let head = self
            .client
            .head(url.clone())
            .headers(session_headers.clone())
            .send()
            .await;
        if let Ok(response) = head
            && let Some(mime) = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_ascii_lowercase)
        {
            if mime.contains("mpegurl") {
                return hls::inspect(
                    &self.client,
                    url,
                    session_headers,
                    ffmpeg_available,
                    cancellation,
                )
                .await;
            }
            if mime.contains("dash+xml") {
                return dash::inspect(
                    &self.client,
                    url,
                    session_headers,
                    ffmpeg_available,
                    cancellation,
                )
                .await;
            }
        }

        Err(MediaError::SidecarMissing(SidecarKind::YtDlp))
    }

    /// Executes a queued media download into `job.ready_dir` and returns the staged file path.
    pub async fn download(
        &self,
        job: MediaTransferJob<'_>,
    ) -> Result<std::path::PathBuf, MediaError> {
        let ffmpeg_pair = self
            .sidecars
            .resolve_binary(SidecarKind::Ffmpeg, job.settings);
        let ffmpeg_bin = ffmpeg_pair.as_ref().map(|(path, _)| path.as_path());

        let (plain_headers, cookie_header, user_agent) =
            ytdlp::extract_cookie_header(job.session_headers);
        let mut combined_headers = job.request.headers.clone();
        for (k, v) in plain_headers {
            if !combined_headers
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(&k))
            {
                combined_headers.push((k, v));
            }
        }
        let header_map = net::build_headers(&combined_headers, cookie_header.as_deref());

        let effective_kind = if is_hls_url(job.url) {
            MediaKind::Hls
        } else if is_dash_url(job.url) {
            MediaKind::Dash
        } else {
            job.request.kind
        };

        match effective_kind {
            MediaKind::Hls => {
                hls::download(hls::NativeStreamJob {
                    client: &self.client,
                    url: job.url,
                    headers: &header_map,
                    working_dir: job.working_dir,
                    ready_dir: job.ready_dir,
                    filename_hint: job.filename_hint.or(job.request.title.as_deref()),
                    format_selector: job.request.format_selector.as_deref(),
                    variant_uri: job.request.variant_uri.as_deref(),
                    dash_video_id: None,
                    dash_audio_id: None,
                    concurrency: job.concurrency,
                    bandwidth: job.bandwidth,
                    ffmpeg_bin,
                    cancellation: job.cancellation,
                    progress: job.progress,
                    on_phase: job.on_phase,
                })
                .await
            }
            MediaKind::Dash => {
                dash::download(hls::NativeStreamJob {
                    client: &self.client,
                    url: job.url,
                    headers: &header_map,
                    working_dir: job.working_dir,
                    ready_dir: job.ready_dir,
                    filename_hint: job.filename_hint.or(job.request.title.as_deref()),
                    format_selector: job.request.format_selector.as_deref(),
                    variant_uri: None,
                    dash_video_id: job.request.dash_video_id.as_deref(),
                    dash_audio_id: job.request.dash_audio_id.as_deref(),
                    concurrency: job.concurrency,
                    bandwidth: job.bandwidth,
                    ffmpeg_bin,
                    cancellation: job.cancellation,
                    progress: job.progress,
                    on_phase: job.on_phase,
                })
                .await
            }
            MediaKind::YtDlp | MediaKind::Direct => {
                let Some((ytdlp_bin, _)) = self
                    .sidecars
                    .resolve_binary(SidecarKind::YtDlp, job.settings)
                else {
                    return Err(MediaError::SidecarMissing(SidecarKind::YtDlp));
                };
                ytdlp::download(ytdlp::YtDlpJob {
                    ytdlp_bin: &ytdlp_bin,
                    ffmpeg_bin,
                    http: &self.http,
                    client: &self.client,
                    url: job.url,
                    cookie_header: cookie_header.as_deref(),
                    user_agent: user_agent.as_deref(),
                    working_dir: job.working_dir,
                    ready_dir: job.ready_dir,
                    filename_hint: job.filename_hint.or(job.request.title.as_deref()),
                    format_selector: job.request.format_selector.as_deref(),
                    concurrency: job.concurrency,
                    bandwidth: job.bandwidth,
                    cancellation: job.cancellation,
                    progress: job.progress,
                    on_phase: job.on_phase,
                })
                .await
            }
        }
    }

    /// Installs or updates an optional sidecar binary (requires explicit user consent in `settings`).
    pub async fn install_sidecar(
        &self,
        kind: SidecarKind,
        settings: &SidecarSettings,
        cancellation: &CancellationToken,
    ) -> Result<SidecarBinaryStatus, MediaError> {
        self.sidecars
            .install_or_update(kind, settings, &self.client, cancellation)
            .await
    }

    /// Reads current status of managed and custom sidecars.
    pub async fn sidecar_status(&self, settings: &SidecarSettings) -> SidecarsStatus {
        self.sidecars.status(settings).await
    }
}

/// Parser entry points for cargo-fuzz.
#[cfg(feature = "fuzzing")]
pub mod fuzzing {
    /// Exercises native HLS master and media playlist parsing.
    pub fn hls(input: &[u8]) {
        crate::hls::fuzz_parse(input);
    }

    /// Exercises native DASH MPD parsing.
    pub fn dash(input: &[u8]) {
        crate::dash::fuzz_parse(input);
    }
}
