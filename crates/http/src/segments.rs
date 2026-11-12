use std::{
    collections::{BTreeMap, VecDeque},
    num::{NonZeroU64, NonZeroUsize},
    ops::Range,
    time::Duration,
};

pub const DEFAULT_MIN_SEGMENT_SIZE: u64 = 1024 * 1024;

/// Identifies a single assignment. Released work receives a new ID when reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkId(u64);

/// An assignment with a half-open byte range. Work stealing may shorten its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    pub id: WorkId,
    pub range: Range<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SegmentError {
    #[error("segment is no longer assigned")]
    UnknownWork,
    #[error("progress exceeds the assigned range")]
    OutsideRange,
}

/// Allocates non-overlapping byte ranges. All offsets represent half-open intervals.
#[derive(Debug)]
pub struct SegmentAllocator {
    size: u64,
    minimum: u64,
    ready: VecDeque<Range<u64>>,
    active: BTreeMap<WorkId, Range<u64>>,
    completed: u64,
    next_id: u64,
}

impl SegmentAllocator {
    pub fn new(size: u64, connections: NonZeroUsize, minimum: NonZeroU64) -> Self {
        let minimum = minimum.get();
        let mut ready = VecDeque::new();
        if size > 0 {
            let count = (size / minimum).max(1).min(connections.get() as u64);
            let mut offset = 0;
            for index in 0..count {
                let end = offset + size / count + u64::from(index < size % count);
                ready.push_back(offset..end);
                offset = end;
            }
        }
        Self {
            size,
            minimum,
            ready,
            active: BTreeMap::new(),
            completed: 0,
            next_id: 0,
        }
    }

    /// Rebuilds assignments around the already synchronized ranges.
    pub(crate) fn restore(
        size: u64,
        connections: NonZeroUsize,
        minimum: NonZeroU64,
        completed: &[Range<u64>],
    ) -> Self {
        let mut allocator = Self::new(size, connections, minimum);
        if completed.is_empty() {
            return allocator;
        }
        allocator.ready.clear();
        let target = size.div_ceil(connections.get() as u64).max(minimum.get());
        let mut cursor = 0;
        let tail = size..size;
        for range in completed.iter().chain(std::iter::once(&tail)) {
            while cursor < range.start {
                let end = cursor.saturating_add(target).min(range.start);
                allocator.ready.push_back(cursor..end);
                cursor = end;
            }
            allocator.completed += range.end - range.start;
            cursor = range.end;
        }
        allocator
    }

    /// Assigns ready work or steals half of the largest remaining active range.
    pub fn assign(&mut self) -> Option<Work> {
        let id = WorkId(self.next_id);
        let next_id = self.next_id.checked_add(1)?;
        let range = self.ready.pop_front().or_else(|| self.steal())?;
        self.next_id = next_id;
        self.active.insert(id, range.clone());
        Some(Work { id, range })
    }

    fn steal(&mut self) -> Option<Range<u64>> {
        let largest = self
            .active
            .values_mut()
            .max_by_key(|range| range.end - range.start)?;
        let half = (largest.end - largest.start) / 2;
        if half < self.minimum {
            return None;
        }
        let midpoint = largest.start + half;
        let stolen = midpoint..largest.end;
        largest.end = midpoint;
        Some(stolen)
    }

    /// Returns the unfinished portion of a live assignment.
    pub fn remaining(&self, id: WorkId) -> Option<Range<u64>> {
        self.active.get(&id).cloned()
    }

    /// Records bytes from the beginning of the current unfinished portion.
    pub fn advance(&mut self, id: WorkId, bytes: u64) -> Result<(), SegmentError> {
        let range = self.active.get_mut(&id).ok_or(SegmentError::UnknownWork)?;
        if bytes > range.end - range.start {
            return Err(SegmentError::OutsideRange);
        }
        range.start += bytes;
        self.completed += bytes;
        Ok(())
    }

    /// Releases an assignment. Any uncompleted suffix goes back to the front of the queue.
    pub fn release(&mut self, id: WorkId) -> Result<(), SegmentError> {
        let range = self.active.remove(&id).ok_or(SegmentError::UnknownWork)?;
        if !range.is_empty() {
            self.ready.push_front(range);
        }
        Ok(())
    }

    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn is_complete(&self) -> bool {
        self.completed == self.size
    }
}

/// Adjusts one host's connection budget from measured aggregate throughput.
#[derive(Debug)]
pub struct ConnectionController {
    maximum: usize,
    limit: usize,
    previous: Option<(usize, f64)>,
    cooldown: u8,
}

impl ConnectionController {
    pub fn new(maximum: NonZeroUsize) -> Self {
        Self::with_initial(maximum, NonZeroUsize::MIN)
    }

