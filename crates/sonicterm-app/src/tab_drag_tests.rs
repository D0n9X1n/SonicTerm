use super::*;
use sonicterm_ui::tabbar_view::{Point, TabAction, TabHit, TAB_BAR_HEIGHT, TEAR_OUT_THRESHOLD_PX};
use sonicterm_ui::tabs::{Tab, TabBar};

fn source_layout() -> TabBarLayout {
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("first"));
    tabs.push(Tab::new("second"));
    tabs.activate(0);
    TabBarLayout::compute(&tabs, 600.0)
}

#[test]
fn subthreshold_click_does_not_merge_into_an_overlapping_window() {
    // A duplicate move or click jitter may hit a background bar without starting a drag.
    let source_bar = source_layout();
    let origin = (100, 100);
    for delta in [0.0, 1.0, DRAG_START_THRESHOLD_PX - 0.25] {
        let mut session = DragSession::new(0, (12.0, TAB_BAR_HEIGHT * 0.5));
        session.current_pos.0 += delta;
        assert!(!drag_moved_enough(&session));
        assert_eq!(
            compute_action::<u64>(&session, None, &source_bar),
            DragAction::ReturnToOriginalBar
        );

        let global = local_to_global(
            origin,
            (f64::from(session.current_pos.0), f64::from(session.current_pos.1)),
        );
        let foreign = find_drop_target(
            global,
            [(2_u64, WindowGeom::new(origin, (600, 400)), source_bar.clone())],
        );
        assert!(foreign.is_some(), "the overlapping bar must be a geometric hit");
        assert_eq!(
            compute_action(&session, foreign, &source_bar),
            DragAction::ReturnToOriginalBar,
            "a {delta}-pixel click movement must not transfer the tab"
        );
    }
}

#[test]
fn subthreshold_edge_slip_does_not_tear_out_from_an_offset_bar() {
    // A shifted bar can lie below the legacy tear Y threshold; a tiny edge slip is still a click.
    let offset = TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 16.0;
    let source_bar = source_layout().with_top_offset(offset);
    let mut session = DragSession::new(0, (0.5, offset + TAB_BAR_HEIGHT * 0.5));
    session.current_pos.0 = -0.5;
    assert!(!drag_moved_enough(&session));
    assert!(!source_bar.point_over_bar(session.current_pos.0, session.current_pos.1));
    assert_eq!(compute_action::<u64>(&session, None, &source_bar), DragAction::ReturnToOriginalBar);
}

#[test]
fn drag_at_the_threshold_still_merges_into_a_foreign_window() {
    // The existing inclusive five-pixel threshold must keep deliberate cross-window dragging working.
    let source_bar = source_layout();
    let mut session = DragSession::new(0, (12.0, TAB_BAR_HEIGHT * 0.5));
    session.current_pos.0 += DRAG_START_THRESHOLD_PX;
    let target = DropTarget { window: 2_u64, slot: 1 };
    assert!(drag_moved_enough(&session));
    assert_eq!(
        compute_action(&session, Some(target), &source_bar),
        DragAction::MergeIntoWindow(target)
    );
}

#[test]
fn deliberate_drag_still_reorders_within_the_source_bar() {
    // A real move to a peer tab retains insertion-slot reorder semantics.
    let source_bar = source_layout();
    let peer = source_bar.tabwidgets()[1].bg_rect;
    let mut session = DragSession::new(0, (12.0, TAB_BAR_HEIGHT * 0.5));
    session.current_pos = (peer.x + peer.w * 0.25, TAB_BAR_HEIGHT * 0.5);
    assert!(drag_moved_enough(&session));
    assert_eq!(
        compute_action::<u64>(&session, None, &source_bar),
        DragAction::ReorderTab { from: 0, to: 1 }
    );
}

#[test]
fn deliberate_drag_below_the_bar_still_tears_out() {
    // A genuine downward drag with no foreign target must still create a new-window tear-out.
    let source_bar = source_layout();
    let mut session = DragSession::new(0, (12.0, TAB_BAR_HEIGHT * 0.5));
    session.current_pos.1 = TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + DRAG_START_THRESHOLD_PX;
    assert!(drag_moved_enough(&session));
    assert_eq!(
        compute_action::<u64>(&session, None, &source_bar),
        DragAction::TearOutToNewWindow { drop_local: session.current_pos }
    );
}

