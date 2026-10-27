//! the vsync coalescing gate shared by the main and child
//! `RedrawRequested` arms. These pin the exact deferral policy that
//! lets a bursty `ls -al` coalesce to one frame per vsync and stops a
//! torn-out child from busy-spinning the VT thread's parser lock. The
//! same predicate now backs BOTH windows, so this one spec covers
//! main/child parity for the gate.

use super::{should_defer_streaming_redraw, should_flush_pending_pty_redraw};
use std::time::Duration;

const FRAME: Duration = Duration::from_micros(16_667); // ~60Hz

#[test]
fn streaming_redraw_within_frame_defers() {
    // Pure PTY-streaming repaint that already drew this vsync window:
    // defer to the next boundary (the coalescing win).
    assert!(should_defer_streaming_redraw(
        false, // not input-driven
        false, // no fresh burst
        false, // hardware GPU
        Duration::from_millis(2),
        FRAME,
    ));
}

#[test]
fn input_driven_redraw_never_defers() {
    // Keystroke / resize / theme / IME must render immediately even
    // inside the vsync window — gating them adds perceptible latency.
    assert!(!should_defer_streaming_redraw(true, false, false, Duration::from_millis(1), FRAME));
}

#[test]
fn pty_burst_within_frame_defers_like_other_streaming_redraws() {
    // A PTY burst is still streaming work, not input. Rendering it early
    // can block in surface acquisition waiting for the next drawable; defer
    // inside the current frame window and render at the next boundary.
    assert!(should_defer_streaming_redraw(false, true, false, Duration::from_millis(1), FRAME));
}

#[test]
fn past_frame_boundary_never_defers() {
    // We're past this vsync window — render now, don't defer forever.
    assert!(!should_defer_streaming_redraw(false, false, false, Duration::from_millis(20), FRAME));
    // Exactly at the boundary also renders (`<` is strict).
    assert!(!should_defer_streaming_redraw(false, false, false, FRAME, FRAME));
}

#[test]
fn input_with_concurrent_burst_coalesces() {
    // The typing case: a keystroke sets input_dirty and the
    // char's PTY echo arrives as a burst, so the echo's redraw is BOTH
    // dirty AND a burst. It must coalesce like other streaming work, not
    // render per echo chunk — otherwise fast typing / Claude Code streams
    // storm the renderer.
    assert!(should_defer_streaming_redraw(true, true, false, Duration::ZERO, FRAME));
    // ...but only within the frame window; past the boundary it renders.
    assert!(!should_defer_streaming_redraw(true, true, false, Duration::from_millis(20), FRAME));
}

#[test]
fn software_render_defers_even_pure_input() {
    // on a CPU rasterizer, even a pure input redraw (dirty,
    // no burst) coalesces to the frame cap — fast typing must not force a
    // full-screen software raster per keystroke. Within the frame window
    // it defers...
    assert!(should_defer_streaming_redraw(true, false, true, Duration::from_millis(1), FRAME));
    // ...but still renders once past the boundary (bounded latency, not
    // dropped).
    assert!(!should_defer_streaming_redraw(true, false, true, Duration::from_millis(40), FRAME));
}

#[test]
fn pty_redraw_flush_waits_for_burst_window() {
    assert!(!should_flush_pending_pty_redraw(512, Duration::from_millis(2)));
    assert!(should_flush_pending_pty_redraw(512, Duration::from_millis(8)));
}

#[test]
fn pty_redraw_flushes_large_bursts_without_waiting() {
    assert!(should_flush_pending_pty_redraw(super::PTY_REDRAW_FLUSH_BYTES, Duration::ZERO,));
}
