use futures::future::BoxFuture;
use governor::DefaultDirectRateLimiter as RateLimiter;
use governor::Quota;
use serde::Deserialize;
use serde::Serialize;
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::watch;

/// Optional request budget shared with other protocols by an embedding application.
pub type DownloadBudget = Arc<dyn Fn(NonZeroU32) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitsConfig {
    pub upload_bps: Option<NonZeroU32>,
    pub download_bps: Option<NonZeroU32>,
}

struct Limit {
    config: watch::Sender<Option<(NonZeroU32, Arc<RateLimiter>)>>,
}

impl Limit {
    fn config(bps: Option<NonZeroU32>) -> Option<(NonZeroU32, Arc<RateLimiter>)> {
        bps.map(|bps| (bps, Arc::new(RateLimiter::direct(Quota::per_second(bps)))))
    }

    fn new(bps: Option<NonZeroU32>) -> Self {
        Self {
            config: watch::Sender::new(Self::config(bps)),
        }
    }

    async fn acquire(&self, size: NonZeroU32) -> crate::Result<()> {
        if self.config.borrow().is_none() {
            return Ok(());
        }
        let mut config = self.config.subscribe();
        let mut remaining = size.get();
        loop {
            let Some((bps, limiter)) = config.borrow_and_update().clone() else {
                return Ok(());
            };
            // Governor rejects requests larger than the bucket's burst capacity.
            let Some(batch) = NonZeroU32::new(remaining.min(bps.get())) else {
                return Ok(());
            };
            tokio::select! {
                biased;
                _ = config.changed() => {}
                result = limiter.until_n_ready(batch) => {
                    result?;
                    remaining -= batch.get();
                }
            }
        }
    }

    fn set(&self, limit: Option<NonZeroU32>) {
        self.config.send_if_modified(|current| {
            if current.as_ref().map(|(bps, _)| *bps) == limit {
                return false;
            }
            *current = Self::config(limit);
            true
        });
    }

    fn get(&self) -> Option<NonZeroU32> {
        self.config.borrow().as_ref().map(|(bps, _)| *bps)
    }
}

pub struct Limits {
    down: Limit,
    up: Limit,
}

impl Limits {
    pub fn new(config: LimitsConfig) -> Self {
        Self {
            down: Limit::new(config.download_bps),
            up: Limit::new(config.upload_bps),
        }
    }

    pub async fn prepare_for_upload(&self, len: NonZeroU32) -> crate::Result<()> {
        self.up.acquire(len).await
    }

    pub async fn prepare_for_download(&self, len: NonZeroU32) -> crate::Result<()> {
        self.down.acquire(len).await
    }

    pub fn set_upload_bps(&self, bps: Option<NonZeroU32>) {
        self.up.set(bps);
    }

    pub fn set_download_bps(&self, bps: Option<NonZeroU32>) {
        self.down.set(bps);
    }

    pub fn get_upload_bps(&self) -> Option<NonZeroU32> {
        self.up.get()
    }

    pub fn get_download_bps(&self) -> Option<NonZeroU32> {
        self.down.get()
    }

    pub fn get_config(&self) -> LimitsConfig {
        LimitsConfig {
            upload_bps: self.get_upload_bps(),
            download_bps: self.get_download_bps(),
        }
    }
}
