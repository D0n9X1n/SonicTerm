//! Clock seam so supervisor timing is provable without wall-clock waits.

use parking_lot::Mutex;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Source of monotonic time for the reaper supervisor.
pub trait Clock: Send + Sync + 'static {
    /// Return the current instant.
    fn now(&self) -> Instant;
}

/// Real monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[inline]
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Manually advanced clock.
///
/// Deadline and retry behavior is asserted by advancing time explicitly, so a
/// timing test cannot pass or fail on how loaded the machine happens to be.
#[derive(Clone)]
pub struct TestClock {
    now: Arc<Mutex<Instant>>,
}

impl TestClock {
    /// Create a clock anchored at the current instant.
    pub fn new() -> Self {
        Self { now: Arc::new(Mutex::new(Instant::now())) }
    }

    /// Move time forward.
    pub fn advance(&self, step: Duration) {
        let mut now = self.now.lock();
        *now = now.checked_add(step).expect("test clock overflow");
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.now.lock()
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod clock_tests;
