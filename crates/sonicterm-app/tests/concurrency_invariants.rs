//! Regression tests for the debug-only concurrency invariant probes added
//! alongside CLAUDE.md §4 land-mines. Each test constructs a scenario that
//! *violates* one invariant and asserts the corresponding `debug_assert!`
//! fires. The tests are debug-build-only because `debug_assert!` compiles
//! out in release.

#![cfg(debug_assertions)]

use std::time::Duration;

use sonicterm_app::app::invariants::{
    assert_render_lock_forbidden, debug_assert_burst_gen_monotonic, FlushReason,
    RedrawCoalescerProbe,
};

/// §4 land-mine 1: the render path must never use blocking `lock()`. Calling
/// the canary directly is the moral equivalent of a render-path `lock()` —
/// the assertion must fire in debug.
#[test]
#[should_panic(expected = "lock() called from render path is forbidden")]
fn render_path_blocking_lock_trips_invariant() {
    assert_render_lock_forbidden();
}

/// §4 land-mine 2: consecutive redraw requests inside the VT-thread
/// coalescer loop must be spaced apart by at least the configured min
/// interval. Two back-to-back `note_redraw` calls with a generous min must
/// trip the probe.
#[test]
#[should_panic(expected = "PTY redraw coalescer fired Interval-flush too fast")]
fn redraw_coalescer_back_to_back_trips_invariant() {
    let mut probe = RedrawCoalescerProbe::new();
    // First call seeds the probe and never trips.
    probe.note_redraw(Duration::from_secs(10), FlushReason::Interval);
    // Second call lands microseconds later — way below the 10 s floor.
    probe.note_redraw(Duration::from_secs(10), FlushReason::Interval);
}

/// PR #308 follow-up: byte-threshold (`FlushReason::Buffer`) flushes are an
/// explicit second valid flush trigger and MUST be allowed to fire faster
/// than `min_interval` — otherwise the debug_assert panics on every heavy
/// PTY burst (the 128 KB threshold can be hit in well under 3 ms).
#[test]
fn redraw_coalescer_buffer_flush_under_min_interval_is_ok() {
    let mut probe = RedrawCoalescerProbe::new();
    probe.note_redraw(Duration::from_secs(10), FlushReason::Buffer);
    // Immediately again — must NOT panic.
    probe.note_redraw(Duration::from_secs(10), FlushReason::Buffer);
}

/// §4 land-mine 3 / PR #162: the PTY burst-generation counter must be
/// monotonically non-decreasing. A backwards transition (e.g. someone
/// reverted to `bool` semantics + reset on render) must trip the probe.
#[test]
#[should_panic(expected = "PTY burst generation counter went backwards")]
fn burst_gen_backwards_trips_invariant() {
    debug_assert_burst_gen_monotonic(42, 41);
}
