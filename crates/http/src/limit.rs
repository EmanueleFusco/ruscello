use std::{
    num::NonZeroU64,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use tokio::{
    sync::watch,
    time::{self, Instant},
};

const MAX_BURST: u64 = 128 * 1024;

/// Shared token bucket with a runtime-adjustable rate in bytes per second.
#[derive(Debug, Clone)]
pub struct BandwidthLimit(Arc<Limit>);

#[derive(Debug)]
struct Limit {
    bucket: Mutex<Bucket>,
    changed: watch::Sender<()>,
}

#[derive(Debug)]
struct Bucket {
    rate: Option<NonZeroU64>,
    tokens: f64,
    updated: Instant,
}

impl Default for BandwidthLimit {
    fn default() -> Self {
        Self::new(None)
    }
}

impl BandwidthLimit {
    pub fn new(rate: Option<NonZeroU64>) -> Self {
        Self(Arc::new(Limit {
            bucket: Mutex::new(Bucket {
                rate,
                tokens: capacity(rate) as f64,
                updated: Instant::now(),
            }),
            changed: watch::channel(()).0,
        }))
    }

    /// Sets a new rate or removes the limit. Existing waits are recalculated immediately.
    pub fn set_rate(&self, rate: Option<NonZeroU64>) {
        let mut bucket = self.lock();
        if bucket.rate != rate {
            bucket.refill(Instant::now());
            let cap = capacity(rate) as f64;
            bucket.tokens = if bucket.rate.is_none() {
                cap
            } else {
                bucket.tokens.min(cap)
            };
            bucket.rate = rate;
            self.0.changed.send_replace(());
        }
    }

    /// Atomically debits both buckets. Locks are held only for the accounting operation.
    pub(crate) async fn acquire(&self, local: &Self, requested: usize) -> usize {
        if requested == 0 {
            return 0;
        }
        if let Ok(granted) = self.try_acquire(local, requested, Instant::now()) {
            return granted;
        }
        let mut global_changed = self.0.changed.subscribe();
        let mut local_changed = local.0.changed.subscribe();
        loop {
            match self.try_acquire(local, requested, Instant::now()) {
                Ok(granted) => return granted,
                Err(wait) => {
                    tokio::select! {
                        _ = time::sleep(wait) => {}
                        _ = global_changed.changed() => {}
                        _ = local_changed.changed() => {}
                    }
                }
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, Bucket> {
        self.0
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn try_acquire(&self, local: &Self, requested: usize, now: Instant) -> Result<usize, Duration> {
        if Arc::ptr_eq(&self.0, &local.0) {
            return debit(&mut self.lock(), None, requested, now);
        }
        // Address order also protects callers that reuse the buckets in reverse roles.
        if Arc::as_ptr(&self.0).addr() < Arc::as_ptr(&local.0).addr() {
            let mut first = self.lock();
            debit(&mut first, Some(&mut local.lock()), requested, now)
        } else {
            let mut first = local.lock();
            debit(&mut first, Some(&mut self.lock()), requested, now)
        }
    }
}

fn capacity(rate: Option<NonZeroU64>) -> usize {
    rate.map_or(usize::MAX, |rate| {
        (rate.get() / 10).clamp(1, MAX_BURST) as usize
    })
}

impl Bucket {
    fn refill(&mut self, now: Instant) {
        if let Some(rate) = self.rate {
            let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
            self.tokens =
                (self.tokens + elapsed * rate.get() as f64).min(capacity(self.rate) as f64);
        }
        self.updated = self.updated.max(now);
    }

    fn wait(&self, count: usize) -> Duration {
        self.rate.map_or(Duration::ZERO, |rate| {
            Duration::from_secs_f64((count as f64 - self.tokens).max(0.0) / rate.get() as f64)
        })
    }

    fn spend(&mut self, count: usize) {
        if self.rate.is_some() {
            self.tokens = (self.tokens - count as f64).max(0.0);
        }
    }
}

fn debit(
    first: &mut Bucket,
    second: Option<&mut Bucket>,
    requested: usize,
    now: Instant,
) -> Result<usize, Duration> {
    first.refill(now);
    let mut count = requested.min(capacity(first.rate));
    let mut second = second;
    if let Some(other) = &mut second {
        other.refill(now);
        count = count.min(capacity(other.rate));
    }
    let wait = first.wait(count).max(
        second
            .as_ref()
            .map_or(Duration::ZERO, |other| other.wait(count)),
    );
    if !wait.is_zero() {
        return Err(wait);
    }
    first.spend(count);
    if let Some(other) = second {
        other.spend(count);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_both_limits_and_debits_only_when_both_are_ready() {
        let start = Instant::now();
        let mut global = Bucket {
            rate: NonZeroU64::new(1000),
            tokens: 100.0,
            updated: start,
        };
        let mut local = Bucket {
            rate: NonZeroU64::new(200),
            tokens: 0.0,
            updated: start,
        };
        assert_eq!(
            debit(&mut global, Some(&mut local), 30, start),
            Err(Duration::from_millis(100))
        );
        assert_eq!(global.tokens, 100.0);
        let later = start + Duration::from_millis(100);
        assert_eq!(debit(&mut global, Some(&mut local), 30, later), Ok(20));
        assert_eq!(global.tokens, 80.0);
        assert_eq!(local.tokens, 0.0);
    }

    #[tokio::test]
    async fn reconfiguration_wakes_waiting_workers() {
        let global = BandwidthLimit::new(NonZeroU64::new(1));
        let local = BandwidthLimit::default();
        assert_eq!(global.acquire(&local, 500).await, 1);
        let pending = global.acquire(&local, 500);
        tokio::pin!(pending);
        assert!(
            time::timeout(Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        global.set_rate(None);
        assert_eq!(
            time::timeout(Duration::from_secs(1), pending).await.ok(),
            Some(500)
        );
    }
}
