//! #386 PR-D — scrollbar auto-hide + fade animation.
//!
//! Pure-state tests over the `scrollbar_visibility` helpers. The full
//! mouse-move + render plumbing is exercised manually via the §13 GUI
//! smoke; here we lock down the contract that the per-frame `tick` will
//! converge alpha to the expected target under each mode, given the
//! activity inputs from the window-event handler.

use sonicterm_app::app::scrollbar_visibility::{
    is_animating, is_mouse_near_right_edge, tick, update_and_collect, ScrollbarVisState,
    ALPHA_EMIT_FLOOR, EDGE_PROXIMITY_PX, FADE_IN_MS, FADE_OUT_MS, IDLE_HIDE_MS,
};
use sonicterm_core::config::ScrollbarMode;
use std::time::{Duration, Instant};

/// Advance simulated time by repeatedly ticking with synthetic now()
/// stamps spaced 16 ms apart (~60 fps).
fn advance_frames(
    state: &mut ScrollbarVisState,
    mode: ScrollbarMode,
    drag: bool,
    start: Instant,
    total_ms: u64,
) -> Instant {
    let mut now = start;
    let mut elapsed = 0;
    while elapsed < total_ms {
        now += Duration::from_millis(16);
        elapsed += 16;
        let _ = tick(state, mode, drag, now);
    }
    now
}

#[test]
fn auto_default_alpha_settles_to_zero_after_idle() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    // No activity ever. After ~1 s of frames the alpha must be at 0.
    let _ = advance_frames(&mut s, ScrollbarMode::Auto, false, t0, 1000);
    assert!(s.alpha < ALPHA_EMIT_FLOOR, "expected alpha ~0, got {}", s.alpha);
}

#[test]
fn auto_mouse_near_right_edge_raises_alpha_to_one() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    s.mouse_near_right_edge = true;
    s.mark_active(t0);
    // Fade-in completes in ~FADE_IN_MS; give it a margin.
    let _ = advance_frames(&mut s, ScrollbarMode::Auto, false, t0, FADE_IN_MS + 50);
    assert!((s.alpha - 1.0).abs() < 1e-3, "expected alpha 1.0, got {}", s.alpha);
}

#[test]
fn auto_mouse_leaves_right_edge_alpha_fades_back_to_zero() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    s.mouse_near_right_edge = true;
    s.mark_active(t0);
    let t1 = advance_frames(&mut s, ScrollbarMode::Auto, false, t0, FADE_IN_MS + 50);
    assert!((s.alpha - 1.0).abs() < 1e-3);
    // Mouse leaves; last_active stays at t0 so idle window is already
    // counting down. After IDLE_HIDE_MS + FADE_OUT_MS + slop the bar
    // must be hidden.
    s.mouse_near_right_edge = false;
    let _ =
        advance_frames(&mut s, ScrollbarMode::Auto, false, t1, IDLE_HIDE_MS + FADE_OUT_MS + 100);
    assert!(s.alpha < ALPHA_EMIT_FLOOR, "expected alpha ~0, got {}", s.alpha);
}

#[test]
fn auto_scroll_event_briefly_shows_then_hides() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    // Simulate a scroll: mark_active, no hover.
    s.mark_active(t0);
    // Inside the idle window the alpha should rise.
    let t1 = advance_frames(&mut s, ScrollbarMode::Auto, false, t0, FADE_IN_MS + 50);
    assert!(s.alpha > 0.9, "expected alpha near 1, got {}", s.alpha);
    // After idle + fade-out it goes back to 0.
    let _ =
        advance_frames(&mut s, ScrollbarMode::Auto, false, t1, IDLE_HIDE_MS + FADE_OUT_MS + 100);
    assert!(s.alpha < ALPHA_EMIT_FLOOR, "expected alpha ~0, got {}", s.alpha);
}

