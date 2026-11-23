use std::{
    num::NonZeroU64,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    sync::{Semaphore, watch},
    time::{self, Instant},
};

const MAX_BURST: u64 = 128 * 1024;

/// Shared token bucket with a runtime-adjustable rate in bytes per second.
#[derive(Debug, Clone)]
pub struct BandwidthLimit(Arc<Limit>);

#[derive(Debug)]
struct Limit {
    bucket: Mutex<Bucket>,
    limited: AtomicBool,
    changed: watch::Sender<()>,
    shared_turn: Semaphore,
}

#[derive(Debug)]
struct Bucket {
    rate: Option<NonZeroU64>,
    tokens: f64,
    updated: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum GrantWait {
    Shared(Duration),
    Scoped(Duration),
}

impl Default for BandwidthLimit {
    fn default() -> Self {
        Self::new(None)
    }
}

impl BandwidthLimit {
    pub fn new(rate: Option<NonZeroU64>) -> Self {
        Self(Arc::new(Limit {
            limited: AtomicBool::new(rate.is_some()),
            bucket: Mutex::new(Bucket {
                rate,
                tokens: capacity(rate) as f64,
                updated: Instant::now(),
            }),
            changed: watch::channel(()).0,
            shared_turn: Semaphore::new(1),
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
            self.0.limited.store(rate.is_some(), Ordering::Release);
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
        if requested == 0
            || [Some(self), Some(local), individual]
                .into_iter()
                .flatten()
                .all(|limit| !limit.0.limited.load(Ordering::Acquire))
        {
            return requested;
        }
        let mut limits = [Some(self), Some(local), individual];
        // A consistent address order avoids deadlocks when callers share a bucket.
        limits.sort_unstable_by_key(|limit| {
            limit.map_or(usize::MAX, |limit| Arc::as_ptr(&limit.0).addr())
        });
        let mut global_changed = self.0.changed.subscribe();
        let mut local_changed = local.0.changed.subscribe();
        let mut individual_changed = individual.map(|limit| limit.0.changed.subscribe());
        let mut turn = None;
        loop {
            if turn.is_none() && self.0.limited.load(Ordering::Acquire) {
                // Competing byte sizes must wait for the shared bucket in arrival order.
                turn = self.0.shared_turn.acquire().await.ok();
            }
            let wait = match self.try_acquire(limits, requested) {
                Ok(granted) => return granted,
                Err(GrantWait::Shared(wait)) => wait,
                Err(GrantWait::Scoped(wait)) => {
                    // A slower category or per-download limit must not hold up unrelated jobs.
                    drop(turn.take());
                    wait
                }
            };
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

    fn lock(&self) -> MutexGuard<'_, Bucket> {
        self.0
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn try_acquire(
        &self,
        limits: [Option<&Self>; 3],
        requested: usize,
    ) -> Result<usize, GrantWait> {
        let mut previous = None;
        let mut guards = limits.map(|limit| {
            let limit = limit?;
            let current = Arc::as_ptr(&limit.0);
            if previous == Some(current) {
                return None;
            }
            previous = Some(current);
            Some((Arc::ptr_eq(&self.0, &limit.0), limit.lock()))
        });
        debit(
            guards
                .iter_mut()
                .flatten()
                .map(|(shared, guard)| (*shared, &mut **guard)),
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
    limits: impl Iterator<Item = (bool, &'a mut Bucket)>,
    requested: usize,
    now: Instant,
) -> Result<usize, GrantWait> {
    let mut buckets = [None, None, None];
    for (slot, bucket) in buckets.iter_mut().zip(limits) {
        *slot = Some(bucket);
    }
    let mut count = requested;
    for (_, bucket) in buckets.iter_mut().flatten() {
        bucket.refill(now);
        count = count.min(capacity(bucket.rate));
    }
    let mut shared_wait = Duration::ZERO;
    let mut scoped_wait = Duration::ZERO;
    for (shared, bucket) in buckets.iter().flatten() {
        let wait = bucket.wait(count);
        if *shared {
            shared_wait = wait;
        } else {
            scoped_wait = scoped_wait.max(wait);
        }
    }
    if !scoped_wait.is_zero() {
        return Err(GrantWait::Scoped(scoped_wait));
    }
    if !shared_wait.is_zero() {
        return Err(GrantWait::Shared(shared_wait));
    }
    for (_, bucket) in buckets.iter_mut().flatten() {
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
            debit(
                [(true, &mut global), (false, &mut local)].into_iter(),
                30,
                start
            ),
            Err(GrantWait::Scoped(Duration::from_millis(100)))
        );
        assert_eq!(global.tokens, 100.0);
        let later = start + Duration::from_millis(100);
        assert_eq!(
            debit(
                [(true, &mut global), (false, &mut local)].into_iter(),
                30,
                later
            ),
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
    async fn applies_new_limits_after_unrestricted_transfers() {
        let global = BandwidthLimit::default();
        let local = BandwidthLimit::default();
        let individual = BandwidthLimit::default();

        assert_eq!(global.acquire(&local, Some(&individual), 500).await, 500);
        local.set_rate(NonZeroU64::new(30));
        assert_eq!(global.acquire(&local, Some(&individual), 500).await, 3);
        local.set_rate(None);
        individual.set_rate(NonZeroU64::new(20));
        assert_eq!(global.acquire(&local, Some(&individual), 500).await, 2);
        individual.set_rate(None);
        assert_eq!(global.acquire(&local, Some(&individual), 500).await, 500);
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

    #[tokio::test]
    async fn smaller_requests_cannot_overtake_a_waiting_shared_grant() {
        let global = BandwidthLimit::new(NonZeroU64::new(1_000));
        let local = BandwidthLimit::default();
        assert_eq!(global.acquire(&local, None, 100).await, 100);
        let large = global.acquire(&local, None, 100);
        tokio::pin!(large);
        assert!(
            time::timeout(Duration::from_millis(10), &mut large)
                .await
                .is_err()
        );
        assert!(
            time::timeout(Duration::from_millis(15), global.acquire(&local, None, 1))
                .await
                .is_err(),
            "a small grant overtook the waiter for a full block"
        );
        assert_eq!(
            time::timeout(Duration::from_secs(1), large).await.ok(),
            Some(100)
        );
    }

    #[tokio::test]
    async fn a_scoped_wait_releases_the_shared_turn() {
        let global = BandwidthLimit::new(NonZeroU64::new(1_000));
        let slow = BandwidthLimit::new(Some(NonZeroU64::MIN));
        let unlimited = BandwidthLimit::default();
        assert_eq!(global.acquire(&slow, None, 1).await, 1);
        let scoped = global.acquire(&slow, None, 1);
        tokio::pin!(scoped);
        assert!(
            time::timeout(Duration::from_millis(10), &mut scoped)
                .await
                .is_err()
        );
        assert_eq!(
            time::timeout(
                Duration::from_millis(100),
                global.acquire(&unlimited, None, 10)
            )
            .await
            .ok(),
            Some(10)
        );
    }

    #[tokio::test]
    async fn cancelling_a_shared_wait_releases_its_turn() {
        let global = BandwidthLimit::new(NonZeroU64::new(1_000));
        let local = BandwidthLimit::default();
        assert_eq!(global.acquire(&local, None, 100).await, 100);
        assert!(
            time::timeout(Duration::from_millis(10), global.acquire(&local, None, 100))
                .await
                .is_err()
        );
        assert_eq!(
            time::timeout(Duration::from_millis(50), global.acquire(&local, None, 1))
                .await
                .ok(),
            Some(1)
        );
    }
}
