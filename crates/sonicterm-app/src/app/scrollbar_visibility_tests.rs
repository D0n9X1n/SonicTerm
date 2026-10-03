//! Pure-helper coverage for the auto-hide/fade model. These functions
//! back BOTH the main-window render path (`window_event.rs`) and the
//! torn-out child render path (`child_window_redraw.rs`) verbatim, so a single
//! correct spec here pins main/child scrollbar parity. The
//! `child_window` integration suite exercises the same helpers through
//! the child plumbing; this module nails the math directly.

use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use std::collections::HashMap;
use std::time::Duration;
use winit::window::WindowId;

// A single pane id=1 occupying x∈[0,800), y∈[30,600).
const PANE: (u64, f32, f32, f32, f32) = (1, 0.0, 30.0, 800.0, 570.0);

fn at(secs_ago: u64, now: Instant) -> Instant {
    now.checked_sub(Duration::from_secs(secs_ago)).unwrap()
}

/// One frame at the 60 Hz rate the fade durations assume.
const FRAME: Duration = Duration::from_micros(16_667);
/// The idle window as a duration.
const IDLE: Duration = Duration::from_millis(IDLE_HIDE_MS);
/// A cursor inside pane 1's right-edge band, and one over its text.
const NEAR_EDGE: (f32, f32) = (795.0, 300.0);
const AWAY: (f32, f32) = (400.0, 300.0);

/// Tick an Auto, animated `state` at 60 Hz from `first_frame` until its alpha
/// reaches `goal`; returns how many frames that took.
fn frames_to_reach(state: &mut ScrollbarVisState, first_frame: Instant, goal: f32) -> usize {
    for frame_count in 1..=120u32 {
        let frame_at = first_frame + FRAME * (frame_count - 1);
        let alpha = tick(state, ScrollbarMode::Auto, false, ScrollbarMotion::Animated, frame_at);
        if alpha == goal {
            return frame_count as usize;
        }
    }
    panic!("alpha never reached {goal}: {state:?}");
}

/// A visible, settled Auto scrollbar: active at `active`, fully faded in, and
/// last ticked at `active`, so any later first tick sees a stale `last_tick`.
fn settled_visible(active: Instant) -> ScrollbarVisState {
    let mut state = ScrollbarVisState::new(active);
    note_activity(&mut state, active);
    retarget(&mut state, ScrollbarMode::Auto, false, active);
    state.alpha = 1.0;
    state
}

#[test]
fn new_state_starts_hidden() {
    let now = Instant::now();
    let state = ScrollbarVisState::new(now);
    assert_eq!(state.alpha, 0.0);
    assert!(!state.mouse_near_right_edge);
    // `None` == never active == infinitely idle, so the bar starts
    // hidden. This must hold even on a freshly-booted machine whose
    // monotonic clock is younger than the old 3600s offset (the bug
    // CI caught on fresh Windows runners).
    assert_eq!(state.last_active, None);
    assert!(
        !is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated),
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
        !is_animating(state, ScrollbarMode::Auto, ScrollbarMotion::Animated),
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

/// Recent activity holds visibility; once the idle window has passed, the
/// accelerated fade returns to hidden over several frames, not in one jump.
#[test]
fn recent_scroll_activity_keeps_bar_visible_then_fades() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    note_activity(&mut state, now);
    retarget(&mut state, ScrollbarMode::Auto, false, now);
    assert!(is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));
    assert!(frames_to_reach(&mut state, now, 1.0) >= 2);
    assert!(!is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));

    // A frame long after the idle window retargets toward hidden and starts the fade.
    let late = now + Duration::from_secs(11);
    let first = tick(&mut state, ScrollbarMode::Auto, false, ScrollbarMotion::Animated, late);
    assert!(first > 0.0 && first < 1.0, "the first fade step is one frame, got {first}");
    assert!(frames_to_reach(&mut state, late + FRAME, 0.0) >= 1);
    assert!(!is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));
}

/// Degraded presentation reaches both opacity targets immediately and never animates.
#[test]
fn snap_reaches_targets_immediately_without_animation() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    note_activity(&mut state, now);
    retarget(&mut state, ScrollbarMode::Auto, false, now);
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
    assert!(!is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Snap));
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
    note_activity(&mut state, now);
    retarget(&mut state, ScrollbarMode::Auto, false, now);
    state.alpha = 1.0;
    let mut vis = std::collections::HashMap::from([(1, state)]);

    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), Some(deadline));
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Snap,
        deadline - Duration::from_millis(1),
    ));
    assert!(expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Snap, deadline));
    assert_eq!(vis[&1].alpha, 0.0);
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Snap,
        deadline + Duration::from_millis(1),
    ));
}

