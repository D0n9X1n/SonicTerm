//! Tests for `tab_drag::compute_action` + helpers.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/tab_drag.rs`.
//! Existing `tests/tab_drag_*.rs` files cover end-to-end UX scenarios;
//! this file is the unit-level companion.

use sonicterm_app::tab_drag::{
    compute_action, find_drop_target, global_to_local, local_to_global, DragAction, DragSession,
    DropTarget, WindowGeom,
};
use sonicterm_ui::tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT, TEAR_OUT_THRESHOLD_PX};
use sonicterm_ui::tabs::{Tab, TabBar};

fn synth_bar(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

#[test]
fn local_to_global_offsets_correctly() {
    assert_eq!(local_to_global((100, 50), (10.0, 20.0)), (110, 70));
    assert_eq!(local_to_global((100, 50), (-5.0, 200.0)), (95, 250));
}

#[test]
fn global_to_local_rejects_outside() {
    let g = WindowGeom::new((200, 100), (800, 600));
    assert_eq!(global_to_local(g, (199, 200)), None);
    assert_eq!(global_to_local(g, (1000, 200)), None);
    assert_eq!(global_to_local(g, (300, 99)), None);
    assert_eq!(global_to_local(g, (300, 700)), None);
    assert_eq!(global_to_local(g, (200, 100)), Some((0.0, 0.0)));
    assert_eq!(global_to_local(g, (999, 699)), Some((799.0, 599.0)));
}

#[test]
fn drop_target_picks_window_under_cursor() {
    let bar_a = synth_bar(3);
    let layout_a = TabBarLayout::compute(&bar_a, 800.0);
    let geom_a = WindowGeom::new((0, 0), (800, 600));

    let bar_b = synth_bar(2);
    let layout_b = TabBarLayout::compute(&bar_b, 800.0);
    let geom_b = WindowGeom::new((1000, 0), (800, 600));

    let candidates = vec![("a", geom_a, layout_a), ("b", geom_b, layout_b)];
    let t = find_drop_target((1100, 10), candidates).expect("hits b");
    assert_eq!(t.window, "b");
}

#[test]
fn drop_target_none_when_no_window_underneath() {
    let bar = synth_bar(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let geom = WindowGeom::new((0, 0), (800, 600));
    assert!(find_drop_target((2000, 2000), vec![("a", geom, layout)]).is_none());
}

#[test]
fn drop_target_none_when_cursor_below_bar_in_window() {
    let bar = synth_bar(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let geom = WindowGeom::new((0, 0), (800, 600));
    assert!(find_drop_target((100, 400), vec![("a", geom, layout)]).is_none());
}

#[test]
fn drop_slot_at_end_of_bar() {
    let bar = synth_bar(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let geom = WindowGeom::new((0, 0), (800, 600));
    let t = find_drop_target((700, 10), vec![("a", geom, layout)]).expect("over bar");
    assert_eq!(t.slot, 2);
}

fn src_layout() -> TabBarLayout {
    TabBarLayout::compute(&synth_bar(3), 800.0)
}

#[test]
fn action_returns_to_original_bar_when_cursor_over_source() {
    let mut s = DragSession::new(1, (300.0, 10.0));
    s.current_pos = (300.0, 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &src_layout());
    assert_eq!(a, DragAction::ReturnToOriginalBar);
}

#[test]
fn action_returns_to_bar_when_just_below_bar_within_hysteresis() {
    let mut s = DragSession::new(1, (100.0, 10.0));
    s.current_pos = (120.0, TAB_BAR_HEIGHT + 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &src_layout());
    assert_eq!(a, DragAction::ReturnToOriginalBar);
}

#[test]
fn action_tears_out_when_well_below_bar() {
    let mut s = DragSession::new(1, (100.0, 10.0));
    s.current_pos = (120.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 1.0);
    let a: DragAction<&str> = compute_action(&s, None, &src_layout());
    assert!(matches!(a, DragAction::TearOutToNewWindow { .. }));
}

#[test]
fn action_merges_when_foreign_target_set_even_if_cursor_far_below() {
    let mut s = DragSession::new(1, (100.0, 10.0));
    s.current_pos = (500.0, 999.0);
    let target = DropTarget { window: "b", slot: 2 };
    let a = compute_action(&s, Some(target), &src_layout());
    assert_eq!(a, DragAction::MergeIntoWindow(target));
}

#[test]
fn action_drag_below_then_back_over_bar_cancels() {
    let mut s = DragSession::new(1, (100.0, 10.0));
    s.current_pos = (120.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 50.0);
    assert!(matches!(
        compute_action::<&str>(&s, None, &src_layout()),
        DragAction::TearOutToNewWindow { .. }
    ));
    s.current_pos = (140.0, 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &src_layout());
    assert_eq!(a, DragAction::ReturnToOriginalBar);
}

#[test]
fn action_reorders_when_cursor_over_different_slot_on_source_bar() {
    let mut s = DragSession::new(2, (500.0, 10.0));
    s.current_pos = (10.0, 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &src_layout());
    assert_eq!(a, DragAction::ReorderTab { from: 2, to: 0 });
}

#[test]
fn action_no_reorder_when_cursor_over_same_slot() {
    let mut s = DragSession::new(1, (300.0, 10.0));
    s.current_pos = (300.0, 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &src_layout());
    assert_eq!(a, DragAction::ReturnToOriginalBar);
}

#[test]
fn action_foreign_target_wins_over_within_bar_reorder() {
    let mut s = DragSession::new(0, (10.0, 10.0));
    s.current_pos = (500.0, 5.0);
    let target = DropTarget { window: "b", slot: 1 };
    let a = compute_action(&s, Some(target), &src_layout());
    assert_eq!(a, DragAction::MergeIntoWindow(target));
}
