use std::{
    error::Error,
    fmt::Debug,
    future::Future,
    io,
    pin::Pin,
    time::{Duration, SystemTime},
};

use reqwest::{Response, StatusCode, Url, header};
use tokio::{sync::watch, time};
use tokio_util::sync::CancellationToken;

use crate::HttpError;

/// How an HTTP failure should be handled by the downloader or its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Transient,
    Permanent,
    Authentication,
    ExpiredLink,
}

impl HttpError {
    pub fn classification(&self) -> ErrorClass {
        match self {
            Self::Status(StatusCode::UNAUTHORIZED | StatusCode::PROXY_AUTHENTICATION_REQUIRED) => {
                ErrorClass::Authentication
            }
            Self::Status(StatusCode::FORBIDDEN | StatusCode::GONE) => ErrorClass::ExpiredLink,
            Self::ExpiredRepresentation => ErrorClass::ExpiredLink,
            Self::Status(code)
                if code.is_server_error()
                    || matches!(
                        *code,
                        StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT
                    ) =>
            {
                ErrorClass::Transient
            }
            Self::Throttled { .. } | Self::Truncated => ErrorClass::Transient,
            Self::Request(error)
                if error.is_connect()
                    || error.is_timeout()
                    || error.is_body()
                    || error.is_decode() =>
            {
                ErrorClass::Transient
            }
            _ => ErrorClass::Permanent,
        }
    }

    pub(crate) fn congested(&self) -> bool {
        match self {
            Self::Status(StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE)
            | Self::Throttled { .. }
            | Self::Truncated => true,
            Self::Request(error) => {
                error.is_body()
                    || transport_kind(error).is_some_and(|kind| {
                        matches!(
                            kind,
                            io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::BrokenPipe
                        )
                    })
            }
            _ => false,
        }
    }

    pub(crate) fn offline(&self) -> bool {
        match self {
            Self::Request(error) => transport_kind(error).is_some_and(|kind| {
                matches!(
                    kind,
                    io::ErrorKind::NetworkDown
                        | io::ErrorKind::NetworkUnreachable
                        | io::ErrorKind::HostUnreachable
                        | io::ErrorKind::NotConnected
                )
            }),
            _ => false,
        }
    }
}

fn transport_kind(error: &reqwest::Error) -> Option<io::ErrorKind> {
    let mut source = error.source();
    while let Some(error) = source {
        if let Some(io) = error.downcast_ref::<io::Error>()
            && io.kind() != io::ErrorKind::Other
        {
            return Some(io.kind());
        }
        source = error.source();
    }
    None
}

pub(crate) fn status_error(response: &Response) -> HttpError {
    let status = response.status();
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) && let Some(value) = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        && let Some(retry_after) = parse_retry_after(value, SystemTime::now())
    {
        return HttpError::Throttled {
            status,
            retry_after,
        };
    }
    HttpError::Status(status)
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let delay = value.parse::<u64>().ok().map(Duration::from_secs);
    delay.or_else(|| {
        let date = httpdate::parse_http_date(value).ok()?;
        Some(date.duration_since(now).unwrap_or_default())
    })
}

/// Retry budget and exponential backoff for transient errors.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_delay: Duration,
    pub maximum_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 8,
            initial_delay: Duration::from_millis(200),
            maximum_delay: Duration::from_secs(30),
        }
    }
}

#[derive(Default)]
pub(crate) struct RetryState {
    failures: u32,
    offline_failures: u32,
    progress: u64,
}

impl RetryState {
    pub(crate) fn record_progress(&mut self, completed: u64) {
        if completed >= self.progress.saturating_add(1024 * 1024) {
            self.failures = 0;
            self.progress = completed;
        }
    }

    pub(crate) fn next_delay(
        &mut self,
        error: &HttpError,
        policy: &RetryPolicy,
        online: bool,
    ) -> Option<Duration> {
        if error.classification() != ErrorClass::Transient {
            return None;
        }
        if !online || error.offline() {
            let exponent = self.offline_failures;
            self.offline_failures = self.offline_failures.saturating_add(1);
            let delay = backoff(policy, exponent).max(Duration::from_millis(250));
            tracing::debug!("Waiting for network connectivity");
            return Some(delay);
        }
        self.offline_failures = 0;
        if self.failures >= policy.max_retries {
            return None;
        }
        let delay = backoff(policy, self.failures);
        self.failures += 1;
        Some(match error {
            HttpError::Throttled { retry_after, .. } => delay.max(*retry_after),
            _ => delay,
        })
    }
}