/// Hover, drag, and non-Auto modes suppress one-shot hide deadlines.
#[test]
fn snap_deadline_respects_visibility_overrides_and_modes() {
    let now = Instant::now();
    let mut state = ScrollbarVisState::new(now);
    note_activity(&mut state, now);
    retarget(&mut state, ScrollbarMode::Auto, false, now);
    state.alpha = 1.0;
    let mut vis = std::collections::HashMap::from([(1, state)]);

    vis.get_mut(&1).unwrap().mouse_near_right_edge = true;
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
    vis.get_mut(&1).unwrap().mouse_near_right_edge = false;
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, Some(1)), None);
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Always, None), None);
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Never, None), None);
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
    assert!(!is_animating(&state, ScrollbarMode::Always, ScrollbarMotion::Animated));
    assert_eq!(tick(&mut state, ScrollbarMode::Never, false, ScrollbarMotion::Animated, now,), 0.0);
    assert!(!is_animating(&state, ScrollbarMode::Never, ScrollbarMotion::Animated));
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
    let (_, pane_x, pane_y, pane_w, pane_h) = PANE;
    assert!(
        !is_mouse_near_right_edge(pane_x, pane_y, pane_w, pane_h, 400.0, 300.0),
        "center is not near edge"
    );
    assert!(
        !is_mouse_near_right_edge(pane_x, pane_y, pane_w, pane_h, 770.0, 300.0),
        "30px in is outside the 20px band"
    );
    assert!(
        is_mouse_near_right_edge(pane_x, pane_y, pane_w, pane_h, 795.0, 300.0),
        "5px from edge is inside the band"
    );
    // Outside the pane vertically → never near the edge.
    assert!(
        !is_mouse_near_right_edge(pane_x, pane_y, pane_w, pane_h, 795.0, 5.0),
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
        update_hover_states(&mut hover_vis, &visible, cursor, ScrollbarMode::Auto, None, now);
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

// ── Settled scrollbar: one idle deadline, no frames in between ─────

#[test]
fn a_settled_auto_scrollbar_requests_no_frames_before_its_idle_deadline() {
    // A bar that reached its visible target draws nothing new for the rest of
    // the idle window, so it must not ask for frames; its single wake is the
    // idle deadline at `last_active + IDLE_HIDE_MS`.
    let active = Instant::now();
    let state = settled_visible(active);
    assert!(!is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));
    let vis = HashMap::from([(1, state)]);
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), Some(active + IDLE));
}

#[test]
fn the_idle_deadline_fires_once_and_leaves_alpha_for_the_next_frame() {
    // Expiry consumes the deadline and retargets at the deadline instant; the
    // fade itself runs on the frames that follow, and a second expiry is inert.
    let active = Instant::now();
    let deadline = active + IDLE;
    let mut vis = HashMap::from([(1, settled_visible(active))]);
    let early = deadline - Duration::from_millis(1);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        early
    ));
    assert!(expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        deadline
    ));
    let state = vis[&1];
    assert_eq!(state.alpha, 1.0, "the fade starts on the next frame, not at expiry");
    assert!(state.idle_consumed);
    assert_eq!(state.target, 0.0);
    assert_eq!(state.transition_start, Some(deadline));
    assert!(is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
    let after = vis.clone();
    let later = deadline + Duration::from_secs(1);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        later
    ));
    assert_eq!(vis, after, "a consumed deadline cannot fire again");
}

#[test]
fn activity_after_the_deadline_fired_arms_a_new_one() {
    // Activity clears the consumed flag, so every activity arms exactly one deadline.
    let active = Instant::now();
    let deadline = active + IDLE;
    let mut vis = HashMap::from([(1, settled_visible(active))]);
    assert!(expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        deadline
    ));
    let again = deadline + Duration::from_millis(50);
    let state = vis.get_mut(&1).unwrap();
    note_activity(state, again);
    retarget(state, ScrollbarMode::Auto, false, again);
    assert!(!state.idle_consumed);
    assert_eq!(state.target, 1.0);
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), Some(again + IDLE));
}

