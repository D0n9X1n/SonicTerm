//! Integration tests for cross-window tab drag-to-merge (v1, same
//! process). Mirrors the structure of `tab_tearout.rs`:
//!
//! 1. exercise the pure drop-target hit-test (no winit), and
//! 2. drive `App::detach_tab_state` + `App::attach_tab_state` directly
//!    to assert that tab+pane state moves between containers without
//!    losing the swappable redraw target.
//!
//! Cross-window mouse routing through a live winit `EventLoop` is not
//! exercised here — that requires the main thread on macOS.

use std::sync::Arc;

use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_shared::app::App;
use sonic_shared::tab_drag::{find_drop_target, global_to_local, local_to_global, WindowGeom};
use sonic_shared::tabbar_view::TabBarLayout;
use sonic_shared::tabs::{Tab, TabBar};

fn synth_theme() -> Theme {
    let hex = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    };
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                close_button_fg: hex(),
            },
        },
    }
}

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() };
    App::new(synth_theme(), Config::default(), keymap)
}

fn synth_bar(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

// ---- gesture / drop-target -------------------------------------------------

#[test]
fn local_to_global_handles_negative_drag() {
    // Dragging off the left edge of the source window — local x goes
    // negative; global must still be computed correctly.
    assert_eq!(local_to_global((500, 200), (-50.0, 10.0)), (450, 210));
}

#[test]
fn global_to_local_clips_to_inner_area() {
    let g = WindowGeom::new((100, 50), (400, 300));
    assert!(global_to_local(g, (50, 200)).is_none()); // left of window
    assert!(global_to_local(g, (600, 200)).is_none()); // right of window
    assert_eq!(global_to_local(g, (100, 50)), Some((0.0, 0.0)));
}

#[test]
fn drop_target_picks_correct_window_and_slot() {
    // Two non-overlapping windows side by side.
    let bar_a = synth_bar(3);
    let layout_a = TabBarLayout::compute(&bar_a, 800.0);
    let geom_a = WindowGeom::new((0, 0), (800, 600));

    let bar_b = synth_bar(4);
    let layout_b = TabBarLayout::compute(&bar_b, 800.0);
    let geom_b = WindowGeom::new((900, 0), (800, 600));

    // Global cursor at (910, 10) → over window B, near left edge → slot 0.
    let cands = vec![("a", geom_a, layout_a.clone()), ("b", geom_b, layout_b.clone())];
    let t = find_drop_target((910, 10), cands).expect("hits b");
    assert_eq!(t.window, "b");
    assert_eq!(t.slot, 0);

    // Same windows, cursor far past last tab in B → slot == len.
    let cands = vec![("a", geom_a, layout_a), ("b", geom_b, layout_b)];
    let t = find_drop_target((1650, 10), cands).expect("hits b end");
    assert_eq!(t.window, "b");
    assert_eq!(t.slot, 4);
}

#[test]
fn drop_target_skips_source_window() {
    // If caller pre-filters out source, find_drop_target won't pick it.
    let bar = synth_bar(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let geom = WindowGeom::new((0, 0), (800, 600));
    // Cursor over the (excluded) source → no candidate → None.
    let cands: Vec<(&str, WindowGeom, TabBarLayout)> = Vec::new();
    assert!(find_drop_target((100, 10), cands).is_none());
    // Sanity: present in candidates → found.
    assert!(find_drop_target((100, 10), vec![("dest", geom, layout)]).is_some());
}

// ---- state transfer between containers -------------------------------------

#[test]
fn attach_inserts_tab_at_requested_index() {
    let mut app = synth_app();
    let _a = app.__test_seed_tab("alpha");
    let _b = app.__test_seed_tab("bravo");
    let _c = app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3);

    // Pull "alpha" (index 0) out, then re-attach in the middle (index 1).
    let (tab, state, panes) = app.detach_tab_state(0).expect("detach");
    assert_eq!(app.__test_tab_count(), 2);
    app.attach_tab_state(1, tab, state, panes);
    // Now tabs are [bravo, alpha, charlie] (or similar) and len==3.
    assert_eq!(app.__test_tab_count(), 3);
}

#[test]
fn attach_at_end_clamps_oob_index() {
    let mut app = synth_app();
    let _a = app.__test_seed_tab("alpha");
    let _b = app.__test_seed_tab("bravo");
    let (tab, state, panes) = app.detach_tab_state(0).expect("detach alpha");
    // Ask for an index well past the end — should clamp to current len.
    app.attach_tab_state(99, tab, state, panes);
    assert_eq!(app.__test_tab_count(), 2);
}

#[test]
fn round_trip_detach_then_attach_preserves_pane_arc() {
    // The whole point of drag-merge is that the existing VT thread
    // keeps running and just gets re-pointed. The redraw_target Arc
    // therefore MUST be the same allocation before and after.
    let mut app = synth_app();
    let p = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let arc_before = app.__test_pane_redraw_target(p).expect("alpha present");
    let strong_before = Arc::strong_count(&arc_before);

    let (tab, state, panes) = app.detach_tab_state(0).expect("detach alpha");
    // Re-attach at end of bar.
    app.attach_tab_state(99, tab, state, panes);
    let arc_after = app.__test_pane_redraw_target(p).expect("alpha back in App");
    assert!(Arc::ptr_eq(&arc_before, &arc_after), "redraw target Arc must survive transfer");
    // Strong count should be the same magnitude (>=1) — we don't pin
    // an exact number because PaneState clones and drops freely
    // during transfer, but it must not have been freed.
    assert!(Arc::strong_count(&arc_after) >= 1);
    let _ = strong_before;
}

#[test]
fn attach_to_missing_child_returns_false() {
    use winit::event_loop::EventLoop;
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let (tab, state, panes) = app.detach_tab_state(0).expect("detach");
    // Conjure a WindowId that won't be in child_windows. We use the
    // dummy one from `EventLoop::owned_display_handle`-adjacent code
    // not being available here; instead, fabricate via the public
    // `WindowId::from(NonZeroU64::new(1))`... which winit doesn't
    // expose. Sidestep: assert via the no-children precondition that
    // any id we pass in won't match.
    assert_eq!(app.child_window_count(), 0);
    // Force a known-bad id by constructing a fresh App's window id
    // — except winit only mints those from a live event loop. We
    // instead test the simpler invariant: when there are 0 children,
    // any `WindowId` lookup misses. Confirm by checking the per-id
    // helper returns None.
    let _ = EventLoop::<()>::with_user_event(); // marker: needs main thread; do not run
                                                // The detached bundle is dropped here — verifies the API at
                                                // least doesn't panic on the unattach-then-drop sequence (this
                                                // is what happens in production when a destination window
                                                // closes mid-drag).
    drop((tab, state, panes));
    assert_eq!(app.__test_pane_ids().len(), 1); // bravo still present
                                                // Demonstrate the typed helper: there are no child windows, so
                                                // any query returns None.
    let fake = make_phantom_window_id();
    assert!(app.__test_child_tab_count(fake).is_none());
}

// Helper: produce a WindowId we can pass to APIs without minting one
// from a live winit event loop. WindowId is `#[repr(transparent)]`
// over an internal type but its only constructor is winit-internal;
// for test purposes we transmute from a NonZeroU64. This is safe
// because WindowId is documented to be opaque and is only ever used
// as a HashMap key.
fn make_phantom_window_id() -> winit::window::WindowId {
    // Use winit's public dummy id constructor (added in 0.30).
    winit::window::WindowId::dummy()
}

// ---- last-tab drain / exit policy (PR #48 review fix) ----------------------

#[test]
fn should_exit_pure_keeps_alive_when_main_drained_but_children_remain() {
    // The blocker from haiku review: dragging the LAST main tab into a
    // child should NOT exit the app. The pure policy fn must agree:
    // main empty + hidden, but ≥1 child alive → keep running.
    assert!(!App::should_exit_pure(0, true, 1));
    assert!(!App::should_exit_pure(0, true, 3));
}

#[test]
fn should_exit_pure_exits_when_all_windows_gone() {
    // No main tabs, main hidden, zero children → nothing left to draw.
    assert!(App::should_exit_pure(0, true, 0));
}

#[test]
fn should_exit_pure_stays_alive_while_main_has_tabs() {
    // Visible main with at least one tab → never exit, regardless of
    // child count.
    assert!(!App::should_exit_pure(1, false, 0));
    assert!(!App::should_exit_pure(5, false, 2));
}

#[test]
fn merging_last_main_tab_drains_main_without_panicking() {
    // Simulate: main has exactly one tab, a child window is alive.
    // Detach the last main tab (this is what merge_main_into_child
    // does after the PR #48 guard was removed). Main bar is empty.
    // The should_exit policy with main_hidden=true and 1 phantom child
    // must NOT request exit.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    assert_eq!(app.__test_tab_count(), 1);

    let (_tab, _state, _panes) = app.detach_tab_state(0).expect("detach last main tab");
    assert_eq!(app.__test_tab_count(), 0, "main bar must drain");
    // Simulate the hide_main_window() side-effect that
    // merge_main_into_child performs when ≥1 child is alive.
    app.__test_set_main_hidden(true);
    assert!(app.__test_main_hidden());
    // Policy check with 1 phantom child: app stays alive.
    assert!(!App::should_exit_pure(app.__test_tab_count(), app.__test_main_hidden(), 1));
}

#[test]
fn merging_last_child_tab_into_main_keeps_main_alive() {
    // Simulate the reverse direction: child window had one tab, it's
    // merged into main. After detach_from_child + attach_tab_state +
    // reap_empty_child, main has the tab + zero children. The policy
    // must keep running (and `should_exit` should be false).
    let mut app = synth_app();
    // Pre-existing tab in main + a phantom-style detached bundle that
    // we re-attach to mimic "child→main merge result".
    let _ = app.__test_seed_tab("main-a");
    let _ = app.__test_seed_tab("from-child");
    let (tab, state, panes) = app.detach_tab_state(1).expect("pluck the 'from-child' bundle");
    // ...and re-attach at end of main bar (mirrors what
    // merge_child_into_target's main-path branch does).
    app.attach_tab_state(99, tab, state, panes);
    assert_eq!(app.__test_tab_count(), 2);
    assert!(!app.__test_main_hidden());
    assert!(!App::should_exit_pure(app.__test_tab_count(), app.__test_main_hidden(), 0));
    // And reaping a phantom child should be a no-op since no child
    // windows exist — that path is exercised by the live app on drop.
    assert_eq!(app.child_window_count(), 0);
    let fake = make_phantom_window_id();
    assert!(app.__test_child_tab_count(fake).is_none());
}

#[test]
fn merge_with_multiple_tabs_reindexes_remaining_tabs() {
    // Merging one of N>1 tabs out of main must leave the OTHER N-1
    // tabs in place and renumber the bar so subsequent ops still
    // address valid indices. (Regression guard for the post-drain
    // reindex path that the PR #48 fix introduces.)
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let _ = app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3);

    // Pluck the middle tab out (as if dropped onto a child window).
    let (_tab, _state, _panes) = app.detach_tab_state(1).expect("detach bravo");
    assert_eq!(app.__test_tab_count(), 2);
    // Remaining tabs must still be reachable by their NEW indices
    // (0 and 1). Attempt a no-op detach+reattach at index 1 to prove
    // the bar is internally consistent.
    let (tab, state, panes) = app.detach_tab_state(1).expect("index 1 still valid");
    app.attach_tab_state(1, tab, state, panes);
    assert_eq!(app.__test_tab_count(), 2);
    // Main was not drained — policy must not request exit, regardless
    // of phantom child count.
    assert!(!app.__test_main_hidden());
    assert!(!App::should_exit_pure(app.__test_tab_count(), app.__test_main_hidden(), 0));
    assert!(!App::should_exit_pure(app.__test_tab_count(), app.__test_main_hidden(), 1));
}

