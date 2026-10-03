//! The grid-scan throttle: at most one scan per interval, plus one trailing check.
//!
//! The probe looks for a sentinel, the typing prompt or an image in dispatches the App already
//! receives. Scans are throttled; a throttled scan in a dispatch the App received leaves one
//! trailing check, so the last output before the App goes idle is still looked at.

use std::time::{Duration, Instant};

/// What gave the probe a chance to scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScanTrigger {
    /// A dispatch the App received from outside the harness: a native window event, a redraw or
    /// a user event such as PTY output. New output reaches the grid this way.
    App,
    /// A loop turn (`new_events`, `about_to_wait`) or a dispatch the probe makes itself. The
    /// harness's own timer can cause it, so it may scan but never arms a trailing check.
    Harness,
}

/// Throttles grid scans to one per `interval`, with at most one trailing check.
#[derive(Debug)]
pub(crate) struct ScanThrottle {
    interval: Duration,
    last: Option<Instant>,
    trailing: Option<Instant>,
}

impl ScanThrottle {
    /// A throttle that has never scanned.
    pub(crate) fn new(interval: Duration) -> Self {
        Self { interval, last: None, trailing: None }
    }

    /// Whether to scan at `now`. `wanted` is false once nothing is awaited, which drops any
    /// trailing check.
    pub(crate) fn observe(&mut self, now: Instant, wanted: bool, trigger: ScanTrigger) -> bool {
        if !wanted {
            self.trailing = None;
            return false;
        }
        if self.last.is_none_or(|last| now.saturating_duration_since(last) >= self.interval) {
            self.last = Some(now);
            self.trailing = None;
            return true;
        }
        if trigger == ScanTrigger::App && self.trailing.is_none() {
            // Only a dispatch the App received can carry output the throttled scan missed. A
            // harness wake, the trailing check's own included, never schedules another check.
            self.trailing = self.last.map(|last| last + self.interval);
        }
        false
    }

    /// When the armed trailing check is due, if one is armed.
    pub(crate) fn trailing(&self) -> Option<Instant> {
        self.trailing
    }
}

#[cfg(test)]
#[path = "scan_throttle_tests.rs"]
mod scan_throttle_tests;