#[test]
fn the_idle_fade_out_takes_at_least_two_frames_with_a_stale_last_tick() {
    // The settled bar last ticked at its activity, 600 ms before the fade. The
    // first step is capped at one frame period, so the fade is never one jump,
    // and it still finishes on schedule once frames flow.
    let active = Instant::now();
    let deadline = active + IDLE;
    let mut vis = HashMap::from([(1, settled_visible(active))]);
    assert!(expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        deadline
    ));
    let frame_count = frames_to_reach(vis.get_mut(&1).unwrap(), deadline + FRAME, 0.0);
    assert!(frame_count >= 2, "{frame_count} frames");
    assert!(frame_count <= 20, "a 300 ms fade at 60 Hz runs about 18 frames: {frame_count}");
}

#[test]
fn late_service_still_fades_over_at_least_two_frames() {
    // A deadline serviced 200 ms late, and a fade-in whose first frame runs
    // 200 ms late, both measure their first step from the transition and cap it.
    let active = Instant::now();
    let deadline = active + IDLE;
    let late = deadline + Duration::from_millis(200);
    let mut vis = HashMap::from([(1, settled_visible(active))]);
    assert!(expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Animated, late));
    assert_eq!(vis[&1].transition_start, Some(deadline), "the fade dates from the deadline");
    assert!(frames_to_reach(vis.get_mut(&1).unwrap(), late + FRAME, 0.0) >= 2);

    let mut fade_in = ScrollbarVisState::new(active);
    note_activity(&mut fade_in, active);
    retarget(&mut fade_in, ScrollbarMode::Auto, false, active);
    let frame_count = frames_to_reach(&mut fade_in, active + Duration::from_millis(200), 1.0);
    assert!(frame_count >= 2, "the 150 ms fade-in jumped in {frame_count} frame");
}

#[test]
fn a_hover_exit_and_a_drag_release_each_start_a_fade() {
    // Leaving the edge band and releasing a drag change the target at that
    // instant, so the fade starts at once and steps over frames even though
    // the bar last ticked two seconds earlier.
    let entry = Instant::now();
    let exit = entry + Duration::from_secs(2);
    for clear in [false, true] {
        let mut vis = HashMap::new();
        assert!(update_hover_states(
            &mut vis,
            &[PANE],
            NEAR_EDGE,
            ScrollbarMode::Auto,
            None,
            entry
        ));
        assert_eq!(vis[&1].target, 1.0);
        vis.get_mut(&1).unwrap().alpha = 1.0;
        let exited = if clear {
            clear_hover_states(&mut vis, ScrollbarMode::Auto, None, exit)
        } else {
            update_hover_states(&mut vis, &[PANE], AWAY, ScrollbarMode::Auto, None, exit)
        };
        assert!(exited);
        let state = vis.get_mut(&1).unwrap();
        assert_eq!((state.target, state.transition_start), (0.0, Some(exit)), "clear {clear}");
        assert!(frames_to_reach(state, exit + FRAME, 0.0) >= 2);
    }

    let start = Instant::now();
    let release = start + Duration::from_secs(2);
    let mut vis = HashMap::from([(1, ScrollbarVisState::new(start))]);
    retarget_panes(&mut vis, ScrollbarMode::Auto, Some(1), start);
    assert_eq!(vis[&1].target, 1.0, "a drag holds the bar");
    vis.get_mut(&1).unwrap().alpha = 1.0;
    retarget_panes(&mut vis, ScrollbarMode::Auto, None, release);
    let state = vis.get_mut(&1).unwrap();
    assert_eq!((state.target, state.transition_start), (0.0, Some(release)));
    assert!(frames_to_reach(state, release + FRAME, 0.0) >= 2);
}

#[test]
fn repeated_activity_that_keeps_the_target_keeps_the_transition_start() {
    // A stream of scrolls or hover events that leaves the target at 1 must not
    // restart the transition, or a fade in progress would restart on every event.
    let start = Instant::now();
    let mut state = ScrollbarVisState::new(start);
    note_activity(&mut state, start);
    assert!(retarget(&mut state, ScrollbarMode::Auto, false, start));
    assert_eq!(state.transition_start, Some(start));
    for step in 1..=5u32 {
        let event_at = start + Duration::from_millis(100) * step;
        note_activity(&mut state, event_at);
        assert!(!retarget(&mut state, ScrollbarMode::Auto, false, event_at));
    }
    let mut vis = HashMap::from([(1, state)]);
    let hover_at = start + IDLE;
    assert!(update_hover_states(&mut vis, &[PANE], NEAR_EDGE, ScrollbarMode::Auto, None, hover_at));
    let still = hover_at + FRAME;
    assert!(!update_hover_states(&mut vis, &[PANE], NEAR_EDGE, ScrollbarMode::Auto, None, still));
    assert_eq!(vis[&1].transition_start, Some(start));
}

