//! History ring buffer + rate tracking with an injectable clock, so tests
//! run on a fake clock (the C++ reference used caller-supplied clocks for
//! the same reason: deterministic eviction and delta tests).

use std::collections::VecDeque;

/// Fixed-capacity history of the newest `capacity` samples.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    cap: usize,
    items: VecDeque<T>,
}

impl<T: Clone> Ring<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring capacity must be > 0");
        Self {
            cap: capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, item: T) {
        if self.items.len() == self.cap {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Oldest → newest.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    pub fn latest(&self) -> Option<&T> {
        self.items.back()
    }

    pub fn min(&self) -> Option<&T>
    where
        T: Ord,
    {
        self.items.iter().min()
    }

    pub fn max(&self) -> Option<&T>
    where
        T: Ord,
    {
        self.items.iter().max()
    }
}

/// Monotonic counter that can wrap (u64 kernel jiffies/bytes counters) or
/// reset (counter went backwards more than a plausible wrap — e.g. the
/// producing subsystem restarted). Call `sample()` once per tick; the
/// first sample only seeds the baseline.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counter {
    last: Option<u64>,
}

/// Outcome of feeding a new counter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterDelta {
    /// First observation; no rate yet.
    Seed,
    /// Normal forward progress (includes the zero-delta case).
    Delta(u64),
    /// Value went backwards: either the counter passed u64::MAX between
    /// ticks, or the producing subsystem restarted and re-baselined near
    /// zero. From two samples these are indistinguishable — a large
    /// u64 value followed by a small one is exactly what both look like.
    /// We re-baseline and report no rate this tick (the safe reading:
    /// claiming a wrap when the source restarted would fabricate a giant
    /// spike in every graph). Callers that want to tell the two apart can
    /// observe `new` itself: near zero suggests a restart.
    Reset,
}

impl Counter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, now: u64) -> CounterDelta {
        match self.last {
            None => {
                self.last = Some(now);
                CounterDelta::Seed
            }
            Some(prev) => {
                self.last = Some(now);
                if now >= prev {
                    CounterDelta::Delta(now - prev)
                } else {
                    CounterDelta::Reset
                }
            }
        }
    }

    pub fn last(&self) -> Option<u64> {
        self.last
    }
}

/// Tracks a value over time with a clock function, emitting
/// `value_delta / time_delta` — the scrolling graphs' data source.
/// `clock` returns microseconds; keeping it generic lets tests inject a
/// fake clock.
#[derive(Debug, Clone)]
pub struct RateTracker {
    counter: Counter,
    last_us: Option<u64>,
}

/// Rate result for one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rate {
    /// Cannot compute yet (seed tick or counter reset).
    Unknown,
    /// Units per second.
    PerSec(f64),
}

impl RateTracker {
    pub fn new() -> Self {
        Self {
            counter: Counter::new(),
            last_us: None,
        }
    }

    /// Feed `(value, clock_us)`; returns units/second or Unknown.
    pub fn sample(&mut self, value: u64, now_us: u64) -> Rate {
        let d = self.counter.sample(value);
        let elapsed = match self.last_us {
            Some(t) => now_us.saturating_sub(t),
            None => 0,
        };
        self.last_us = Some(now_us);
        match d {
            CounterDelta::Seed | CounterDelta::Reset => Rate::Unknown,
            CounterDelta::Delta(0) => Rate::Unknown,
            CounterDelta::Delta(dd) => rate_over(dd, elapsed),
        }
    }
}

fn rate_over(delta: u64, elapsed_us: u64) -> Rate {
    if elapsed_us == 0 {
        Rate::Unknown
    } else {
        Rate::PerSec(delta as f64 * 1_000_000.0 / elapsed_us as f64)
    }
}

impl Default for RateTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest() {
        let mut r = Ring::new(3);
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(r.latest(), Some(&4));
        assert_eq!(r.min(), Some(&2));
        assert_eq!(r.max(), Some(&4));
    }

    #[test]
    fn ring_empty_and_fill() {
        let mut r = Ring::<u32>::new(4);
        assert!(r.is_empty());
        assert_eq!(r.latest(), None);
        r.push(7);
        assert_eq!(r.latest(), Some(&7));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn counter_forward_and_seed() {
        let mut c = Counter::new();
        assert_eq!(c.sample(100), CounterDelta::Seed);
        assert_eq!(c.sample(150), CounterDelta::Delta(50));
        assert_eq!(c.sample(150), CounterDelta::Delta(0));
    }

    #[test]
    fn counter_wrap_and_reset_rebaseline() {
        // A u64 wrap and a subsystem restart are indistinguishable from
        // two samples; both must re-baseline (Reset) rather than fabricate
        // a rate. The tick after either is measured from the new baseline.
        let mut c = Counter::new();
        c.sample(u64::MAX - 10);
        assert_eq!(c.sample(4), CounterDelta::Reset); // looks wrapped
        assert_eq!(c.sample(1000), CounterDelta::Delta(996));
        assert_eq!(c.sample(3), CounterDelta::Reset); // looks restarted
        assert_eq!(c.sample(50), CounterDelta::Delta(47));
    }

    #[test]
    fn rate_rebaselines_after_backwards_step() {
        let mut rt = RateTracker::new();
        rt.sample(u64::MAX - 500, 1_000_000);
        assert_eq!(rt.sample(500, 1_500_000), Rate::Unknown); // Reset tick
        match rt.sample(1500, 2_000_000) {
            // 1000 units in 0.5 s from the new baseline
            Rate::PerSec(v) => assert!((v - 2000.0).abs() < 1e-6),
            other => panic!("expected rate, got {other:?}"),
        }
    }

    #[test]
    fn rate_math_on_fake_clock() {
        let mut rt = RateTracker::new();
        assert_eq!(rt.sample(0, 1_000_000), Rate::Unknown); // seed
        // 500 units in 0.5 s -> 1000/s
        match rt.sample(500, 1_500_000) {
            Rate::PerSec(v) => assert!((v - 1000.0).abs() < 1e-9),
            other => panic!("expected rate, got {other:?}"),
        }
        // backwards step re-baselines: that tick Unknown, next tick OK
        assert_eq!(rt.sample(10, 2_000_000), Rate::Unknown); // reset
        match rt.sample(60, 2_500_000) {
            Rate::PerSec(v) => assert!((v - 100.0).abs() < 1e-9),
            other => panic!("expected rate, got {other:?}"),
        }
    }

    #[test]
    fn rate_zero_elapsed_is_unknown() {
        let mut rt = RateTracker::new();
        rt.sample(0, 500);
        assert_eq!(rt.sample(123, 500), Rate::Unknown); // clock did not advance
    }
}