#[test]
fn auto_drag_keeps_alpha_pinned_at_one() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    // Drag is in progress, no recent mark_active, no hover. The drag
    // flag alone must keep the bar visible.
    let _ = advance_frames(&mut s, ScrollbarMode::Auto, true, t0, 2000);
    assert!((s.alpha - 1.0).abs() < 1e-3, "drag pinned alpha != 1, got {}", s.alpha);
}

#[test]
fn always_mode_alpha_is_always_one() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    let a = tick(&mut s, ScrollbarMode::Always, false, t0);
    assert_eq!(a, 1.0);
    let a = tick(&mut s, ScrollbarMode::Always, false, t0 + Duration::from_secs(10));
    assert_eq!(a, 1.0);
}

#[test]
fn never_mode_alpha_is_always_zero() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    s.mouse_near_right_edge = true;
    s.mark_active(t0);
    let a = tick(&mut s, ScrollbarMode::Never, true, t0);
    assert_eq!(a, 0.0);
}

#[test]
fn always_and_never_never_animate() {
    let t0 = Instant::now();
    let mut s = ScrollbarVisState::new(t0);
    let _ = tick(&mut s, ScrollbarMode::Always, false, t0);
    assert!(!is_animating(&s, ScrollbarMode::Always, false));
    let _ = tick(&mut s, ScrollbarMode::Never, false, t0);
    assert!(!is_animating(&s, ScrollbarMode::Never, false));
}

#[test]
fn edge_proximity_excludes_far_left_clicks() {
    // 100x100 pane at (0,0). Cursor at x=5 is far from right edge.
    assert!(!is_mouse_near_right_edge(0.0, 0.0, 100.0, 100.0, 5.0, 50.0));
}

#[test]
fn edge_proximity_includes_clicks_just_inside_right_edge() {
    // Pane right edge at x=100. Within EDGE_PROXIMITY_PX = 20 logical
    // px of the edge counts as near.
    assert!(is_mouse_near_right_edge(
        0.0,
        0.0,
        100.0,
        100.0,
        100.0 - EDGE_PROXIMITY_PX + 1.0,
        50.0,
    ));
}

#[test]
fn edge_proximity_excludes_cursor_outside_pane_vertically() {
    assert!(!is_mouse_near_right_edge(0.0, 100.0, 100.0, 100.0, 95.0, 50.0));
}

#[test]
fn update_and_collect_only_pane_under_cursor_gets_alpha_rise() {
    // Two side-by-side panes; cursor near the right edge of pane A
    // only. Pane B's alpha must stay at 0.
    let t0 = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let panes = vec![(1u64, 0.0, 0.0, 100.0, 100.0), (2u64, 100.0, 0.0, 100.0, 100.0)];
    // Cursor at right edge of pane A (x=95, y=50) — pane B starts at x=100.
    let cursor = (95.0, 50.0);
    let alphas = update_and_collect(
        &mut vis,
        &panes,
        cursor,
        /* active_id */ 1,
        /* drag_active_on_pane */ None,
        ScrollbarMode::Auto,
        t0,
    );
    let near_a = vis.get(&1).unwrap().mouse_near_right_edge;
    let near_b = vis.get(&2).unwrap().mouse_near_right_edge;
    assert!(near_a, "pane A should be flagged near-right-edge");
    assert!(!near_b, "pane B must NOT be flagged near-right-edge");
    // First tick alpha won't yet be 1.0 (we just started fading in),
    // but pane B must be exactly 0.0.
    assert_eq!(alphas[&2], 0.0);
}

#[test]
fn update_and_collect_prunes_closed_panes() {
    let t0 = Instant::now();
    let mut vis = std::collections::HashMap::new();
    vis.insert(99u64, ScrollbarVisState::new(t0));
    let panes = vec![(1u64, 0.0, 0.0, 100.0, 100.0)];
    let _ = update_and_collect(&mut vis, &panes, (0.0, 0.0), 1, None, ScrollbarMode::Auto, t0);
    assert!(!vis.contains_key(&99), "closed pane entry must be pruned");
    assert!(vis.contains_key(&1), "live pane entry must be inserted");
}