/// A drag pinned to `pane_id` with plausible geometry.
fn drag_state(pane_id: u64) -> crate::app::scrollbar_input::ScrollbarDragState {
    let track_rect = sonicterm_ui::scrollbar::Rect { x: 792.0, y: 0.0, w: 8.0, h: 480.0 };
    let thumb_rect = sonicterm_ui::scrollbar::Rect { h: 48.0, ..track_rect };
    crate::app::scrollbar_input::ScrollbarDragState {
        pane_id,
        geometry: sonicterm_ui::scrollbar::ScrollbarGeometry { track_rect, thumb_rect },
        press_y: 10.0,
        grab_offset: 10.0,
        viewport_rows: 24,
        total_rows: 240,
    }
}

#[test]
fn focus_loss_during_a_drag_and_a_drag_cancel_each_start_a_fade() {
    // Both paths end a drag without a button release; the bar was last active
    // two seconds ago, so ending the hold must start its fade, not leave it shown.
    for cancel in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.config.appearance.scrollbar = ScrollbarMode::Auto;
        let pane = app.__test_seed_tab("drag");
        let main = app.main_window_id.expect("seeded main window");
        let started = at(2, Instant::now());
        let window = app.windows.get_mut(&main).unwrap();
        window.scrollbar_drag = Some(drag_state(pane));
        let mut state = ScrollbarVisState::new(started);
        note_activity(&mut state, started);
        retarget(&mut state, ScrollbarMode::Auto, true, started);
        state.alpha = 1.0;
        window.scrollbar_vis.insert(pane, state);
        if cancel {
            app.cancel_drag_session();
        } else {
            app.handle_window_focus_changed(main, false);
        }
        let window = &app.windows[&main];
        assert!(window.scrollbar_drag.is_none(), "cancel {cancel}");
        let state = window.scrollbar_vis[&pane];
        assert_eq!(state.target, 0.0, "cancel {cancel}: the hold ended");
        assert!(state.transition_start.is_some_and(|start| start > started));
        assert_eq!(state.alpha, 1.0, "cancel {cancel}: an animated bar fades, it does not snap");
        assert!(is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated));
    }
}

#[test]
fn snap_mode_and_the_software_path_still_snap() {
    // Degraded presentation keeps assigning targets at once: shown on activity,
    // hidden in one step at the idle deadline, and the deadline is consumed.
    assert_eq!(window_scrollbar_motion(None, true), ScrollbarMotion::Snap);
    let active = Instant::now();
    let mut state = ScrollbarVisState::new(active);
    note_activity(&mut state, active);
    retarget(&mut state, ScrollbarMode::Auto, false, active);
    let shown = active + Duration::from_millis(1);
    assert_eq!(tick(&mut state, ScrollbarMode::Auto, false, ScrollbarMotion::Snap, shown), 1.0);
    let deadline = active + IDLE;
    let mut vis = HashMap::from([(1, state)]);
    assert!(expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Snap, deadline));
    assert_eq!(vis[&1].alpha, 0.0);
    assert!(vis[&1].idle_consumed);
    assert!(!is_animating(&vis[&1], ScrollbarMode::Auto, ScrollbarMotion::Snap));
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
}

#[test]
fn a_held_scrollbar_arms_no_idle_deadline() {
    // Edge hover and a drag hold the bar visible, so neither contributes a
    // deadline and expiry leaves the held bar alone.
    let active = Instant::now();
    let later = active + Duration::from_secs(10);
    let mut vis = HashMap::from([(1, settled_visible(active))]);
    vis.get_mut(&1).unwrap().mouse_near_right_edge = true;
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        None,
        ScrollbarMotion::Animated,
        later
    ));
    vis.get_mut(&1).unwrap().mouse_near_right_edge = false;
    assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, Some(1)), None);
    assert!(!expire_due_idle(
        &mut vis,
        ScrollbarMode::Auto,
        Some(1),
        ScrollbarMotion::Animated,
        later
    ));
    let state = vis[&1];
    assert!(!state.idle_consumed);
    assert_eq!((state.alpha, state.target), (1.0, 1.0));
}

