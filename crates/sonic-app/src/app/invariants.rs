//! Debug-only invariant probes for the concurrency land-mines documented in
//! CLAUDE.md §4.
//!
//! These helpers exist so that violations of the rules trip in tests rather
//! than as production deadlocks / CPU pin / dropped frames. All checks compile
//! out in release builds via `cfg(debug_assertions)`, so they impose zero
//! runtime cost on shipped binaries.
//!
//! Three invariants are guarded:
//!
//! 1. **Render path never blocks on the parser mutex.** §4 says the redraw
//!    handler MUST use `try_lock()`, never `lock()`. [`assert_render_try_lock`]
//!    accepts an `Option<Guard>` (the result of `try_lock`) and is the
//!    canonical render-path entry; calling the blocking-lock variant
//!    [`assert_render_lock_forbidden`] panics in debug.
//!
//! 2. **PTY-thread redraw coalescer respects a minimum interval.** §4 / PR #132
//!    bound consecutive redraw requests apart so the OS does not flag the app
//!    as unresponsive. [`RedrawCoalescerProbe`] is a debug-only tracker that
//!    panics if two consecutive `note_redraw()` calls land closer than the
//!    configured minimum.
//!
//! 3. **PTY burst flag is a monotonically-increasing generation counter.** §4
//!    / PR #162 replaced the original `bool input_dirty` with a `AtomicU32`
//!    that only ever goes up. [`debug_assert_burst_gen_monotonic`] verifies
//!    that a sampled generation value never went backwards.

use std::time::Duration;
#[cfg(debug_assertions)]
use std::time::Instant;

// ---------------------------------------------------------------------------
// 1. Render-path no-lock invariant
// ---------------------------------------------------------------------------

/// Asserts that the render path used `try_lock` (not `lock`) on the parser
/// mutex. `guard_result` is the value returned by `parking_lot::Mutex::try_lock`
/// — a `None` simply means "another thread holds the lock, skip this frame",
/// which is the expected fallback. This helper is a no-op in release builds.
///
/// The function deliberately accepts the `Option` so the render call sites
/// keep the existing pattern `match arc.try_lock() { Some(g) => ..., None =>
/// defer }` — calling this from a `lock()` call site is a type error because
/// `lock()` returns a guard, not an `Option`.
#[inline]
pub fn assert_render_try_lock<T>(_guard_result: &Option<parking_lot::MutexGuard<'_, T>>) {
    // The very existence of an `Option<Guard>` here is the proof — `lock()`
    // returns a `MutexGuard`, not an `Option`. This is the type-system half
    // of the invariant.
}

/// Panics in debug builds if the render path ever tries to take a blocking
/// `lock()` on the parser. Release builds allow the call (to preserve the old
/// behaviour for any non-render path that legitimately needs blocking access),
/// but in debug builds this is a hard error with a message pointing at
/// CLAUDE.md §4.
#[inline]
pub fn assert_render_lock_forbidden() {
    debug_assert!(
        false,
        "lock() called from render path is forbidden — use try_lock per CLAUDE.md §4 \
         (blocking lock() deadlocked the macOS main thread under shell-startup output bursts)"
    );
}

// ---------------------------------------------------------------------------
// 2. PTY-thread redraw coalescer interval invariant
// ---------------------------------------------------------------------------

/// Debug-only tracker that consecutive redraw requests issued from the VT
/// thread are spaced at least `min_interval` apart. The first call is treated
/// as a fresh start and never trips the assertion. Release builds skip the
/// timing math entirely.
///
/// Note: the production coalescer in `spawn_pane.rs` currently uses a 4 ms
/// floor (small enough to stay under one frame for echo latency); CLAUDE.md
/// §4 documents the original 16 ms guard. This probe is a generic utility
/// — call sites pass whichever interval they actually enforce so the probe
/// matches the implementation rather than the historical doc value.
#[derive(Debug, Default)]
pub struct RedrawCoalescerProbe {
    #[cfg(debug_assertions)]
    last: Option<Instant>,
}

impl RedrawCoalescerProbe {
    /// Construct a fresh probe.
    pub const fn new() -> Self {
        Self {
            #[cfg(debug_assertions)]
            last: None,
        }
    }

    /// Note that a redraw was issued. In debug builds, panics if this call is
    /// closer than `min_interval` to the previous one. In release builds, a
    /// no-op.
    #[inline]
    #[allow(unused_variables)]
    pub fn note_redraw(&mut self, min_interval: Duration) {
        #[cfg(debug_assertions)]
        {
            let now = Instant::now();
            if let Some(prev) = self.last {
                let elapsed = now.saturating_duration_since(prev);
                debug_assert!(
                    elapsed >= min_interval,
                    "PTY redraw coalescer fired too fast — interval {} µs < min {} µs \
                     (see CLAUDE.md §4 / spawn_pane.rs min_interval)",
                    elapsed.as_micros(),
                    min_interval.as_micros(),
                );
            }
            self.last = Some(now);
        }
    }
}

// ---------------------------------------------------------------------------
// 3. PTY burst generation counter monotonicity invariant
// ---------------------------------------------------------------------------

/// Debug-asserts that a newly observed burst-generation value is greater than
/// or equal to a previously observed one. PR #162 replaced the original
/// `bool input_dirty` with a monotonically-increasing `AtomicU32`; if anything
/// ever subtracts from or resets that counter, the renderer's
/// `pty_burst_snapshot != last_seen_burst_gen` test starts producing
/// false-positive misses again.
///
/// Release builds skip the check.
#[inline]
#[allow(unused_variables)]
pub fn debug_assert_burst_gen_monotonic(prev: u32, new: u32) {
    debug_assert!(
        new >= prev,
        "PTY burst generation counter went backwards: {prev} -> {new} \
         (see CLAUDE.md §4 / PR #162 — counter must be monotonic)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescer_first_call_never_trips() {
        let mut probe = RedrawCoalescerProbe::new();
        probe.note_redraw(Duration::from_millis(16));
    }

    #[test]
    fn coalescer_spaced_calls_pass() {
        let mut probe = RedrawCoalescerProbe::new();
        probe.note_redraw(Duration::from_micros(1));
        std::thread::sleep(Duration::from_millis(2));
        probe.note_redraw(Duration::from_millis(1));
    }

    #[test]
    fn burst_gen_equal_is_monotonic() {
        debug_assert_burst_gen_monotonic(5, 5);
    }

    #[test]
    fn burst_gen_increasing_is_monotonic() {
        debug_assert_burst_gen_monotonic(5, 6);
        debug_assert_burst_gen_monotonic(0, u32::MAX);
    }
}