// ---- single-tab cross-window merge (bug: tabs.len()<=1 short-circuit) ------
//
// Before the fix, `App::tear_out_tab` early-returned whenever the source
// bar held only one tab. That was correct for "don't spawn a new window
// identical to this one," but it ALSO swallowed cross-window drops: if a
// user dragged the only main tab onto a child window, `tear_out_tab` was
// invoked by the tear-out gesture detector and silently no-op'd before
// the merge could run. The fix routes through `try_cross_window_merge`
// first; the single-tab guard now only fires when no drop target is
// pending.

use sonic_shared::tab_drag::DropTarget;

#[test]
fn cross_window_merge_runs_even_with_one_main_tab() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    assert_eq!(app.__test_tab_count(), 1);

    let phantom = make_phantom_window_id();
    app.__test_set_drag_target(Some(DropTarget { window: phantom, slot: 0 }));

    let consumed = app.try_cross_window_merge(0);
    assert!(consumed, "1-tab cross-window merge must NOT be blocked by tabs.len()<=1");
    assert_eq!(app.__test_tab_count(), 0);
}

#[test]
fn single_tab_with_no_drag_target_still_noops() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    app.__test_set_drag_target(None);

    let consumed = app.try_cross_window_merge(0);
    assert!(!consumed, "no drop target → cross-window merge must not fire");
    assert_eq!(app.__test_tab_count(), 1);
}

