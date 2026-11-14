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

    /// Atomically debits all applicable buckets.
    pub async fn acquire(
        &self,
        local: &Self,
        individual: Option<&Self>,
        requested: usize,
    ) -> usize {
        if requested == 0 {
            return 0;
        }
        let mut limits = [Some(self), Some(local), individual];
        // A consistent address order avoids deadlocks when callers share a bucket.
        limits.sort_unstable_by_key(|limit| {
            limit.map_or(usize::MAX, |limit| Arc::as_ptr(&limit.0).addr())
        });
        if let Ok(granted) = Self::try_acquire(limits, requested) {
            return granted;
        }
        let mut global_changed = self.0.changed.subscribe();
        let mut local_changed = local.0.changed.subscribe();
        let mut individual_changed = individual.map(|limit| limit.0.changed.subscribe());
        loop {
            match Self::try_acquire(limits, requested) {
                Ok(granted) => return granted,
                Err(wait) => {
                    tokio::select! {
                        _ = time::sleep(wait) => {}
                        _ = global_changed.changed() => {}
                        _ = local_changed.changed() => {}
                        () = async {
                            if let Some(receiver) = &mut individual_changed { let _ = receiver.changed().await; }
                            else { std::future::pending::<()>().await; }
                        } => {}
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

    fn try_acquire(limits: [Option<&Self>; 3], requested: usize) -> Result<usize, Duration> {
        let mut previous = None;
        let mut guards = limits.map(|limit| {
            let limit = limit?;
            let current = Arc::as_ptr(&limit.0);
            if previous == Some(current) {
                return None;
            }
            previous = Some(current);
            Some(limit.lock())
        });
        debit(
            guards.iter_mut().filter_map(Option::as_deref_mut),
            requested,
            Instant::now(),
        )
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

fn debit<'a>(
    limits: impl Iterator<Item = &'a mut Bucket>,
    requested: usize,
    now: Instant,
) -> Result<usize, Duration> {
    let mut buckets: [Option<&mut Bucket>; 3] = [None, None, None];
    for (slot, bucket) in buckets.iter_mut().zip(limits) {
        *slot = Some(bucket);
    }
    let mut count = requested;
    for bucket in buckets.iter_mut().flatten() {
        bucket.refill(now);
        count = count.min(capacity(bucket.rate));
    }
    let wait = buckets
        .iter()
        .flatten()
        .map(|bucket| bucket.wait(count))
        .max()
        .unwrap_or_default();
    if !wait.is_zero() {
        return Err(wait);
    }
    for bucket in buckets.iter_mut().flatten() {
        bucket.spend(count);
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
            debit([&mut global, &mut local].into_iter(), 30, start),
            Err(Duration::from_millis(100))
        );
        assert_eq!(global.tokens, 100.0);
        let later = start + Duration::from_millis(100);
        assert_eq!(
            debit([&mut global, &mut local].into_iter(), 30, later),
            Ok(20)
        );
        assert_eq!(global.tokens, 80.0);
        assert_eq!(local.tokens, 0.0);
    }

    #[tokio::test]
    async fn applies_the_individual_limit_without_double_debiting_a_shared_bucket() {
        let shared = BandwidthLimit::new(NonZeroU64::new(1000));
        let individual = BandwidthLimit::new(NonZeroU64::new(100));
        assert_eq!(shared.acquire(&shared, Some(&individual), 100).await, 10);
        assert!(shared.lock().tokens >= 90.0);
        assert!(individual.lock().tokens < 1.0);
    }

    #[tokio::test]
    async fn reconfiguration_wakes_waiting_workers() {
        let global = BandwidthLimit::new(NonZeroU64::new(1));
        let local = BandwidthLimit::default();
        assert_eq!(global.acquire(&local, None, 500).await, 1);
        let pending = global.acquire(&local, None, 500);
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
