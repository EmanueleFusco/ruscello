use std::{fs::File, io::Write as _, ops::Range, path::Path, time::Duration};

use reqwest::{
    Client, StatusCode, Url,
    header::{self, HeaderMap, HeaderName, HeaderValue},
};
use ruscello_http::{BandwidthLimit, RetryPolicy, sanitize_filename};
use tempfile::NamedTempFile;
use tokio::{sync::watch, time};
use tokio_util::sync::CancellationToken;

use crate::MediaError;

const MAX_RETRIES: u32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Builds the shared `rustls` HTTP client for media manifests and segments.
pub(crate) fn build_client() -> Result<Client, MediaError> {
    Ok(Client::builder()
        .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if !matches!(attempt.url().scheme(), "http" | "https") {
                attempt.error("invalid redirect URL")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

/// Validates and parses an HTTP or HTTPS URL.
pub(crate) fn parse_http_url(raw: &str) -> Result<Url, MediaError> {
    let mut url = Url::parse(raw.trim()).map_err(|_| MediaError::InvalidUrl(raw.to_owned()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(MediaError::InvalidUrl(raw.to_owned()));
    }
    url.set_fragment(None);
    Ok(url)
}

/// Builds a `HeaderMap` from header key-value pairs and optional cookie header.
pub(crate) fn build_headers(pairs: &[(String, String)], cookie_header: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        if name.eq_ignore_ascii_case("cookie") {
            continue;
        }
        if let (Ok(header_name), Ok(header_value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(header_name, header_value);
        }
    }
    if let Some(cookie) = cookie_header
        && !cookie.trim().is_empty()
        && let Ok(mut value) = HeaderValue::from_str(cookie)
    {
        value.set_sensitive(true);
        headers.insert(header::COOKIE, value);
    }
    headers
}

/// Extracts `(headers, cookie_header)` from a `HeaderMap`, keeping sensitive cookies separated.
pub fn split_headers(map: &HeaderMap) -> (Vec<(String, String)>, Option<String>) {
    let mut headers = Vec::new();
    let mut cookie = None;
    for (name, value) in map {
        let Ok(text) = value.to_str() else {
            continue;
        };
        if name == header::COOKIE {
            cookie = Some(text.to_owned());
        } else {
            headers.push((name.as_str().to_owned(), text.to_owned()));
        }
    }
    (headers, cookie)
}

/// Returns whether a URL points to an HLS `.m3u8` playlist by path extension.
pub fn is_hls_url(url: &Url) -> bool {
    extension_lower(url).as_deref() == Some("m3u8")
}

/// Returns whether a URL points to a DASH `.mpd` manifest by path extension.
pub fn is_dash_url(url: &Url) -> bool {
    extension_lower(url).as_deref() == Some("mpd")
}

/// Returns whether a URL points to a supported adaptive manifest (`.m3u8` or `.mpd`).
pub fn is_manifest_url(url: &Url) -> bool {
    matches!(extension_lower(url).as_deref(), Some("m3u8" | "mpd"))
}

pub(crate) fn extension_lower(url: &Url) -> Option<String> {
    let last = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))?;
    let (_, ext) = last.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

/// Sanitizes a title or source filename and ensures the requested extension.
pub(crate) fn ensure_filename(title_or_name: &str, ext: &str) -> String {
    let sanitized = sanitize_filename(title_or_name.trim());
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    if ext.is_empty() {
        return sanitized;
    }
    let current = Path::new(&sanitized)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if current.as_deref() == Some(ext.as_str()) {
        sanitized
    } else if matches!(current.as_deref(), Some("m3u8" | "mpd" | "m3u")) {
        let stem = Path::new(&sanitized)
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("media");
        sanitize_filename(&format!("{stem}.{ext}"))
    } else {
        sanitize_filename(&format!("{sanitized}.{ext}"))
    }
}

/// Derives a fallback human title from a media URL path or host.
pub(crate) fn title_from_url(url: &Url) -> String {
    let segment = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))
        .map(|part| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8_lossy()
                .into_owned()
        });
    let raw = segment
        .as_deref()
        .and_then(|name| {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".m3u8") || lower.ends_with(".mpd") {
                name.rsplit_once('.')
                    .map(|(stem, _)| stem.to_owned())
                    .filter(|stem| {
                        !matches!(
                            stem.to_ascii_lowercase().as_str(),
                            "master" | "index" | "manifest" | "playlist"
                        )
                    })
            } else {
                Some(name.to_owned())
            }
        })
        .unwrap_or_else(|| url.host_str().unwrap_or("media").to_owned());
    sanitize_filename(&raw)
}

