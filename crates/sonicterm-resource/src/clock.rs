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

    /// Wait until `deadline`, or return immediately if it has passed.
    ///
    /// A deferred task must not be re-polled before its own deadline, so the
    /// supervisor waits here rather than spinning the queue. A test clock jumps
    /// straight to the deadline, which keeps deferred work deterministic
    /// without sleeping.
    fn wait_until(&self, deadline: Instant);
}

/// Real monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[inline]
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait_until(&self, deadline: Instant) {
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
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

    fn wait_until(&self, deadline: Instant) {
        let mut now = self.now.lock();
        if deadline > *now {
            *now = deadline;
        }
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod clock_tests;
