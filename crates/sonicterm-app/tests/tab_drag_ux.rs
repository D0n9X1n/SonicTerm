//! Integration tests for the commit-on-release tab-drag UX.
//!
//! These exercise the pure `compute_action` helper that the App
//! consults on mouse-up. The four user-visible scenarios in the spec
//! are locked in here; the event-loop glue is covered by the existing
//! tear-out / merge integration tests.

use sonicterm_app::tab_drag::{
    compute_action, find_drop_target, DragAction, DragSession, DropTarget, WindowGeom,
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

fn src_layout(n: usize) -> TabBarLayout {
    TabBarLayout::compute(&synth_bar(n), 800.0)
}

#[test]
fn scenario1_drag_below_then_back_over_bar_releases_as_noop() {
    let layout = src_layout(3);
    let mut s = DragSession::new(1, (200.0, 10.0));

    // Drag well below the bar — would be a tear if released here.
    s.current_pos = (200.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 50.0);
    assert!(matches!(
        compute_action::<&str>(&s, None, &layout),
        DragAction::TearOutToNewWindow { .. }
    ));

    // Move BACK over the original bar — release is now a no-op.
    s.current_pos = (220.0, 8.0);
    let a: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(a, DragAction::ReturnToOriginalBar);
}

#[test]
fn scenario2_drag_below_release_below_tears_out() {
    let layout = src_layout(3);
    let mut s = DragSession::new(1, (200.0, 10.0));
    s.current_pos = (250.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &layout);
    match a {
        DragAction::TearOutToNewWindow { drop_local } => {
            assert!((drop_local.0 - 250.0).abs() < f32::EPSILON);
        }
        other => panic!("expected tear-out, got {other:?}"),
    }
}

#[test]
fn scenario3_drag_to_other_window_bar_release_merges_at_slot() {
    let src = src_layout(3);

    // Destination window has 3 tabs at global x in [1000..1800].
    let dest_bar = synth_bar(3);
    let dest_layout = TabBarLayout::compute(&dest_bar, 800.0);
    let dest_geom = WindowGeom::new((1000, 0), (800, 600));

    let mut s = DragSession::new(0, (50.0, 10.0));
    s.current_pos = (50.0, 999.0); // foreign target wins regardless
    let target = find_drop_target((1300, 10), vec![("dest", dest_geom, dest_layout)])
        .expect("cursor over dest bar");
    assert_eq!(target.window, "dest");
    let _ = DropTarget { window: "dest", slot: target.slot }; // silence unused

    let a = compute_action(&s, Some(target), &src);
    assert_eq!(a, DragAction::MergeIntoWindow(target));
}

#[test]
fn bonus_release_between_tabs_of_source_bar_reorders_not_tears() {
    // Pick an x that's still over the SOURCE bar (within bar y range),
    // landed past tab #1's right edge → slot resolves to the last tab.
    // The important guarantees: NO TEAR, and (now that reorder is
    // wired up) the action is ReorderTab from the press index to the
    // new slot.
    let layout = src_layout(3);
    let mut s = DragSession::new(0, (50.0, 10.0));
    let between_x = layout.tabs[1].bg.x + layout.tabs[1].bg.w + 1.0;
    s.current_pos = (between_x, 12.0);
    let a: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(a, DragAction::ReorderTab { from: 0, to: 2 });
}

#[test]
fn just_below_bar_within_hysteresis_releases_as_noop() {
    // A tiny slip below the bar must NOT tear out — same intent as
    // scenario 1, exercises the lower edge of the threshold.
    let layout = src_layout(3);
    let mut s = DragSession::new(1, (100.0, 10.0));
    s.current_pos = (120.0, TAB_BAR_HEIGHT + 5.0);
    let a: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(a, DragAction::ReturnToOriginalBar);
}

#[test]
fn reorder_press_tab2_drop_on_tab0_swaps_in_place() {
    // Press tab #2, drag the cursor left until it's over tab #0's
    // left half → release. The bar reorders so the formerly-2nd tab
    // is now first, and (per TabBar::reorder) the active tab follows
    // the moved tab. Critically: no child window is spawned, no
    // tear-out fires, and the action is purely a within-bar reorder.
    let mut bar = TabBar::new();
    bar.push(Tab::new("A"));
    bar.push(Tab::new("B"));
    bar.push(Tab::new("C")); // active = 2 after the third push
    assert_eq!(bar.active_index(), 2);
    let titles_before: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles_before, vec!["A", "B", "C"]);

    let layout = TabBarLayout::compute(&bar, 800.0);
    // Press at the middle of tab 2, move cursor to x=10 (well left of
    // tab 0's midpoint). y stays inside the bar.
    let mut s = DragSession::new(2, (layout.tabs[2].bg.x + layout.tabs[2].bg.w / 2.0, 10.0));
    s.current_pos = (10.0, 10.0);
    let action: DragAction<&str> = compute_action(&s, None, &layout);
    assert_eq!(action, DragAction::ReorderTab { from: 2, to: 0 });

    // Execute the reorder the way the app's match arm does.
    if let DragAction::ReorderTab { from, to } = action {
        bar.reorder(from, to);
    }
    let titles_after: Vec<&str> = bar.tabs().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles_after, vec!["C", "A", "B"]);
    // Active follows the moved tab.
    assert_eq!(bar.active_index(), 0);
}