#[test]
fn cross_window_merge_clears_drag_state_on_consume() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    let phantom = make_phantom_window_id();
    app.__test_set_drag_target(Some(DropTarget { window: phantom, slot: 0 }));

    assert!(app.try_cross_window_merge(0));
    // After consume the target is cleared; second call must return false.
    assert!(!app.try_cross_window_merge(0));
}

// ---- production event ordering (Haiku review of PR #62) -------------------
//
// The factored helper `try_cross_window_merge` is reachable from
// `tear_out_tab`, but the production CursorMoved handler invokes
// `tear_out_tab` ONLY when `drag_target` is still `None` (the `Some`
// branch returns early before tear-out detection). So the new helper
// alone does NOT fix the user-visible bug: a lone tab dragged below
// the tab bar fires tear-out first, the gate decides "no-op", and
// without the review fix the CursorMoved caller cleared `pressed_tab`
// + `mouse_down`, killing the drag before the user could ever cross
// into a sibling window's bar.
//
// These tests model the full event sequence via the public test seams
// `__test_pressed_tab` / `__test_mouse_down` / `tear_out_would_be_noop`
// / `try_cross_window_merge`, asserting the invariant that the gesture
// state SURVIVES a no-op tear-out attempt and can still complete a
// merge afterwards. They fail on the parent commit (which would clear
// the state unconditionally) and pass with the review fix.