/// Drive one pane the way a window does: the idle deadline is serviced when
/// due, frames run at 60 Hz while the bar animates, the loop sleeps to the
/// next deadline while it rests, and it stops when nothing is left. Returns
/// the instant it came to rest.
fn drive_to_rest(
    vis: &mut HashMap<u64, ScrollbarVisState>,
    cursor: (f32, f32),
    mut now: Instant,
) -> Instant {
    for _ in 0..1000 {
        if next_idle_deadline(vis, ScrollbarMode::Auto, None).is_some_and(|due| due <= now) {
            expire_due_idle(vis, ScrollbarMode::Auto, None, ScrollbarMotion::Animated, now);
        }
        if vis
            .values()
            .any(|state| is_animating(state, ScrollbarMode::Auto, ScrollbarMotion::Animated))
        {
            let mode = ScrollbarMode::Auto;
            update_and_collect(
                vis,
                &[PANE],
                cursor,
                PANE.0,
                None,
                mode,
                ScrollbarMotion::Animated,
                now,
            );
            now += FRAME;
        } else if let Some(deadline) = next_idle_deadline(vis, ScrollbarMode::Auto, None) {
            now = now.max(deadline);
        } else {
            return now;
        }
    }
    panic!("the scrollbar never came to rest: {vis:?}");
}

#[test]
fn two_hover_show_hide_cycles_each_end_hidden() {
    // The first hover is held past the idle window; the second leaves 200 ms
    // after entry, so only its deadline can hide it. Each entry arms a deadline
    // and each cycle comes to rest hidden with no deadline left.
    let mut vis = HashMap::new();
    let mut now = Instant::now();
    for hover_ms in [700u64, 200] {
        assert!(update_hover_states(&mut vis, &[PANE], NEAR_EDGE, ScrollbarMode::Auto, None, now));
        let entry = now;
        assert_eq!(vis[&1].last_active, Some(entry));
        assert!(!vis[&1].idle_consumed, "hover {hover_ms}: entry arms a deadline");
        now = drive_to_rest(&mut vis, NEAR_EDGE, now);
        assert_eq!(vis[&1].alpha, 1.0, "hover {hover_ms}: shown while held");
        now = now.max(entry + Duration::from_millis(hover_ms));
        assert!(update_hover_states(&mut vis, &[PANE], AWAY, ScrollbarMode::Auto, None, now));
        assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), Some(entry + IDLE));
        now = drive_to_rest(&mut vis, AWAY, now);
        let state = vis[&1];
        assert_eq!(state.alpha, 0.0, "hover {hover_ms}: the cycle ends hidden");
        assert!(state.idle_consumed);
        assert_eq!(next_idle_deadline(&vis, ScrollbarMode::Auto, None), None);
        now += Duration::from_secs(1);
    }
}

/// Every production `.rs` file under this crate's `src`, with CRLF folded to LF.
fn production_sources() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![root.clone()];
    let mut sources = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let entry_path = entry.expect("dir entry").path();
            if entry_path.is_dir() {
                pending.push(entry_path);
            } else if entry_path.extension().is_some_and(|ext| ext == "rs")
                && !entry_path.to_string_lossy().ends_with("_tests.rs")
            {
                let name =
                    entry_path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                let text = std::fs::read_to_string(&entry_path).expect("read source");
                sources.push((name, text.replace("\r\n", "\n")));
            }
        }
    }
    sources
}

/// Whether `line` opens a function at module or impl level.
fn opens_function(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    indent <= 4
        && ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
}

