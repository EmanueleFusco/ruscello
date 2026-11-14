use std::{
    collections::HashMap,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard},
};

use reqwest::{
    Client, Response, StatusCode, Url,
    header::{self, HeaderMap},
};
use tokio::sync::watch;

use crate::{
    HttpError, Probe,
    probe::{build_client, parse_probe},
};

/// Shared cap on active HTTP requests to each hostname, including redirect targets.
#[derive(Debug, Clone)]
pub struct HostConnections(Arc<HostState>);

#[derive(Debug)]
struct HostState {
    usage: Mutex<Usage>,
    changed: watch::Sender<()>,
}

#[derive(Debug)]
struct Usage {
    limit: NonZeroUsize,
    active: HashMap<String, usize>,
}

impl HostConnections {
    pub fn new(limit: NonZeroUsize) -> Self {
        Self(Arc::new(HostState {
            usage: Mutex::new(Usage {
                limit,
                active: HashMap::new(),
            }),
            changed: watch::channel(()).0,
        }))
    }

    /// Lower limits take effect as existing requests finish.
    pub fn set_limit(&self, limit: NonZeroUsize) {
        self.usage().limit = limit;
        self.0.changed.send_replace(());
    }

    fn usage(&self) -> MutexGuard<'_, Usage> {
        self.0
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn try_acquire(&self, hostname: &str) -> bool {
        let mut usage = self.usage();
        if usage.active.get(hostname).copied().unwrap_or(0) >= usage.limit.get() {
            return false;
        }
        *usage.active.entry(hostname.to_owned()).or_default() += 1;
        true
    }

    async fn acquire(&self, hostname: String) -> HostPermit {
        let mut changed = self.0.changed.subscribe();
        while !self.try_acquire(&hostname) {
            let _ = changed.changed().await;
        }
        HostPermit {
            owner: self.clone(),
            hostname,
        }
    }
}

#[derive(Debug)]
struct HostPermit {
    owner: HostConnections,
    hostname: String,
}

impl Drop for HostPermit {
    fn drop(&mut self) {
        let mut usage = self.owner.usage();
        if let Some(active) = usage.active.get_mut(&self.hostname) {
            *active -= 1;
            if *active == 0 {
                usage.active.remove(&self.hostname);
            }
        }
        drop(usage);
        self.owner.0.changed.send_replace(());
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Transport {
    client: Client,
    hosts: Option<HostConnections>,
}

pub(crate) struct ActiveResponse {
    response: Response,
    _permit: Option<HostPermit>,
}

impl Deref for ActiveResponse {
    type Target = Response;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

impl DerefMut for ActiveResponse {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.response
    }
}

impl Transport {
    pub(crate) fn new(hosts: Option<HostConnections>) -> Result<Self, HttpError> {
        Ok(Self {
            client: build_client(true)?,
            hosts,
        })
    }

    pub(crate) async fn probe(&self, url: &Url) -> Result<Probe, HttpError> {
        self.probe_with(url, HeaderMap::new()).await
    }

    pub(crate) async fn probe_with(
        &self,
        url: &Url,
        mut headers: HeaderMap,
    ) -> Result<Probe, HttpError> {
        headers.insert(header::RANGE, header::HeaderValue::from_static("bytes=0-"));
        let response = self.get(url, headers, true).await?;
        parse_probe(&response)
    }

    pub(crate) async fn get(
        &self,
        url: &Url,
        mut headers: HeaderMap,
        follow_redirects: bool,
    ) -> Result<ActiveResponse, HttpError> {
        headers.insert(
            header::ACCEPT_ENCODING,
            header::HeaderValue::from_static("identity"),
        );
        let mut url = url.clone();
        for _ in 0..=10 {
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(HttpError::UnsupportedScheme);
            }
            let permit = if let Some(hosts) = &self.hosts {
                let host = url.host_str().ok_or(HttpError::UnsupportedScheme)?;
                Some(hosts.acquire(host.trim_end_matches('.').to_owned()).await)
            } else {
                None
            };
            let response = self
                .client
                .get(url.clone())
                .headers(headers.clone())
                .send()
                .await?;
            if !is_redirect(response.status()) {
                return Ok(ActiveResponse {
                    response,
                    _permit: permit,
                });
            }
            if !follow_redirects {
                return Err(HttpError::RepresentationChanged);
            }
            let next = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| url.join(location).ok())
                .ok_or(HttpError::InvalidResponse("invalid redirect location"))?;
            if next.origin() != url.origin() {
                let mut safe = HeaderMap::new();
                for name in [header::ACCEPT_ENCODING, header::RANGE, header::IF_RANGE] {
                    if let Some(value) = headers.remove(&name) {
                        safe.insert(name, value);
                    }
                }
                headers = safe;
            }
            url = next;
        }
        Err(HttpError::InvalidResponse("too many redirects"))
    }
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_limits_are_independent_and_change_for_waiting_requests() {
        let limit = HostConnections::new(NonZeroUsize::MIN);
        let first = limit.acquire("first.test".into()).await;
        let other = limit.acquire("other.test".into()).await;
        assert!(!limit.try_acquire("first.test"));
        assert!(!limit.try_acquire("other.test"));
        let waiting = limit.acquire("first.test".into());
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            _ = &mut waiting => panic!("host permit exceeded the limit"),
            () = async {} => {}
        }
        limit.set_limit(NonZeroUsize::MIN.saturating_add(1));
        let second = waiting.await;
        assert!(!limit.try_acquire("first.test"));
        drop((first, second, other));
        assert!(limit.usage().active.is_empty());
    }
}