/// Fetches a small manifest, playlist, or key resource into memory with retries and cancellation.
pub(crate) async fn fetch_bytes(
    client: &Client,
    url: &Url,
    headers: &HeaderMap,
    range: Option<Range<u64>>,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    let policy = RetryPolicy {
        max_retries: MAX_RETRIES,
        ..RetryPolicy::default()
    };
    let mut attempt = 0_u32;
    loop {
        if cancellation.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let mut request = client.get(url.clone()).headers(headers.clone());
        if let Some(ref span) = range
            && span.end > span.start
        {
            request = request.header(
                header::RANGE,
                format!("bytes={}-{}", span.start, span.end - 1),
            );
        }
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(MediaError::Cancelled),
            response = request.send() => response,
        };
        match result {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    let body = tokio::select! {
                        () = cancellation.cancelled() => return Err(MediaError::Cancelled),
                        bytes = response.bytes() => bytes,
                    };
                    match body {
                        Ok(bytes) => return Ok(bytes.to_vec()),
                        Err(error) if attempt < policy.max_retries => {
                            attempt += 1;
                            tracing::debug!(attempt, %error, "Retrying manifest/key body read");
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else if is_retryable_status(status) && attempt < policy.max_retries {
                    attempt += 1;
                    tracing::debug!(attempt, status = status.as_u16(), "Retrying HTTP request");
                } else {
                    return Err(MediaError::HttpStatus(status.as_u16()));
                }
            }
            Err(error) if !error.is_builder() && attempt < policy.max_retries => {
                attempt += 1;
                tracing::debug!(attempt, %error, "Retrying failed HTTP request");
            }
            Err(error) => return Err(error.into()),
        }
        let delay = backoff_delay(attempt);
        tokio::select! {
            () = cancellation.cancelled() => return Err(MediaError::Cancelled),
            () = time::sleep(delay) => {}
        }
    }
}