    pub(crate) fn with_initial(maximum: NonZeroUsize, initial: NonZeroUsize) -> Self {
        Self {
            maximum: maximum.get(),
            limit: initial.min(maximum).get(),
            previous: None,
            cooldown: 0,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Samples throughput over the supplied interval. Slow start doubles the budget
    /// while the preceding increase improves throughput by more than 10 percent.
    pub fn observe(&mut self, bytes: u64, elapsed: Duration) -> usize {
        if elapsed.is_zero() {
            return self.limit;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return self.limit;
        }
        let rate = bytes as f64 / elapsed.as_secs_f64();
        if let Some((previous_limit, previous_rate)) = self.previous.take()
            && rate <= previous_rate * 1.1
        {
            self.limit = previous_limit;
            self.cooldown = 4;
            return self.limit;
        }
        if bytes > 0 && self.limit < self.maximum {
            self.previous = Some((self.limit, rate));
            self.limit = self.limit.saturating_mul(2).min(self.maximum);
        }
        self.limit
    }

    /// Reduces parallelism on connection resets and host-capacity responses such as 429 or 503.
    pub fn congested(&mut self) -> usize {
        self.limit = (self.limit / 2).max(1);
        self.previous = None;
        self.cooldown = 4;
        self.limit
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // These tests assert that generated settings and assignments are valid.
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn steals_from_slow_connection_after_the_other_finishes() -> Result<(), SegmentError> {
        let mut allocator = SegmentAllocator::new(
            100,
            NonZeroUsize::new(2).unwrap(),
            NonZeroU64::new(10).unwrap(),
        );
        let slow = allocator.assign().unwrap();
        let fast = allocator.assign().unwrap();
        assert_eq!(slow.range, 0..50);
        assert_eq!(fast.range, 50..100);
        allocator.advance(slow.id, 10)?;
        allocator.advance(fast.id, 50)?;
        allocator.release(fast.id)?;
        let stolen = allocator.assign().unwrap();
        assert_eq!(stolen.range, 30..50);
        assert_eq!(allocator.remaining(slow.id), Some(10..30));
        assert_eq!(
            allocator.advance(slow.id, 21),
            Err(SegmentError::OutsideRange)
        );
        assert_eq!(allocator.completed(), 60);
        Ok(())
    }

    #[test]
    fn respects_minimum_and_returns_failed_suffix() -> Result<(), SegmentError> {
        let mut allocator = SegmentAllocator::new(
            30,
            NonZeroUsize::new(8).unwrap(),
            NonZeroU64::new(11).unwrap(),
        );
        let first = allocator.assign().unwrap();
        let second = allocator.assign().unwrap();
        assert_eq!(first.range, 0..15);
        assert_eq!(second.range, 15..30);
        assert!(allocator.assign().is_none());
        allocator.advance(first.id, 9)?;
        allocator.release(first.id)?;
        let retry = allocator.assign().unwrap();
        assert_eq!(retry.range, 9..15);
        assert_ne!(retry.id, first.id);
        Ok(())
    }

    #[test]
    fn slows_down_on_429_and_rejects_unproductive_increases() {
        let mut controller = ConnectionController::new(NonZeroUsize::new(4).unwrap());
        let interval = Duration::from_secs(1);
        assert_eq!(controller.limit(), 1);
        assert_eq!(controller.observe(100, interval), 2);
        assert_eq!(controller.observe(130, interval), 4);
        assert_eq!(controller.observe(138, interval), 2);
        assert_eq!(controller.congested(), 1);
        for _ in 0..4 {
            assert_eq!(controller.observe(100, interval), 1);
        }
        assert_eq!(controller.observe(100, interval), 2);
    }

    proptest! {
        #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

        #[test]
        fn assignments_preserve_coverage_and_monotonic_progress(
            size in 0u64..500,
            connections in 1usize..12,
            minimum in 1u64..64,
            actions in prop::collection::vec((0u8..3, 0usize..64, 0u64..128), 0..180),
        ) {
            let mut allocator = SegmentAllocator::new(size, NonZeroUsize::new(connections).unwrap(), NonZeroU64::new(minimum).unwrap());
            let mut active = Vec::new();
            let mut done = vec![false; size as usize];
            let mut previous = 0;
            for (action, index, bytes) in actions {
                if action == 0 || active.is_empty() {
                    if let Some(work) = allocator.assign() {
                        active.push(work.id);
                    }
                } else {
                    let index = index % active.len();
                    let id = active[index];
                    if action == 1 {
                        let range = allocator.remaining(id).unwrap();
                        let end = range.end.min(range.start + bytes);
                        allocator.advance(id, end - range.start).unwrap();
                        for position in range.start..end {
                            prop_assert!(!done[position as usize]);
                            done[position as usize] = true;
                        }
                    } else {
                        allocator.release(id).unwrap();
                        active.swap_remove(index);
                    }
                }
                prop_assert!(allocator.completed() >= previous);
                previous = allocator.completed();
                let mut covered = done.clone();
                for range in allocator.ready.iter().chain(allocator.active.values()) {
                    for position in range.clone() {
                        prop_assert!(!covered[position as usize]);
                        covered[position as usize] = true;
                    }
                }
                prop_assert!(covered.into_iter().all(|covered| covered));
                prop_assert_eq!(previous as usize, done.iter().filter(|&&byte| byte).count());
            }
        }
    }
}