fn live_layout(top: f32, height: f32) -> TabBarLayout {
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("first"));
    tabs.push(Tab::new("second"));
    tabs.activate(0);
    TabBarLayout::compute_at_y(&tabs, 600.0, height, top)
}

fn valid_press(layout: &TabBarLayout, pos: (f32, f32)) -> DragSession {
    assert_eq!(layout.hit(pos.0, pos.1), Some(TabHit::Activate(0)));
    assert_eq!(layout.tabs[0].hit(Point { x: pos.0, y: pos.1 }), Some(TabAction::Activate(0)));
    DragSession::new(0, pos)
}

#[test]
fn bottom_bar_small_vertical_slips_do_not_tear_out() {
    // Crossing the drag-start distance is not the same as clearing the bar's tear-out gap.
    let layout = live_layout(552.0, 48.0);
    for (press, release) in [((12.0, 556.0), (12.0, 550.0)), ((12.0, 596.0), (12.0, 602.0))] {
        let mut session = valid_press(&layout, press);
        session.current_pos = release;
        assert!(drag_moved_enough(&session));
        assert_eq!(
            compute_action::<u64>(&session, None, &layout),
            DragAction::ReturnToOriginalBar,
            "a two-pixel outside gap must remain below tear-out hysteresis"
        );
    }
}

#[test]
fn sideways_exit_within_the_live_bar_span_does_not_tear_out() {
    // Horizontal departure has no vertical outside distance, regardless of the bar's offset.
    let layout = live_layout(552.0, 48.0);
    let mut session = valid_press(&layout, (2.0, 576.0));
    for x in [-4.0, -100.0, 700.0] {
        session.current_pos = (x, 576.0);
        assert!(drag_moved_enough(&session));
        assert_eq!(compute_action::<u64>(&session, None, &layout), DragAction::ReturnToOriginalBar);
    }
}

#[test]
fn tear_out_uses_inclusive_distance_from_both_live_vertical_edges() {
    // Top, bottom, offset, and scaled bars share the same inclusive 40-raster-pixel outside gap.
    for (top, height) in [(0.0, 40.0), (552.0, 48.0), (17.0, 36.0), (104.5, 80.5)] {
        let layout = live_layout(top, height);
        let mut session = valid_press(&layout, (12.0, top + height * 0.5));
        for gap in [0.0, 2.0, TEAR_OUT_THRESHOLD_PX - 0.25, TEAR_OUT_THRESHOLD_PX, 42.0] {
            for y in [top - gap, top + height + gap] {
                session.current_pos = (12.0, y);
                let expected = if gap >= TEAR_OUT_THRESHOLD_PX {
                    DragAction::TearOutToNewWindow { drop_local: session.current_pos }
                } else {
                    DragAction::ReturnToOriginalBar
                };
                assert_eq!(
                    compute_action::<u64>(&session, None, &layout),
                    expected,
                    "top={top}, height={height}, release_y={y}, outside_gap={gap}"
                );
            }
        }
    }
}

#[test]
fn bottom_bar_retains_merge_reorder_and_cancel_precedence() {
    // Geometry hysteresis cannot override a real foreign drop or a return to the source strip.
    let layout = live_layout(552.0, 48.0);
    let mut session = valid_press(&layout, (12.0, 576.0));
    session.current_pos = (12.0, 510.0);
    let target = DropTarget { window: 2_u64, slot: 1 };
    assert_eq!(
        compute_action(&session, Some(target), &layout),
        DragAction::MergeIntoWindow(target)
    );
    let peer = layout.tabs[1].bg_rect;
    session.current_pos = (peer.x + peer.w * 0.25, 576.0);
    assert_eq!(
        compute_action::<u64>(&session, None, &layout),
        DragAction::ReorderTab { from: 0, to: 1 }
    );
    session.current_pos = (20.0, 576.0);
    assert_eq!(compute_action::<u64>(&session, None, &layout), DragAction::ReturnToOriginalBar);
    session.current_pos = session.press_pos;
    assert_eq!(compute_action(&session, Some(target), &layout), DragAction::ReturnToOriginalBar);
}
