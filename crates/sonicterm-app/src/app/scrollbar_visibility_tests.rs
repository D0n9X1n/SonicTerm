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
    assert!(
        !is_animating(&s, ScrollbarMode::Auto, false, ScrollbarMotion::Animated, now),
        "fresh state must not animate"
    );
}

/// A settled hidden scrollbar must not create an animation redraw loop.
#[test]
fn idle_cursor_away_from_edge_stays_hidden() {
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let cursor = (400.0, 300.0);
    let alphas = update_and_collect(
        &mut vis,
        &[PANE],
        cursor,
        PANE.0,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now,
    );
    assert_eq!(alphas.get(&1).copied(), Some(0.0), "center cursor must keep bar hidden");
    let state = vis.get(&1).unwrap();
    assert!(
        !is_animating(state, ScrollbarMode::Auto, false, ScrollbarMotion::Animated, now,),
        "settled-hidden must not redraw-storm"
    );
}

/// Accelerated opacity advances monotonically and reaches its visible target.
#[test]
fn animated_scrollbar_fades_in_monotonically() {
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let cursor = (795.0, 300.0);
    let first = update_and_collect(
        &mut vis,
        &[PANE],
        cursor,
        1,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now,
    )[&1];
    let middle = update_and_collect(
        &mut vis,
        &[PANE],
        cursor,
        1,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now + Duration::from_millis(75),
    )[&1];
    let final_alpha = update_and_collect(
        &mut vis,
        &[PANE],
        cursor,
        1,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now + Duration::from_millis(225),
    )[&1];

    assert!(first < middle && middle < final_alpha);
    assert_eq!(final_alpha, 1.0);
}

/// Recent activity holds visibility before the accelerated fade returns to hidden.
#[test]
fn recent_scroll_activity_keeps_bar_visible_then_fades() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    state.mark_active(now);
    assert!(is_animating(&state, ScrollbarMode::Auto, false, ScrollbarMotion::Animated, now,));
    assert_eq!(
        tick(
            &mut state,
            ScrollbarMode::Auto,
            false,
            ScrollbarMotion::Animated,
            now + Duration::from_millis(200),
        ),
        1.0
    );

    state.last_active = Some(at(10, now));
    assert_eq!(
        tick(
            &mut state,
            ScrollbarMode::Auto,
            false,
            ScrollbarMotion::Animated,
            now + Duration::from_secs(11),
        ),
        0.0
    );
    assert!(!is_animating(
        &state,
        ScrollbarMode::Auto,
        false,
        ScrollbarMotion::Animated,
        now + Duration::from_secs(11),
    ));
}

/// Degraded presentation reaches both opacity targets immediately and never animates.
#[test]
fn snap_reaches_targets_immediately_without_animation() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    state.mark_active(now);
    assert_eq!(
        tick(
            &mut state,
            ScrollbarMode::Auto,
            false,
            ScrollbarMotion::Snap,
            now + Duration::from_millis(1),
        ),
        1.0
    );
    assert!(!is_animating(
        &state,
        ScrollbarMode::Auto,
        false,
        ScrollbarMotion::Snap,
        now + Duration::from_millis(1),
    ));
    assert_eq!(
        tick(
            &mut state,
            ScrollbarMode::Auto,
            false,
            ScrollbarMotion::Snap,
            now + Duration::from_millis(IDLE_HIDE_MS),
        ),
        0.0
    );
}

/// Snap mode arms exactly one idle deadline and removes it after expiration.
#[test]
fn snap_deadline_expires_once_at_the_idle_boundary() {
    let now = Instant::now();
    let deadline = now + Duration::from_millis(IDLE_HIDE_MS);
    let mut state = ScrollbarVisState::new(now);
    state.mark_active(now);
    state.alpha = 1.0;
    let mut vis = std::collections::HashMap::from([(1, state)]);

    assert_eq!(next_snap_deadline(&vis, ScrollbarMode::Auto, None), Some(deadline));
    assert!(!expire_due_snaps(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        deadline - Duration::from_millis(1),
    ));
    assert!(expire_due_snaps(&mut vis, ScrollbarMode::Auto, None, deadline));
    assert_eq!(vis[&1].alpha, 0.0);
    assert_eq!(next_snap_deadline(&vis, ScrollbarMode::Auto, None), None);
    assert!(!expire_due_snaps(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        deadline + Duration::from_millis(1),
    ));
}

/// Hover, drag, and non-Auto modes suppress one-shot hide deadlines.
#[test]
fn snap_deadline_respects_visibility_overrides_and_modes() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    state.mark_active(now);
    state.alpha = 1.0;
    let mut vis = std::collections::HashMap::from([(1, state)]);

    vis.get_mut(&1).unwrap().mouse_near_right_edge = true;
    assert_eq!(next_snap_deadline(&vis, ScrollbarMode::Auto, None), None);
    vis.get_mut(&1).unwrap().mouse_near_right_edge = false;
    assert_eq!(next_snap_deadline(&vis, ScrollbarMode::Auto, Some(1)), None);
    assert_eq!(next_snap_deadline(&vis, ScrollbarMode::Always, None), None);
    assert_eq!(next_snap_deadline(&vis, ScrollbarMode::Never, None), None);
}

/// An attached renderer's resolved policy overrides the headless app fallback.
#[test]
fn window_motion_prefers_renderer_policy_when_available() {
    assert_eq!(window_scrollbar_motion(Some(true), false), ScrollbarMotion::Snap);
    assert_eq!(window_scrollbar_motion(Some(false), true), ScrollbarMotion::Animated);
    assert_eq!(window_scrollbar_motion(None, true), ScrollbarMotion::Snap);
    assert_eq!(window_scrollbar_motion(None, false), ScrollbarMotion::Animated);
}