fn backoff(policy: &RetryPolicy, exponent: u32) -> Duration {
    let exponent = 1u32 << exponent.min(30);
    let nominal = policy
        .initial_delay
        .saturating_mul(exponent)
        .min(policy.maximum_delay);
    let jitter = getrandom::u64().unwrap_or(u64::MAX / 2) as f64 / u64::MAX as f64;
    Duration::try_from_secs_f64(nominal.as_secs_f64() * (0.5 + jitter))
        .unwrap_or(policy.maximum_delay)
        .min(policy.maximum_delay)
}

pub(crate) async fn wait(
    delay: Duration,
    cancellation: &CancellationToken,
) -> Result<(), HttpError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(HttpError::Cancelled),
        () = time::sleep(delay) => Ok(()),
    }
}

/// Shared network availability signal. Callers may feed it from an operating-system monitor.
#[derive(Debug, Clone)]
pub struct Connectivity(watch::Sender<bool>);

impl Default for Connectivity {
    fn default() -> Self {
        Self(watch::channel(true).0)
    }
}

impl Connectivity {
    pub fn set_online(&self, online: bool) {
        self.0.send_if_modified(|current| {
            let changed = *current != online;
            *current = online;
            changed
        });
    }

    pub fn is_online(&self) -> bool {
        *self.0.borrow()
    }

    pub(crate) async fn wait_online(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), HttpError> {
        let mut receiver = self.0.subscribe();
        tokio::select! {
            () = cancellation.cancelled() => Err(HttpError::Cancelled),
            result = receiver.wait_for(|online| *online) => result.map(|_| ()).map_err(|_| HttpError::WorkerStopped),
        }
    }
}

pub type ResolvedLink<'a> = Pin<Box<dyn Future<Output = Result<Url, HttpError>> + Send + 'a>>;

pub type ResolvedTransfer<'a> =
    Pin<Box<dyn Future<Output = Result<TransferLink, HttpError>> + Send + 'a>>;

/// Ephemeral request metadata for a single resolved URL.
#[derive(Clone)]
pub struct TransferLink {
    pub url: Url,
    pub headers: header::HeaderMap,
    pub expires: Option<SystemTime>,
    pub max_connections: Option<std::num::NonZeroUsize>,
}

impl TransferLink {
    pub fn new(url: Url) -> Self {
        Self {
            url,
            headers: header::HeaderMap::new(),
            expires: None,
            max_connections: None,
        }
    }
}

impl Debug for TransferLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransferLink")
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

/// Supplies a fresh direct URL when an authenticated hoster's link expires.
pub trait ExpiredLinkResolver: Debug + Send + Sync {
    fn resolve<'a>(&'a self, expired: &'a Url) -> ResolvedLink<'a>;

    fn resolve_transfer<'a>(&'a self, expired: &'a Url) -> ResolvedTransfer<'a> {
        Box::pin(async move { self.resolve(expired).await.map(TransferLink::new) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_http_statuses() {
        for (status, class) in [
            (StatusCode::UNAUTHORIZED, ErrorClass::Authentication),
            (StatusCode::FORBIDDEN, ErrorClass::ExpiredLink),
            (StatusCode::GONE, ErrorClass::ExpiredLink),
            (StatusCode::NOT_FOUND, ErrorClass::Permanent),
            (StatusCode::BAD_GATEWAY, ErrorClass::Transient),
            (StatusCode::TOO_MANY_REQUESTS, ErrorClass::Transient),
        ] {
            assert_eq!(HttpError::Status(status).classification(), class);
        }
    }

    #[test]
    fn offline_waits_preserve_the_retry_budget() {
        let policy = RetryPolicy {
            max_retries: 1,
            ..RetryPolicy::default()
        };
        let mut state = RetryState::default();
        let error = HttpError::Truncated;
        for _ in 0..10 {
            assert!(state.next_delay(&error, &policy, false).is_some());
        }
        assert!(state.next_delay(&error, &policy, true).is_some());
        assert!(state.next_delay(&error, &policy, true).is_none());
    }
}
