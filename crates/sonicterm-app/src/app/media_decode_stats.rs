//! Inline-image decode timing on the VT worker, recorded only while the frame counters are on.
//!
//! PNG decode and resize run synchronously on a pane's VT worker, so an image burst can delay that
//! pane's text. Every decode **attempt** is timed, whatever its outcome: Sixel, iTerm2 and kitty
//! transmits and queries, malformed payloads, and decodes a store refused afterwards. Kitty place
//! and delete never decode, so they record nothing. The counts decide the 8 ms investigation rule
//! exactly: p95 is at least 8 ms when fewer than `ceil(0.95 × attempts)` attempts took under it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::frame_counters::{AtomicHistogram, HistogramUnit};

/// The decode duration below which an attempt counts as under the investigation threshold.
pub(crate) const THRESHOLD_US: u64 = 8_000;

/// How one decode attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeOutcome {
    /// It produced an image that was kept, or a kitty query that answered `OK`.
    Decoded,
    /// The payload did not decode.
    Failed,
    /// It decoded, then the store refused the image.
    RefusedAfterDecode,
}

/// Every pane's decode attempts, App-wide.
#[derive(Debug)]
pub(crate) struct DecodeStats {
    /// Decode durations.
    pub(crate) duration: AtomicHistogram,
    /// Decode attempts.
    pub(crate) attempts: AtomicU64,
    /// Attempts that took under [`THRESHOLD_US`].
    pub(crate) under_threshold: AtomicU64,
    /// The longest attempt, in microseconds.
    pub(crate) max_us: AtomicU64,
    /// Attempts that ended [`DecodeOutcome::Decoded`].
    pub(crate) decoded: AtomicU64,
    /// Attempts that ended [`DecodeOutcome::Failed`].
    pub(crate) failed: AtomicU64,
    /// Attempts that ended [`DecodeOutcome::RefusedAfterDecode`].
    pub(crate) refused_after_decode: AtomicU64,
}

impl Default for DecodeStats {
    fn default() -> Self {
        Self {
            duration: AtomicHistogram::new(HistogramUnit::Decode),
            attempts: AtomicU64::new(0),
            under_threshold: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            refused_after_decode: AtomicU64::new(0),
        }
    }
}

impl DecodeStats {
    /// Record one attempt that took `elapsed_us` and ended `outcome`.
    // Ordering: attempts, under_threshold, max_us and the outcome tally are Relaxed; each is an
    // independent count a snapshot reads, never a protocol.
    pub(crate) fn record(&self, elapsed_us: u64, outcome: DecodeOutcome) {
        self.duration.record_us(elapsed_us);
        self.attempts.fetch_add(1, Ordering::Relaxed);
        // Attempts under the threshold count toward the exact p95 rule.
        self.under_threshold.fetch_add(u64::from(elapsed_us < THRESHOLD_US), Ordering::Relaxed);
        self.max_us.fetch_max(elapsed_us, Ordering::Relaxed);
        let tally = match outcome {
            DecodeOutcome::Decoded => &self.decoded,
            DecodeOutcome::Failed => &self.failed,
            DecodeOutcome::RefusedAfterDecode => &self.refused_after_decode,
        };
        tally.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether the nearest-rank p95 of the recorded attempts is at least the threshold, the rule a
    /// comparison applies to the `media_decode_attempts` and `media_decode_under_8ms` counts.
    // Ordering: attempts and under_threshold Relaxed; tests read them after recording stops.
    #[cfg(test)]
    pub(crate) fn p95_at_least_threshold(&self) -> bool {
        p95_at_least_threshold(
            self.attempts.load(Ordering::Relaxed),
            self.under_threshold.load(Ordering::Relaxed),
        )
    }
}

/// Whether `attempts` decodes, of which `under` took under [`THRESHOLD_US`], have a nearest-rank
/// p95 at or above it: fewer than `ceil(0.95 × attempts)` were under it.
#[cfg(test)]
pub(crate) fn p95_at_least_threshold(attempts: u64, under: u64) -> bool {
    attempts > 0 && under < (attempts * 95).div_ceil(100)
}

/// Times decode attempts on one worker when the counters are on; reads no clock when they are off.
pub(crate) struct DecodeTimer<'stats, Now: FnMut() -> Instant> {
    /// The App's stats, or `None` while the counters are off.
    stats: Option<&'stats DecodeStats>,
    /// The worker's clock.
    now: Now,
}

impl<'stats, Now: FnMut() -> Instant> DecodeTimer<'stats, Now> {
    /// A timer recording into `stats`, reading `now`.
    pub(crate) fn new(stats: Option<&'stats DecodeStats>, now: Now) -> Self {
        Self { stats, now }
    }

    /// Run `decode` and return its result with the attempt's elapsed microseconds; `None` for the
    /// time while the counters are off, when no clock is read.
    pub(crate) fn time<Value>(&mut self, decode: impl FnOnce() -> Value) -> (Value, Option<u64>) {
        let Some(_) = self.stats else {
            // When: stats is None, the counters are off, so the decode runs untimed.
            return (decode(), None);
        };
        let started = (self.now)();
        let value = decode();
        let elapsed = (self.now)().saturating_duration_since(started);
        (value, Some(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)))
    }

    /// Record an attempt timed by [`Self::time`] once its outcome is known.
    pub(crate) fn record(&self, elapsed_us: Option<u64>, outcome: DecodeOutcome) {
        if let (Some(stats), Some(elapsed_us)) = (self.stats, elapsed_us) {
            stats.record(elapsed_us, outcome);
        }
    }
}

#[cfg(test)]
#[path = "media_decode_stats_tests.rs"]
mod media_decode_stats_tests;