/// Always and Never pin opacity without scheduling motion.
#[test]
fn always_and_never_short_circuit() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    assert_eq!(
        tick(&mut state, ScrollbarMode::Always, false, ScrollbarMotion::Animated, now,),
        1.0
    );
    assert!(!is_animating(&state, ScrollbarMode::Always, false, ScrollbarMotion::Animated, now,));
    assert_eq!(tick(&mut state, ScrollbarMode::Never, false, ScrollbarMotion::Animated, now,), 0.0);
    assert!(!is_animating(&state, ScrollbarMode::Never, false, ScrollbarMotion::Animated, now,));
}

/// A drag keeps its pane visible independently of cursor position and idle age.
#[test]
fn drag_overrides_idle_and_edge() {
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let cursor = (10.0, 300.0);
    update_and_collect(
        &mut vis,
        &[PANE],
        cursor,
        1,
        Some(1),
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now,
    );
    let alphas = update_and_collect(
        &mut vis,
        &[PANE],
        cursor,
        1,
        Some(1),
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now + Duration::from_millis(300),
    );
    assert_eq!(alphas.get(&1).copied(), Some(1.0));
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

// ── Registry cleanup ────────────────────────────────────────────────

/// The per-window `scrollbar_vis` map is the only pane-keyed registry
/// that is grown implicitly: entries appear via `entry().or_insert_with`
/// on whatever pane list the render path supplies, and no call site ever
/// calls `remove` on it. What bounds it is the `retain` at the top of
/// this helper, which keeps only the panes in the list it was handed.
///
/// The render path hands it the *visible* pane rects — the active tab of
/// one window — so the map is bounded by visible pane count, not by
/// panes ever created. This pins that bound across pane churn far larger
/// than any real session, and pins the cost that buys it: an entry for a
/// live-but-hidden pane is dropped and rebuilt, so its fade state does
/// not survive a tab switch.
#[test]
fn v120_registry_cleanup_removes_all_owned_entries() {
    let now = Instant::now();
    let mut vis = std::collections::HashMap::new();
    let cursor = (795.0, 300.0); // parked in the right-edge band

    // Churn far past any real session. Pane ids come from a monotonic
    // `AtomicU64` and are never reused, so an unpruned map would grow by
    // one entry per generation and never shrink.
    const GENERATIONS: u64 = 5_000;
    let mut high_water = 0usize;
    for generation in 0..GENERATIONS {
        let id = generation + 1;
        let visible = [(id, 0.0f32, 30.0f32, 800.0f32, 570.0f32)];
        update_and_collect(
            &mut vis,
            &visible,
            cursor,
            id,
            None,
            ScrollbarMode::Auto,
            ScrollbarMotion::Animated,
            now,
        );
        high_water = high_water.max(vis.len());
    }
    assert_eq!(
        high_water, 1,
        "one visible pane must never leave more than one entry behind; \
         {GENERATIONS} generations reached {high_water}"
    );
    assert!(
        vis.contains_key(&GENERATIONS),
        "the surviving entry must be the visible pane, not an arbitrary leftover"
    );

    // The same rule applies to the hover-only path, which the cursor-move
    // handler drives far more often than a full render.
    let mut hover_vis = std::collections::HashMap::new();
    for generation in 0..GENERATIONS {
        let id = generation + 1;
        let visible = [(id, 0.0f32, 30.0f32, 800.0f32, 570.0f32)];
        update_hover_states(&mut hover_vis, &visible, cursor, now);
    }
    assert_eq!(
        hover_vis.len(),
        1,
        "the hover path must prune closed panes too, not only the render path"
    );

    // Two live panes in different tabs. Only one is ever visible, so the
    // hidden one's entry is dropped even though its pane is alive.
    let mut tabbed = std::collections::HashMap::new();
    let front = (1u64, 0.0f32, 30.0f32, 800.0f32, 570.0f32);
    let back = (2u64, 0.0f32, 30.0f32, 800.0f32, 570.0f32);

    update_and_collect(
        &mut tabbed,
        &[front],
        cursor,
        front.0,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        now,
    );
    let faded_in = now.checked_add(Duration::from_millis(200)).unwrap();
    let alphas = update_and_collect(
        &mut tabbed,
        &[front],
        cursor,
        front.0,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        faded_in,
    );
    assert_eq!(
        alphas.get(&front.0).copied(),
        Some(1.0),
        "hovering the right edge must fade the bar fully in"
    );

    // Switch to the other tab: pane 1 is alive but not visible.
    update_and_collect(
        &mut tabbed,
        &[back],
        cursor,
        back.0,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        faded_in,
    );
    assert_eq!(
        tabbed.keys().copied().collect::<Vec<_>>(),
        vec![back.0],
        "only the visible pane may hold an entry while another tab is shown"
    );

    // Switch back. The entry is rebuilt from scratch, so the bar restarts
    // its fade rather than resuming at full alpha. This is the accepted
    // cost of bounding the map by visibility: fade state is ephemeral
    // polish, and trading it for a hard bound is the right trade — but it
    // is a real behavior change on tab switch, so it is pinned here
    // rather than left to be rediscovered as a bug.
    let returned = faded_in.checked_add(Duration::from_millis(10)).unwrap();
    let back_alphas = update_and_collect(
        &mut tabbed,
        &[front],
        cursor,
        front.0,
        None,
        ScrollbarMode::Auto,
        ScrollbarMotion::Animated,
        returned,
    );
    let resumed = back_alphas.get(&front.0).copied().expect("returning pane gets an alpha");
    assert!(
        resumed < 1.0,
        "returning to a tab must restart the fade, not resume it: got {resumed}"
    );
}
