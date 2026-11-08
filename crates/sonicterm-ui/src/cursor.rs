//! Cursor shape and blink timing.
//!
//! The blink is two-phase: the cursor is drawn at exactly its theme colour, or not at all, so a
//! yellow theme never reads as olive mid-fade. It runs only for a while after activity and then
//! rests solid, so an idle window schedules no blink frames. GPU-free, so the timing is unit-tested
//! without a surface; the app owns the clock and the renderer receives only the sampled phase.

use std::time::{Duration, Instant};

pub use sonicterm_cfg::config::CursorShape;

/// One phase, on or off: half of the xterm and GTK 1,060 ms blink cycle.
pub const BLINK_PHASE: Duration = Duration::from_millis(530);

/// How long after the last activity the cursor keeps blinking before it rests solid.
pub const BLINK_REST_AFTER: Duration = Duration::from_secs(15);

/// Whether a cursor blinking since `epoch` is drawn at `now`. The first phase after `epoch` is on,
/// so activity always shows the cursor at once, and from [`BLINK_REST_AFTER`] it stays on.
pub fn blink_on(epoch: Instant, now: Instant) -> bool {
    let elapsed = now.saturating_duration_since(epoch);
    if elapsed >= BLINK_REST_AFTER {
        // When: `elapsed` reached the rest, the cursor stays solid until the next activity.
        return true;
    }
    (elapsed.as_millis() / BLINK_PHASE.as_millis()).is_multiple_of(2)
}

/// When the next blink frame is due for a cursor blinking since `epoch`, or `None` once it rests.
/// The last boundary is the rest itself, so a rest that begins in an off phase still gets the frame
/// that shows the cursor again.
pub fn next_blink_deadline(epoch: Instant, now: Instant) -> Option<Instant> {
    let elapsed = now.saturating_duration_since(epoch);
    if elapsed >= BLINK_REST_AFTER {
        // When: `elapsed` reached the rest, no further blink frame is due.
        return None;
    }
    let phase_ms = BLINK_PHASE.as_millis();
    let next_boundary_ms = (elapsed.as_millis() / phase_ms + 1) * phase_ms;
    let next_boundary = Duration::from_millis(next_boundary_ms as u64).min(BLINK_REST_AFTER);
    Some(epoch + next_boundary)
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod cursor_tests;
