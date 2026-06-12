
//! Pure-helper coverage for the auto-hide/fade model. These functions
//! back BOTH the main-window render path (`window_event.rs`) and the
//! torn-out child render path (`child_window.rs`) verbatim, so a single
//! correct spec here pins main/child scrollbar parity. The
//! `child_window` integration suite exercises the same helpers through
//! the child plumbing; this module nails the math directly.

use super::*;
use std::time::Duration;

// A single pane id=1 occupying x∈[0,800), y∈[30,600).
const PANE: (u64, f32, f32, f32, f32) = (1, 0.0, 30.0, 800.0, 570.0);

fn at(secs_ago: u64, now: Instant) -> Instant {
    now.checked_sub(Duration::from_secs(secs_ago)).unwrap()
}

#[test]
fn new_state_starts_hidden() {
    let now = Instant::now();
    let s = ScrollbarVisState::new(now);
    assert_eq!(s.alpha, 0.0);
    assert!(!s.mouse_near_right_edge);
    // `None` == never active == infinitely idle, so the bar starts
    // hidden. This must hold even on a freshly-booted machine whose
    // monotonic clock is younger than the old 3600s offset (the bug
    // CI caught on fresh Windows runners).
    assert_eq!(s.last_active, None);
    assert!(!is_animating(&s, ScrollbarMode::Auto, false), "fresh state must not animate");
}

#[test]
fn idle_cursor_away_from_edge_stays_hidden() {
    // The user's bug: scrollbar visible without the cursor near the
    // right edge. With no recent activity and the cursor parked in the
    // middle of the pane, alpha must stay 0 and the bar must not animate.
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let cursor = (400.0, 300.0); // dead-center, far from right edge
    let alphas =
        update_and_collect(&mut vis, &[PANE], cursor, PANE.0, None, ScrollbarMode::Auto, now);
    assert_eq!(alphas.get(&1).copied(), Some(0.0), "center cursor must keep bar hidden");
    let st = vis.get(&1).unwrap();
    assert!(!is_animating(st, ScrollbarMode::Auto, false), "settled-hidden must not redraw-storm");
}

#[test]
fn cursor_near_right_edge_shows_bar() {
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    // x just inside the right edge (800 - 5 = 795) within EDGE_PROXIMITY_PX.
    let cursor = (795.0, 300.0);
    // First frame enters the proximity strip → marks active, begins fade-in.
    update_and_collect(&mut vis, &[PANE], cursor, 1, None, ScrollbarMode::Auto, now);
    assert!(vis.get(&1).unwrap().mouse_near_right_edge);
    // Advance ~200ms (> FADE_IN_MS) and the bar reaches full alpha.
    let later = now.checked_add(Duration::from_millis(200)).unwrap();
    let alphas = update_and_collect(&mut vis, &[PANE], cursor, 1, None, ScrollbarMode::Auto, later);
    assert_eq!(alphas.get(&1).copied(), Some(1.0));
}

#[test]
fn recent_scroll_activity_keeps_bar_visible_then_fades() {
    // Mirrors set_child_pane_view_top/mark_scrollbar_active: a scroll
    // marks the pane active, so the bar shows even with the cursor away
    // from the edge — but only for the idle window, then it fades.
    let now = Instant::now();
    let mut st = ScrollbarVisState::new(now);
    st.mark_active(now);
    // Immediately after activity: animating toward visible.
    assert!(is_animating(&st, ScrollbarMode::Auto, false));
    let v = tick(
        &mut st,
        ScrollbarMode::Auto,
        false,
        now.checked_add(Duration::from_millis(200)).unwrap(),
    );
    assert_eq!(v, 1.0, "recent activity makes the bar fully visible");
    // Long past the idle window with no further activity: fades to hidden.
    st.last_active = Some(at(10, now));
    let faded = tick(
        &mut st,
        ScrollbarMode::Auto,
        false,
        now.checked_add(Duration::from_secs(11)).unwrap(),
    );
    assert_eq!(faded, 0.0, "idle past IDLE_HIDE_MS fades the bar out");
    assert!(
        !is_animating(&st, ScrollbarMode::Auto, false),
        "fully hidden + idle must not keep redrawing"
    );
}

#[test]
fn always_and_never_short_circuit() {
    let now = Instant::now();
    let mut st = ScrollbarVisState::new(now);
    assert_eq!(tick(&mut st, ScrollbarMode::Always, false, now), 1.0);
    assert!(!is_animating(&st, ScrollbarMode::Always, false), "Always never animates");
    assert_eq!(tick(&mut st, ScrollbarMode::Never, false, now), 0.0);
    assert!(!is_animating(&st, ScrollbarMode::Never, false), "Never never animates");
}

#[test]
fn drag_overrides_idle_and_edge() {
    // A thumb drag keeps the bar visible regardless of cursor position
    // or idle time — true on both windows (drag_active_on_pane).
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let cursor = (10.0, 300.0); // far left, nowhere near the edge
    let later = now.checked_add(Duration::from_millis(300)).unwrap();
    update_and_collect(&mut vis, &[PANE], cursor, 1, Some(1), ScrollbarMode::Auto, now);
    let alphas =
        update_and_collect(&mut vis, &[PANE], cursor, 1, Some(1), ScrollbarMode::Auto, later);
    assert_eq!(alphas.get(&1).copied(), Some(1.0), "active drag forces visible");
}

#[test]
fn near_edge_band_is_tight_to_the_right_gutter() {
    // Regression guard for the "scrollbar shows without edge hover"
    // report: the proximity test must be FALSE for a center cursor and
    // TRUE only within EDGE_PROXIMITY_PX of the right edge.
    let (_, px, py, pw, ph) = PANE;
    assert!(!is_mouse_near_right_edge(px, py, pw, ph, 400.0, 300.0), "center is not near edge");
    assert!(
        !is_mouse_near_right_edge(px, py, pw, ph, 770.0, 300.0),
        "30px in is outside the 20px band"
    );
    assert!(
        is_mouse_near_right_edge(px, py, pw, ph, 795.0, 300.0),
        "5px from edge is inside the band"
    );
    // Outside the pane vertically → never near the edge.
    assert!(
        !is_mouse_near_right_edge(px, py, pw, ph, 795.0, 5.0),
        "above the pane is not near edge"
    );
}