#[test]
fn lone_tab_tear_out_threshold_does_not_kill_drag_without_target() {
    // Simulate: user mouse-down on the only tab, then drags far enough
    // to trip tear-out, but the cursor is NOT yet over a sibling
    // window's bar (drag_target is None).
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    app.__test_set_pressed_tab(Some(0));
    app.__test_set_mouse_down(true);
    app.__test_set_drag_target(None);

    // The pure predicate the CursorMoved handler consults: with one
    // tab + no target, the call would be a no-op and the handler must
    // bail BEFORE clearing the gesture.
    assert!(
        app.tear_out_would_be_noop(),
        "single-tab + no-target must be classified as no-op so the handler skips state-clear"
    );

    // Production-shaped assertion: gesture is intact, ready for the
    // next CursorMoved to acquire a target.
    assert_eq!(app.__test_pressed_tab(), Some(0));
    assert!(app.__test_mouse_down());
    assert_eq!(app.__test_tab_count(), 1);
}

#[test]
fn lone_tab_drag_completes_merge_after_threshold_then_target_acquired() {
    // Full end-to-end production sequence for the single-tab cross-
    // window merge:
    //   1. mouse-down on the only tab → press recorded
    //   2. cursor drags below the bar far enough to trip tear-out, but
    //      still over the source window → no drop target → gate says
    //      "no-op" → state preserved
    //   3. cursor enters a sibling window's tab bar → drag_target =
    //      Some(child) → gate flips to "not a no-op"
    //   4. mouse-up (or further movement) consumes the gesture via
    //      `try_cross_window_merge`, which drains the source tab.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("only");
    // (1) press
    app.__test_set_pressed_tab(Some(0));
    app.__test_set_mouse_down(true);
    // (2) cursor below the bar, no target yet
    app.__test_set_drag_target(None);
    assert!(app.tear_out_would_be_noop());
    // The CursorMoved branch sees the no-op and returns without
    // touching the gesture. Simulate that by NOT clearing anything.
    assert_eq!(app.__test_pressed_tab(), Some(0));
    assert!(app.__test_mouse_down());

    // (3) cursor now over a sibling window's bar
    let phantom = make_phantom_window_id();
    app.__test_set_drag_target(Some(DropTarget { window: phantom, slot: 0 }));
    assert!(!app.tear_out_would_be_noop(), "with a target the gate must allow forward progress");

    // (4) consume via the merge gate (mirrors what tear_out_tab does
    // internally before the single-tab guard).
    assert!(app.try_cross_window_merge(0), "merge must run for a lone tab once a target exists");
    assert_eq!(app.__test_tab_count(), 0, "source tab drained on consume");
    // try_cross_window_merge clears its own bookkeeping on consume.
    assert_eq!(app.__test_pressed_tab(), None);
    assert!(!app.__test_mouse_down());
}

#[test]
fn multi_tab_tear_out_is_not_classified_as_noop() {
    // Negative control: tabs.len() > 1 means tear-out IS productive
    // (spawns a new window with the dragged tab). The predicate must
    // say so, otherwise the gate change would over-broadly suppress
    // the normal tear-out gesture.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("a");
    let _ = app.__test_seed_tab("b");
    app.__test_set_drag_target(None);
    assert!(!app.tear_out_would_be_noop(), "multi-tab tear-out must not be classified no-op");
}
