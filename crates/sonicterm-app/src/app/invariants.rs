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
/// Note: the production coalescer in `spawn_pane.rs` currently uses a **3 ms**
/// floor (Epic #300 P3, down from the original 16 ms) plus a 128 KB byte
/// threshold for early flush. 3 ms is safe because the macOS "not responding"
/// beach ball is driven by *main-thread* blocking, not by how often a
/// *background* PTY thread posts redraw requests — the main thread coalesces
/// RedrawRequested via vsync (PR #132). wezterm ships the same 3 ms / 128 KB
/// combo. CLAUDE.md §4 documents the rationale. This probe is a generic
/// utility — call sites pass whichever interval they actually enforce so the
/// probe matches the implementation rather than the historical doc value.
#[derive(Debug, Default)]
pub struct RedrawCoalescerProbe {
    #[cfg(debug_assertions)]
    last: Option<Instant>,
}

/// Reason a redraw was flushed by the PTY-thread coalescer.
///
/// The coalescer in `spawn_pane.rs` has two legitimate flush triggers:
/// the interval timer elapsing (`Interval`), or the pending byte buffer
/// crossing the `FLUSH_BYTES` (128 KB) threshold (`Buffer`). Buffer-driven
/// flushes can legally fire faster than `min_interval` — under a heavy PTY
/// burst the 128 KB threshold can be hit in well under 3 ms, and the
/// renderer needs that data immediately. The probe therefore only enforces
/// the spacing rule for `Interval` flushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReason {
    /// Time-based flush — must respect the `min_interval` floor.
    Interval,
    /// Byte-threshold flush — may fire at any interval.
    Buffer,
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
    /// closer than `min_interval` to the previous one **and** the flush
    /// reason was [`FlushReason::Interval`]. Byte-threshold ([`FlushReason::Buffer`])
    /// flushes are always permitted regardless of spacing — a heavy PTY burst
    /// can legally drop > 128 KB into the buffer in well under 3 ms, and the
    /// whole point of the byte threshold is to keep the renderer fed in that
    /// case. In release builds, a no-op.
    #[inline]
    #[allow(unused_variables)]
    pub fn note_redraw(&mut self, min_interval: Duration, reason: FlushReason) {
        #[cfg(debug_assertions)]
        {
            let now = Instant::now();
            if matches!(reason, FlushReason::Interval) {
                if let Some(prev) = self.last {
                    let elapsed = now.saturating_duration_since(prev);
                    debug_assert!(
                        elapsed >= min_interval,
                        "PTY redraw coalescer fired Interval-flush too fast — \
                         interval {} µs < min {} µs (see CLAUDE.md §4 / \
                         spawn_pane.rs min_interval)",
                        elapsed.as_micros(),
                        min_interval.as_micros(),
                    );
                }
            }
            // Buffer-driven flushes are allowed at any interval — they exist
            // precisely to short-circuit the timer when a burst exceeds
            // FLUSH_BYTES.
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
        probe.note_redraw(Duration::from_millis(16), FlushReason::Interval);
    }

    #[test]
    fn coalescer_spaced_calls_pass() {
        let mut probe = RedrawCoalescerProbe::new();
        probe.note_redraw(Duration::from_micros(1), FlushReason::Interval);
        std::thread::sleep(Duration::from_millis(2));
        probe.note_redraw(Duration::from_millis(1), FlushReason::Interval);
    }

    #[test]
    fn coalescer_buffer_flush_does_not_panic_under_3ms() {
        // Buffer-threshold flushes (128 KB hit) MUST be allowed to fire at any
        // interval — including faster than min_interval. Regression guard for
        // the PR #308 review finding: heavy PTY bursts otherwise trip the
        // debug_assert on every flush.
        let mut probe = RedrawCoalescerProbe::new();
        probe.note_redraw(Duration::from_millis(3), FlushReason::Buffer);
        // 1 ms later (much less than the 3 ms min_interval), another buffer
        // flush — this must NOT panic.
        std::thread::sleep(Duration::from_millis(1));
        probe.note_redraw(Duration::from_millis(3), FlushReason::Buffer);
        // And a third one, also under the interval. Still must not panic.
        probe.note_redraw(Duration::from_millis(3), FlushReason::Buffer);
    }

    #[test]
    #[should_panic(expected = "PTY redraw coalescer fired Interval-flush too fast")]
    fn coalescer_interval_flush_under_3ms_panics() {
        // Two Interval-reason flushes < min_interval apart MUST trip the
        // debug_assert — that is the whole point of the probe. Use a wide
        // 10-second min_interval + zero sleep so the test is independent of
        // CI runner timer slop (macos-14 was tripping a false-pass with a
        // 3 ms min_interval + 1 ms sleep when the runner's sleep overran).
        let mut probe = RedrawCoalescerProbe::new();
        probe.note_redraw(Duration::from_secs(10), FlushReason::Interval);
        probe.note_redraw(Duration::from_secs(10), FlushReason::Interval);
    }

    #[test]
    fn coalescer_buffer_then_interval_respects_interval_spacing() {
        // After a buffer flush, the next interval flush still must respect the
        // spacing rule relative to the buffer flush — that prevents an
        // adversarial caller from "laundering" interval flushes through buffer
        // flushes.
        let mut probe = RedrawCoalescerProbe::new();
        probe.note_redraw(Duration::from_micros(1), FlushReason::Buffer);
        std::thread::sleep(Duration::from_millis(2));
        probe.note_redraw(Duration::from_millis(1), FlushReason::Interval);
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
