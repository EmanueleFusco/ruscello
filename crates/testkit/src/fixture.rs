use std::{num::NonZeroU64, ops::Range, time::Duration};

use axum::http::StatusCode;

/// Random-access, deterministic test data without a resident file-sized buffer.
#[derive(Clone, Copy, Debug)]
pub struct Payload {
    pub(crate) seed: u64,
    pub(crate) size: u64,
}

impl Payload {
    pub fn len(self) -> u64 {
        self.size
    }

    pub fn is_empty(self) -> bool {
        self.size == 0
    }

    /// Generates the requested half-open byte range, clipped to the payload size.
    pub fn bytes(self, range: Range<u64>) -> Vec<u8> {
        let end = range.end.min(self.size);
        let mut offset = range.start;
        let mut bytes = Vec::with_capacity(end.saturating_sub(offset) as usize);
        while offset < end {
            // SplitMix64 permits seeking to an arbitrary 8-byte block.
            let mut value = self
                .seed
                .wrapping_add((offset / 8 + 1).wrapping_mul(0x9e3779b97f4a7c15));
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            let word = (value ^ (value >> 31)).to_le_bytes();
            let start = (offset % 8) as usize;
            let count = (8 - start).min((end - offset) as usize);
            bytes.extend_from_slice(&word[start..start + count]);
            offset += count as u64;
        }
        bytes
    }
}

/// A version of a file served by the fixture.
#[derive(Clone, Debug)]
pub struct Representation {
    pub(crate) payload: Payload,
    pub(crate) etag: Option<String>,
    pub(crate) modified: Option<String>,
}

impl Representation {
    pub fn random(seed: u64, size: u64) -> Self {
        Self {
            payload: Payload { seed, size },
            etag: Some(format!("\"{seed:016x}-{size}\"")),
            modified: None,
        }
    }

    /// Sets an ETag header value, including quotes and the optional weak prefix.
    pub fn etag(mut self, value: impl Into<String>) -> Self {
        self.etag = Some(value.into());
        self
    }

    pub fn without_etag(mut self) -> Self {
        self.etag = None;
        self
    }

    /// Sets a Last-Modified header value using HTTP date syntax.
    pub fn last_modified(mut self, value: impl Into<String>) -> Self {
        self.modified = Some(value.into());
        self
    }

    pub fn payload(&self) -> Payload {
        self.payload
    }
}

/// Configures one file route and its optional HTTP faults.
#[derive(Clone, Debug)]
pub struct FileSpec {
    pub(crate) representation: Representation,
    pub(crate) replacement: Option<(u64, Representation)>,
    pub(crate) filename: String,
    pub(crate) ranges: bool,
    pub(crate) chunked: bool,
    pub(crate) redirect_hops: u8,
    pub(crate) disposition: Option<String>,
    pub(crate) content_type: Option<String>,
    pub(crate) drop_after: Option<(u64, Option<u64>)>,
    pub(crate) throttle: Option<NonZeroU64>,
    pub(crate) max_connections: Option<(usize, StatusCode)>,
    pub(crate) latency: Duration,
    pub(crate) intermittent: Option<(NonZeroU64, StatusCode)>,
}

impl FileSpec {
    /// Serves seeded bytes, with byte ranges and Content-Length by default.
    pub fn random(seed: u64, size: u64) -> Self {
        Self {
            representation: Representation::random(seed, size),
            replacement: None,
            filename: "payload.bin".into(),
            ranges: true,
            chunked: false,
            redirect_hops: 0,
            disposition: None,
            content_type: None,
            drop_after: None,
            throttle: None,
            max_connections: None,
            latency: Duration::ZERO,
            intermittent: None,
        }
    }

    pub fn representation(mut self, value: Representation) -> Self {
        self.representation = value;
        self
    }

    /// Changes the representation for new requests after the server emits these bytes.
    pub fn change_after_bytes(mut self, bytes: u64, replacement: Representation) -> Self {
        self.replacement = Some((bytes, replacement));
        self
    }

    pub fn filename(mut self, value: impl Into<String>) -> Self {
        self.filename = value.into();
        self
    }

    pub fn ranges(mut self, supported: bool) -> Self {
        self.ranges = supported;
        self
    }

    /// Omits Content-Length and streams the body using HTTP/1.1 chunked framing.
    pub fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }

    pub fn redirects(mut self, hops: u8) -> Self {
        self.redirect_hops = hops;
        self
    }

    pub fn content_disposition(mut self, value: impl Into<String>) -> Self {
        self.disposition = Some(value.into());
        self
    }

    pub fn content_type(mut self, value: impl Into<String>) -> Self {
        self.content_type = Some(value.into());
        self
    }

    /// Breaks every response that would send more than this many bytes.
    pub fn drop_after_bytes(mut self, bytes: u64) -> Self {
        self.drop_after = Some((bytes, None));
        self
    }

    /// Breaks at most this many responses, regardless of subsequent retries.
    pub fn drop_first_responses(mut self, count: u64, after_bytes: u64) -> Self {
        self.drop_after = Some((after_bytes, Some(count)));
        self
    }

    /// Limits bytes per second for each response. Zero disables the limit.
    pub fn throttle_per_conn(mut self, bytes_per_second: u64) -> Self {
        self.throttle = NonZeroU64::new(bytes_per_second);
        self
    }

    /// Caps concurrent responses from an IP and returns the chosen status above the cap.
    pub fn max_conns_per_ip(mut self, max: usize, status: StatusCode) -> Self {
        self.max_connections = Some((max, status));
        self
    }

    pub fn latency(mut self, value: Duration) -> Self {
        self.latency = value;
        self
    }

    /// Returns the chosen status every Nth request. Zero disables the fault.
    pub fn intermittent_error(mut self, every: u64, status: StatusCode) -> Self {
        self.intermittent = NonZeroU64::new(every).map(|period| (period, status));
        self
    }
}