/// Streams a segment payload while enforcing global, category, and per-download bandwidth limits.
pub(crate) async fn fetch_segment_rate_limited(
    client: &Client,
    url: &Url,
    headers: &HeaderMap,
    range: Option<Range<u64>>,
    bandwidth: &BandwidthContext,
    cancellation: &CancellationToken,
    on_chunk: Option<&(dyn Fn(u64) + Send + Sync)>,
) -> Result<Vec<u8>, MediaError> {
    let policy = RetryPolicy {
        max_retries: MAX_RETRIES,
        ..RetryPolicy::default()
    };
    let mut attempt = 0_u32;
    loop {
        if cancellation.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let mut request = client.get(url.clone()).headers(headers.clone());
        if let Some(ref span) = range
            && span.end > span.start
        {
            request = request.header(
                header::RANGE,
                format!("bytes={}-{}", span.start, span.end - 1),
            );
        }
        let sent = tokio::select! {
            () = cancellation.cancelled() => return Err(MediaError::Cancelled),
            response = request.send() => response,
        };
        match sent {
            Ok(mut response) => {
                let status = response.status();
                if !status.is_success() {
                    if is_retryable_status(status) && attempt < policy.max_retries {
                        attempt += 1;
                        let delay = backoff_delay(attempt);
                        tokio::select! {
                            () = cancellation.cancelled() => return Err(MediaError::Cancelled),
                            () = time::sleep(delay) => {}
                        }
                        continue;
                    }
                    return Err(MediaError::HttpStatus(status.as_u16()));
                }
                let mut buffer = Vec::new();
                let stream_error = loop {
                    let chunk = tokio::select! {
                        () = cancellation.cancelled() => return Err(MediaError::Cancelled),
                        next = response.chunk() => next,
                    };
                    match chunk {
                        Ok(Some(bytes)) => {
                            let mut offset = 0;
                            while offset < bytes.len() {
                                let grant = bandwidth
                                    .acquire(bytes.len() - offset, cancellation)
                                    .await?;
                                let next = (offset + grant).min(bytes.len());
                                buffer.extend_from_slice(&bytes[offset..next]);
                                if let Some(callback) = on_chunk {
                                    callback((next - offset) as u64);
                                }
                                offset = next;
                            }
                        }
                        Ok(None) => return Ok(buffer),
                        Err(error) => break error,
                    }
                };
                if attempt < policy.max_retries {
                    attempt += 1;
                    tracing::debug!(attempt, error = %stream_error, "Retrying interrupted media segment");
                } else {
                    return Err(stream_error.into());
                }
            }
            Err(error) if !error.is_builder() && attempt < policy.max_retries => {
                attempt += 1;
                tracing::debug!(attempt, %error, "Retrying segment request");
            }
            Err(error) => return Err(error.into()),
        }
        let delay = backoff_delay(attempt);
        tokio::select! {
            () = cancellation.cancelled() => return Err(MediaError::Cancelled),
            () = time::sleep(delay) => {}
        }
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

fn backoff_delay(attempt: u32) -> Duration {
    Duration::from_millis(200 * 2_u64.saturating_pow(attempt.saturating_sub(1).min(5)))
}

/// Global, category, and per-download token buckets shared with HTTP transfers.
#[derive(Debug, Clone, Default)]
pub struct BandwidthContext {
    pub global: BandwidthLimit,
    pub category: BandwidthLimit,
    pub individual: Option<BandwidthLimit>,
}

impl BandwidthContext {
    async fn acquire(
        &self,
        requested: usize,
        cancellation: &CancellationToken,
    ) -> Result<usize, MediaError> {
        tokio::select! {
            () = cancellation.cancelled() => Err(MediaError::Cancelled),
            granted = self.global.acquire(&self.category, self.individual.as_ref(), requested) => Ok(granted),
        }
    }
}

/// Writes cookie header pairs into a private Netscape `cookies.txt` file deleted on drop.
pub(crate) fn write_netscape_cookies_file(
    url: &Url,
    cookie_header: &str,
) -> Result<NamedTempFile, MediaError> {
    let host = url
        .host_str()
        .ok_or_else(|| MediaError::InvalidUrl(url.as_str().to_owned()))?;
    let secure = if url.scheme() == "https" {
        "TRUE"
    } else {
        "FALSE"
    };
    let mut temp = NamedTempFile::new()?;
    set_private_permissions(temp.as_file())?;
    writeln!(temp, "# Netscape HTTP Cookie File")?;
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if !name.is_empty() {
            writeln!(temp, "{host}\tFALSE\t/\t{secure}\t0\t{name}\t{value}")?;
        }
    }
    temp.as_file().sync_all()?;
    Ok(temp)
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = file.metadata()?.permissions();
    perms.set_mode(0o600);
    file.set_permissions(perms)
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File) -> std::io::Result<()> {
    Ok(())
}

/// Progress reporter forwarding byte updates safely to `ruscello_http::DownloadProgress`.
pub(crate) fn report_progress(
    sender: Option<&watch::Sender<ruscello_http::DownloadProgress>>,
    completed: u64,
    total: Option<u64>,
) {
    if let Some(sender) = sender {
        let next = ruscello_http::DownloadProgress {
            completed,
            total,
            resumable: true,
            ..Default::default()
        };
        sender.send_if_modified(|current| {
            let changed = *current != next;
            *current = next;
            changed
        });
    }
}
