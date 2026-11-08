//! How long the reaper's synchronous teardown fallback blocked the event loop. The fallback runs
//! only when no teardown slot could be reserved; its native cleanup is bounded, but it happens on
//! the thread that retired the pane, so its time is recorded and a slow one is warned about.

use std::time::Duration;

/// A fallback longer than this is logged at `warn`.
pub(super) const SLOW_FALLBACK: Duration = Duration::from_millis(50);
/// Upper bounds of the blocked-time histogram's buckets, in microseconds; a last bucket holds the
/// rest. Powers of four from 1 µs to about 268 ms, so the 50 ms warning sits inside the range.
pub(crate) const FALLBACK_BUCKETS_US: [u64; 15] = [
    1,
    4,
    16,
    64,
    256,
    1_024,
    4_096,
    16_384,
    65_536,
    262_144,
    1_048_576,
    4_194_304,
    16_777_216,
    67_108_864,
    268_435_456,
];

/// What the synchronous fallback has cost so far.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FallbackTiming {
    /// Fallbacks run.
    pub(crate) count: u64,
    /// Fallbacks that blocked longer than [`SLOW_FALLBACK`].
    pub(crate) slow: u64,
    /// Total time blocked, in microseconds.
    pub(crate) total_us: u64,
    /// The longest one, in microseconds.
    pub(crate) longest_us: u64,
    /// How many fallbacks blocked for at most each [`FALLBACK_BUCKETS_US`] bound, the last entry
    /// counting those past every bound.
    pub(crate) histogram: [u64; FALLBACK_BUCKETS_US.len() + 1],
}

impl FallbackTiming {
    /// Add one fallback that blocked for `blocked`; returns whether it was slow.
    pub(super) fn record(&mut self, blocked: Duration) -> bool {
        let micros = u64::try_from(blocked.as_micros()).unwrap_or(u64::MAX);
        self.count += 1;
        self.total_us = self.total_us.saturating_add(micros);
        self.longest_us = self.longest_us.max(micros);
        let bucket = FALLBACK_BUCKETS_US.iter().position(|bound| micros <= *bound);
        self.histogram[bucket.unwrap_or(FALLBACK_BUCKETS_US.len())] += 1;
        let slow = blocked > SLOW_FALLBACK;
        self.slow += u64::from(slow);
        slow
    }
}

/// The warning a fallback that blocked for `blocked` raises, or `None` when it was not slow.
pub(super) fn slow_fallback_warning(blocked: Duration) -> Option<u64> {
    (blocked > SLOW_FALLBACK).then(|| u64::try_from(blocked.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "reaper_fallback_tests.rs"]
mod reaper_fallback_tests;
