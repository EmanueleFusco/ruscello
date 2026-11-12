use std::{ops::Range, time::Duration};

use reqwest::{
    Client, Response, StatusCode, Url,
    header::{self, HeaderMap, HeaderValue},
};

use crate::filename::suggest_filename;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("only HTTP and HTTPS URLs are supported")]
    UnsupportedScheme,
    #[error("HTTP request failed")]
    Request(#[source] reqwest::Error),
    #[error("the server returned HTTP {0}")]
    Status(StatusCode),
    #[error("invalid HTTP response: {0}")]
    InvalidResponse(&'static str),
    #[error("the server returned HTTP {status} and requested a retry delay")]
    Throttled {
        status: StatusCode,
        retry_after: Duration,
    },
    #[error("the representation changed")]
    RepresentationChanged,
    #[error("download cancelled")]
    Cancelled,
    #[error("combining mirrors requires an expected checksum")]
    MirrorChecksumRequired,
    #[error("the response ended before its declared length")]
    Truncated,
    #[error("an HTTP download worker stopped")]
    WorkerStopped,
    #[error(transparent)]
    Storage(#[from] crate::WriterError),
    #[error(transparent)]
    Segment(#[from] crate::SegmentError),
}

impl From<reqwest::Error> for HttpError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error.without_url())
    }
}

/// Representation validators obtained from the server.
#[derive(Debug, Clone, Default)]
pub struct Validators {
    pub etag: Option<HeaderValue>,
    pub last_modified: Option<HeaderValue>,
    date_is_strong: bool,
}

impl Validators {
    /// Returns a validator suitable for a conditional byte-range request.
    pub fn if_range(&self) -> Option<&HeaderValue> {
        match self.etag.as_ref() {
            Some(tag) if strong_etag(tag) => Some(tag),
            Some(_) => None,
            None if self.date_is_strong => self.last_modified.as_ref(),
            None => None,
        }
    }

    fn read(headers: &HeaderMap) -> Self {
        let date = |name| httpdate::parse_http_date(headers.get(name)?.to_str().ok()?).ok();
        let age = date(header::DATE)
            .zip(date(header::LAST_MODIFIED))
            .and_then(|(response, modified)| response.duration_since(modified).ok());
        Self {
            etag: headers.get(header::ETAG).cloned(),
            last_modified: headers.get(header::LAST_MODIFIED).cloned(),
            date_is_strong: age.is_some_and(|age| age >= Duration::from_secs(60)),
        }
    }
}

fn strong_etag(tag: &HeaderValue) -> bool {
    tag.as_bytes()
        .strip_prefix(b"\"")
        .and_then(|tag| tag.strip_suffix(b"\""))
        .is_some_and(|tag| {
            tag.iter()
                .all(|&byte| byte == 0x21 || (0x23..=0x7e).contains(&byte) || byte >= 0x80)
        })
}

#[derive(Debug, Clone)]
pub struct Probe {
    pub final_url: Url,
    pub size: Option<u64>,
    pub supports_ranges: bool,
    pub filename: String,
    pub validators: Validators,
}

/// Builds a reusable HTTP/HTTPS download client with finite connect and read timeouts.
pub fn client() -> Result<Client, HttpError> {
    build_client(false)
}

pub(crate) fn build_client(manual_redirects: bool) -> Result<Client, HttpError> {
    let builder = Client::builder()
        .user_agent(concat!("Ruscello/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();
    let builder = if manual_redirects {
        builder.redirect(reqwest::redirect::Policy::none())
    } else {
        builder
    };
    Ok(builder.build()?)
}

/// Probes an HTTP resource using a GET without consuming the body.
pub async fn probe(client: &Client, url: &Url) -> Result<Probe, HttpError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HttpError::UnsupportedScheme);
    }
    let response = client
        .get(url.clone())
        .header(header::ACCEPT_ENCODING, "identity")
        .header(header::RANGE, "bytes=0-")
        .send()
        .await?;
    parse_probe(&response)
}

pub(crate) fn parse_probe(response: &Response) -> Result<Probe, HttpError> {
    let (size, supports_ranges) = probe_size(response)?;
    let headers = response.headers();
    let final_url = response.url().clone();
    let filename = suggest_filename(headers, &final_url);
    let validators = Validators::read(headers);
    tracing::debug!(
        ?size,
        supports_ranges,
        status = response.status().as_u16(),
        "HTTP probe complete"
    );
    Ok(Probe {
        final_url,
        size,
        supports_ranges,
        filename,
        validators,
    })
}

fn probe_size(response: &Response) -> Result<(Option<u64>, bool), HttpError> {
    let headers = response.headers();
    match response.status() {
        StatusCode::OK => {
            require_identity(headers)?;
            Ok((response.content_length(), false))
        }
        StatusCode::PARTIAL_CONTENT => {
            require_identity(headers)?;
            let (range, size) = content_range(headers)?;
            if range.start != 0
                || response
                    .content_length()
                    .is_some_and(|length| length != range.end)
            {
                return Err(HttpError::InvalidResponse(
                    "range does not match the probe request",
                ));
            }
            Ok((size, true))
        }
        StatusCode::RANGE_NOT_SATISFIABLE
            if headers
                .get(header::CONTENT_RANGE)
                .is_some_and(|range| range == "bytes */0") =>
        {
            Ok((Some(0), true))
        }
        _ => Err(crate::retry::status_error(response)),
    }
}

pub(crate) fn require_identity(headers: &HeaderMap) -> Result<(), HttpError> {
    if headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .any(|encoding| encoding != "identity")
    {
        return Err(HttpError::InvalidResponse("unexpected content encoding"));
    }
    Ok(())
}

pub(crate) fn content_range(headers: &HeaderMap) -> Result<(Range<u64>, Option<u64>), HttpError> {
    parse_content_range(headers).ok_or(HttpError::InvalidResponse(
        "missing or invalid Content-Range",
    ))
}

fn parse_content_range(headers: &HeaderMap) -> Option<(Range<u64>, Option<u64>)> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (unit, range) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (span, total) = range.split_once('/')?;
    let total = if total == "*" {
        None
    } else {
        Some(unsigned(total)?)
    };
    let (start, last) = span.split_once('-')?;
    let start = unsigned(start)?;
    let end = unsigned(last)?.checked_add(1)?;
    (start < end && total.is_none_or(|total| end <= total)).then_some((start..end, total))
}

fn unsigned(value: &str) -> Option<u64> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}