#[test]
fn every_scrollbar_target_input_write_is_followed_by_a_retarget() {
    // The target is stored, not recomputed per frame, so a write to a target
    // input (drag, edge hover, activity) that skips `retarget` leaves the
    // stored target stale: a release that never fades, or a deadline that
    // never arms. Every such write must be followed by a retarget in the same
    // function, and the inventory pins every site so a new one is reviewed.
    const PATTERNS: [&str; 5] = [
        "scrollbar_drag = ",
        "scrollbar_drag.take()",
        "mouse_near_right_edge = ",
        "last_active = ",
        "note_activity(",
    ];
    let mut found: Vec<(String, &str, usize)> = Vec::new();
    for (name, text) in production_sources() {
        let lines: Vec<&str> = text.lines().collect();
        for pattern in PATTERNS {
            let mut site_count = 0;
            for (index, line) in lines.iter().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//")
                    || !line.contains(pattern)
                    || line.contains("fn note_activity(")
                {
                    continue;
                }
                let opener = lines[..index].iter().rev().find(|prior| opens_function(prior));
                if pattern == "last_active = "
                    && opener.is_some_and(|open| open.contains("fn note_activity("))
                {
                    // `note_activity` is the activity operation itself.
                    continue;
                }
                let end = lines[index + 1..]
                    .iter()
                    .position(|next| opens_function(next))
                    .map_or(lines.len(), |offset| index + 1 + offset);
                let rest = lines[index..end].join("\n");
                assert!(
                    ["retarget(", "retarget_panes(", "retarget_scrollbars("]
                        .iter()
                        .any(|call| rest.contains(call)),
                    "{name}:{}: `{pattern}` is not followed by a retarget",
                    index + 1
                );
                site_count += 1;
            }
            if site_count > 0 {
                found.push((name.clone(), pattern, site_count));
            }
        }
    }
    found.sort();
    let expected: Vec<(String, &str, usize)> = vec![
        ("app/scrollbar_visibility.rs".into(), "mouse_near_right_edge = ", 3),
        ("app/scrollbar_visibility.rs".into(), "note_activity(", 3),
        ("app/scrollbar_visibility.rs".into(), "scrollbar_drag = ", 1),
        ("app/scrollbar_visibility.rs".into(), "scrollbar_drag.take()", 1),
    ];
    assert_eq!(found, expected);
}

// ── Every retarget that starts an animation wakes its window ──────

/// A window whose scrollbar thumb drag has settled: the bar is shown, its
/// activity is two seconds old, and no frame is in flight. Returns the app,
/// the window, its pane and the redraw causes before the next event.
fn settled_drag(child: bool) -> (App, WindowId, u64, super::super::redraw::CauseSnapshot) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.config.appearance.scrollbar = ScrollbarMode::Auto;
    let main_pane = app.__test_seed_tab("drag");
    let (window_id, pane) = if child {
        let id = app.__test_seed_child_window(&["drag"]);
        (id, app.__test_child_active_pane(id).expect("child pane"))
    } else {
        (app.main_window_id.expect("seeded main window"), main_pane)
    };
    let started = at(2, Instant::now());
    let window = app.windows.get_mut(&window_id).unwrap();
    window.begin_scrollbar_drag(drag_state(pane), ScrollbarMode::Auto, started);
    window.note_scrollbar_activity(pane, ScrollbarMode::Auto, started);
    // The fade-in finished long ago and its last frame presented.
    window.scrollbar_vis.get_mut(&pane).unwrap().alpha = 1.0;
    window.redraw.request_in_flight = false;
    let before = window.redraw.snapshot();
    (app, window_id, pane, before)
}

/// Run the frames the render path runs while `is_animating` holds: the same
/// update the frame calls, at 60 Hz from `first`, with the pointer over text.
fn frames_until_settled(app: &mut App, window_id: WindowId, pane: u64, first: Instant) {
    let window = app.windows.get_mut(&window_id).unwrap();
    let rect = (pane, 0.0, 30.0, 800.0, 570.0);
    for frame_count in 0..60u32 {
        let state = window.scrollbar_vis[&pane];
        if !is_animating(&state, ScrollbarMode::Auto, ScrollbarMotion::Animated) {
            return;
        }
        let frame_at = first + FRAME * frame_count;
        let mode = ScrollbarMode::Auto;
        update_and_collect(
            &mut window.scrollbar_vis,
            &[rect],
            AWAY,
            pane,
            None,
            mode,
            ScrollbarMotion::Animated,
            frame_at,
        );
    }
    panic!("the fade never settled: {:?}", window.scrollbar_vis[&pane]);
}

