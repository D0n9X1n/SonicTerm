//! Regression tests for within-bar tab drag-reorder (the Chrome /
//! Firefox / Terminal.app gesture).
//!
//! User report (2026-05): "drag tab horizontally within the same tab
//! strip should reorder it among siblings; currently dragging does
//! nothing or tears out prematurely." This file locks in the three
//! canonical scenarios from that bug:
//!
//! 1. Drag the active tab one slot to the right → tabs reorder, active
//!    follows the moved tab.
//! 2. Drag the rightmost tab all the way to slot 0 → tabs reorder,
//!    active follows.
//! 3. Drag downward past the tear-out hysteresis → NO reorder, the
//!    action resolves to `TearOutToNewWindow` (proves the within-bar
//!    branch and the tear-out branch are mutually exclusive).
//!
//! The pure helper `compute_action` is the source of truth — the
//! app's mouse-up arm in `app/window_event.rs` is a thin match that
//! either calls `TabBar::reorder` or `tear_out_tab`, so exercising
//! `compute_action` + `TabBar::reorder` covers the real code path.

use sonicterm_app::tab_drag::{compute_action, DragAction, DragSession};
use sonicterm_ui::tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT, TEAR_OUT_THRESHOLD_PX};
use sonicterm_ui::tabs::{Tab, TabBar};

fn three_tab_bar() -> TabBar {
    let mut b = TabBar::new();
    b.push(Tab::new("A"));
    b.push(Tab::new("B"));
    b.push(Tab::new("C"));
    // Most recently pushed tab becomes active; force-activate 0 so the
    // first scenario matches the user-reported setup ("active = A").
    b.activate(0);
    assert_eq!(b.active_index(), 0);
    b
}

#[test]
fn drag_tab0_onto_tab1_left_half_reorders_to_b_a_c_with_active_following() {
    let mut bar = three_tab_bar();
    let layout = TabBarLayout::compute(&bar, 800.0);

    // Press in the middle of tab 0; drag right to a point clearly
    // inside tab 1's LEFT half (i.e. left of its midpoint). That is
    // the slot at which Chrome/Firefox swap the dragged tab past tab 1.
    let press_x = layout.tabs[0].bg.x + layout.tabs[0].bg.w / 2.0;
    let tab1_left_third = layout.tabs[1].bg.x + layout.tabs[1].bg.w * 0.3;
    let mut s = DragSession::new(0, (press_x, 10.0));
    s.current_pos = (tab1_left_third, 10.0);

    let action: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(
        action,
        DragAction::ReorderTab { from: 0, to: 1 },
        "press-tab-0 + drag onto tab-1's left half must reorder 0 → 1"
    );

    if let DragAction::ReorderTab { from, to } = action {
        bar.reorder(from, to);
    }
    let titles: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["B", "A", "C"]);
    assert_eq!(bar.active_index(), 1, "active tab must follow its tab to slot 1");
}

#[test]
fn drag_last_tab_all_the_way_left_reorders_to_c_a_b_with_active_following() {
    let mut bar = three_tab_bar();
    // For this scenario the user dragged the LAST tab — make it active
    // so we can prove the active index tracks the moved tab.
    bar.activate(2);
    assert_eq!(bar.active_index(), 2);

    let layout = TabBarLayout::compute(&bar, 800.0);
    let press_x = layout.tabs[2].bg.x + layout.tabs[2].bg.w / 2.0;
    let mut s = DragSession::new(2, (press_x, 10.0));
    // Drop on tab 0's left half — clearly slot 0.
    s.current_pos = (layout.tabs[0].bg.x + 2.0, 10.0);

    let action: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(
        action,
        DragAction::ReorderTab { from: 2, to: 0 },
        "press-tab-2 + drag to slot 0 must reorder 2 → 0"
    );

    if let DragAction::ReorderTab { from, to } = action {
        bar.reorder(from, to);
    }
    let titles: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["C", "A", "B"]);
    assert_eq!(bar.active_index(), 0, "active tab must follow its tab to slot 0");
}

#[test]
fn drag_tab0_past_tab2_reorders_to_b_c_a() {
    let mut bar = three_tab_bar();
    let layout = TabBarLayout::compute(&bar, 800.0);

    let press_x = layout.tabs[0].bg.x + layout.tabs[0].bg.w / 2.0;
    let past_tab2 = layout.tabs[2].bg.x + layout.tabs[2].bg.w + 8.0;
    let mut s = DragSession::new(0, (press_x, 10.0));
    s.current_pos = (past_tab2, 10.0);

    let action: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(action, DragAction::ReorderTab { from: 0, to: 2 });

    if let DragAction::ReorderTab { from, to } = action {
        bar.reorder(from, to);
    }
    let titles: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, vec!["B", "C", "A"]);
    assert_eq!(bar.active_index(), 2, "active tab must follow its tab past tab 2");
}

#[test]
fn drag_past_tear_out_threshold_does_not_reorder() {
    // Same press, but the cursor leaves the bar vertically past the
    // tear-out hysteresis. Within-bar reorder must NOT fire; the
    // action must resolve to a tear-out so the existing tear-out path
    // (not the reorder path) handles release.
    let bar = three_tab_bar();
    let layout = TabBarLayout::compute(&bar, 800.0);
    let press_x = layout.tabs[0].bg.x + layout.tabs[0].bg.w / 2.0;
    let mut s = DragSession::new(0, (press_x, 10.0));
    // Move horizontally too (so we're not under the press), and well
    // below the bar past the hysteresis.
    s.current_pos = (press_x + 80.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 20.0);

    let action: DragAction<&str> = compute_action(&s, None, &layout);
    match action {
        DragAction::TearOutToNewWindow { .. } => {}
        other => panic!("expected TearOutToNewWindow, got {other:?}; reorder must not fire"),
    }
}