#[test]
fn releasing_a_settled_drag_over_text_requests_the_fade_frame_in_main_and_child() {
    // The reviewer's sequence: a drag held past the idle window is released
    // over plain text with nothing else pending. The release retargets the bar
    // to hidden while it is still drawn, so it must ask its own window for a
    // frame, or the bar stays shown. Both release handlers end the drag through
    // `end_scrollbar_drag`; the handlers themselves need a native event loop.
    for child in [false, true] {
        let (mut app, window_id, pane, before) = settled_drag(child);
        let release = Instant::now();
        assert!(app
            .windows
            .get_mut(&window_id)
            .unwrap()
            .end_scrollbar_drag(ScrollbarMode::Auto, release));
        let window = &app.windows[&window_id];
        let state = window.scrollbar_vis[&pane];
        assert_eq!((state.alpha, state.target), (1.0, 0.0), "child {child}");
        assert!(window.redraw.request_in_flight, "child {child}: the release requested a frame");
        assert_ne!(window.redraw.snapshot(), before, "child {child}: a scrollbar cause is marked");
        frames_until_settled(&mut app, window_id, pane, release + FRAME);
        assert_eq!(app.windows[&window_id].scrollbar_vis[&pane].alpha, 0.0, "child {child}");
    }
    for handler in [include_str!("window_pointer.rs"), include_str!("child_window_pointer.rs")] {
        assert!(handler.replace("\r\n", "\n").contains(".end_scrollbar_drag("));
    }
}

#[test]
fn focus_loss_and_a_drag_cancel_request_the_fade_frame() {
    // Both end a settled drag without a release; each must wake the window.
    for cancel in [false, true] {
        let (mut app, window_id, pane, before) = settled_drag(false);
        if cancel {
            app.cancel_drag_session();
        } else {
            app.handle_window_focus_changed(window_id, false);
        }
        let window = &app.windows[&window_id];
        assert!(window.scrollbar_drag.is_none(), "cancel {cancel}");
        assert_eq!(window.scrollbar_vis[&pane].target, 0.0, "cancel {cancel}");
        assert!(window.redraw.request_in_flight, "cancel {cancel}: a frame was requested");
        assert_ne!(window.redraw.snapshot(), before, "cancel {cancel}");
    }
}

#[test]
fn an_overdue_expiry_reports_a_pane_still_fading_toward_an_earlier_target() {
    // The target already went to 0 (by a release or a frame) but no frame
    // ran. The overdue deadline must still report the pane as needing a frame.
    let active = Instant::now();
    let mut state = settled_visible(active);
    let late = active + IDLE + Duration::from_millis(200);
    assert!(retarget(&mut state, ScrollbarMode::Auto, false, late));
    let mut vis = HashMap::from([(1, state)]);
    assert!(expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Animated, late));
    assert!(vis[&1].idle_consumed);
    assert!(!expire_due_idle(&mut vis, ScrollbarMode::Auto, None, ScrollbarMotion::Animated, late));
}

#[test]
fn every_scrollbar_retarget_caller_wakes_its_window() {
    // A retarget can leave alpha short of its target with no frame coming.
    // The pure helpers that retarget may be called only from other pure helpers
    // in this module, from window methods that then call
    // `wake_scrollbar_if_animating`, or from the three paths that request their
    // own frame: idle expiry (the deadline service) and the two render paths.
    const RETARGETING: [&str; 7] = [
        "retarget(",
        "retarget_panes(",
        "note_pane_activity(",
        "update_hover_states(",
        "clear_hover_states(",
        "expire_due_idle(",
        "update_and_collect(",
    ];
    const OWN_FRAME: [(&str, &str); 3] = [
        ("app/event_loop.rs", "expire_due_idle("),
        ("app/window_event.rs", "update_and_collect("),
        ("app/child_window_redraw.rs", "update_and_collect("),
    ];
    let mut method_sites = Vec::new();
    for (name, text) in production_sources() {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || line.contains("fn ") {
                continue;
            }
            for callee in RETARGETING {
                let hit = line.match_indices(callee).any(|(at, _)| {
                    !line[..at].ends_with(|character: char| {
                        character.is_alphanumeric() || character == '_'
                    })
                });
                if !hit {
                    continue;
                }
                let opener_at = lines[..index].iter().rposition(|prior| opens_function(prior));
                let opener = opener_at.map_or("", |at| lines[at]);
                let free_function = !opener.starts_with(' ');
                if OWN_FRAME.contains(&(name.as_str(), callee)) {
                    continue;
                }
                if name == "app/scrollbar_visibility.rs" && free_function {
                    continue;
                }
                assert!(!free_function, "{name}:{}: `{callee}` outside a window method", index + 1);
                let end = lines[index + 1..]
                    .iter()
                    .position(|next| opens_function(next))
                    .map_or(lines.len(), |offset| index + 1 + offset);
                let body = lines[opener_at.unwrap()..end].join("\n");
                assert!(
                    body.contains("wake_scrollbar_if_animating()"),
                    "{name}:{}: `{callee}` in a method that never wakes its window",
                    index + 1
                );
                method_sites.push(format!("{name}:{callee}"));
            }
        }
    }
    method_sites.sort();
    assert_eq!(
        method_sites,
        [
            "app/scrollbar_visibility.rs:clear_hover_states(",
            "app/scrollbar_visibility.rs:note_pane_activity(",
            "app/scrollbar_visibility.rs:retarget_panes(",
            "app/scrollbar_visibility.rs:update_hover_states(",
        ]
    );
}

// ── Edge-band crossings ask for one frame each way ─────────────────

/// An app whose main window (or one child window) has one pane laid out
/// headlessly with Auto scrollbars. Returns the app, the window, the pane and
/// the pane's rect.
fn edge_band_app(child: bool) -> (App, WindowId, u64, sonicterm_ui::pane::Rect) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.config.appearance.scrollbar = ScrollbarMode::Auto;
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 480.0);
    let main_pane = app.__test_seed_tab("edge");
    let (window_id, pane) = if child {
        let id = app.__test_seed_child_window(&["edge"]);
        app.windows.get_mut(&id).unwrap().test_pane_viewport = Some((outer, 10.0, 20.0));
        (id, app.__test_child_active_pane(id).expect("child pane"))
    } else {
        app.__test_set_main_pane_viewport(outer, 10.0, 20.0);
        (app.main_window_id.expect("seeded main window"), main_pane)
    };
    let rects = if child {
        App::compute_pane_rects_for(&app.windows[&window_id])
    } else {
        app.compute_active_pane_rects()
    };
    let rect = rects.into_iter().find_map(|(id, rect)| (id == pane).then_some(rect)).unwrap();
    (app, window_id, pane, rect)
}

/// Move the window's pointer to `point` and run the production hover refresh
/// for that window; returns how many redraw asks it made and whether it crossed.
fn hover_to(app: &mut App, window_id: WindowId, child: bool, point: (f32, f32)) -> (u64, bool) {
    app.windows.get_mut(&window_id).unwrap().cursor_pos = (f64::from(point.0), f64::from(point.1));
    let before = crate::app::window_state::window_redraw_requests();
    let crossed = if child {
        app.refresh_scrollbar_hover_from_cursor_in_child(window_id)
    } else {
        app.refresh_scrollbar_hover_from_cursor()
    };
    (crate::app::window_state::window_redraw_requests() - before, crossed)
}

/// Stand in for the frame that presented: alpha reached its target and the
/// window has no request in flight.
fn present(app: &mut App, window_id: WindowId, pane: u64) {
    let window = app.windows.get_mut(&window_id).unwrap();
    let state = window.scrollbar_vis.get_mut(&pane).unwrap();
    state.alpha = state.target;
    window.redraw.request_in_flight = false;
}

#[test]
fn crossing_the_edge_band_asks_for_one_redraw_each_way_in_main_and_child() {
    // The redraw-request counters measure this path, so an edge crossing must
    // ask once, through the window's scrollbar wake, not once more through a
    // second request. Leaving after the idle window starts the fade: one ask.
    // Leaving within it keeps the bar shown, draws nothing new, and asks nothing.
    for child in [false, true] {
        let (mut app, window_id, pane, rect) = edge_band_app(child);
        let row_y = rect.y + rect.h / 2.0;
        let edge = (rect.x + rect.w - 5.0, row_y);
        let away = (rect.x + rect.w / 2.0, row_y);
        assert_eq!(hover_to(&mut app, window_id, child, away), (0, false), "child {child}");

        assert_eq!(hover_to(&mut app, window_id, child, edge), (1, true), "child {child}: entry");
        present(&mut app, window_id, pane);
        let state = app.windows.get_mut(&window_id).unwrap().scrollbar_vis.get_mut(&pane).unwrap();
        state.last_active = Some(at(2, Instant::now()));
        assert_eq!(hover_to(&mut app, window_id, child, away), (1, true), "child {child}: exit");
        assert_eq!(app.windows[&window_id].scrollbar_vis[&pane].target, 0.0);

        present(&mut app, window_id, pane);
        assert_eq!(
            hover_to(&mut app, window_id, child, edge),
            (1, true),
            "child {child}: re-entry"
        );
        present(&mut app, window_id, pane);
        assert_eq!(
            hover_to(&mut app, window_id, child, away),
            (0, true),
            "child {child}: an exit within the idle window changes nothing drawn"
        );
    }
}
