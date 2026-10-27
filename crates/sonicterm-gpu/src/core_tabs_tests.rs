//! Renderer tabs contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn an_in_tab_pointer_move_keeps_the_hovered_tab_and_requests_no_redraw() {
    // The hovered tab is the only hover fact a frame draws, so a move that
    // stays inside one tab must not ask for a frame.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let tab = rects[0];
        let center_y = tab.y + tab.h / 2.0;
        let start = (tab.x + 2.0, center_y);
        let end = (tab.x + tab.w - 2.0, center_y);
        assert_eq!(hovered_tab_at(&tabs, geometry, Some(start)), 0);
        assert_eq!(hovered_tab_at(&tabs, geometry, Some(end)), 0);
        assert!(!hover_tab_changed(&tabs, geometry, Some(start), Some(end)));
    }
}

#[test]
fn crossing_from_one_tab_to_another_changes_the_hovered_tab_exactly_once() {
    // A 1 px sweep from the first tab's center to the second's must report a
    // change only on the step that moves the hover to the second tab.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let center_y = rects[0].y + rects[0].h / 2.0;
        let from_x = rects[0].x + rects[0].w / 2.0;
        let to_x = rects[1].x + rects[1].w / 2.0;
        let mut previous = Some((from_x, center_y));
        let mut change_count = 0;
        let mut pointer_x = from_x + 1.0;
        while pointer_x <= to_x {
            let next = Some((pointer_x, center_y));
            if hover_tab_changed(&tabs, geometry, previous, next) {
                change_count += 1;
            }
            previous = next;
            pointer_x += 1.0;
        }
        assert_eq!(hovered_tab_at(&tabs, geometry, previous), 1);
        // The inter-tab gap, when there is one, holds no tab, so crossing it
        // reports leaving the first tab and entering the second.
        let gap_px = rects[1].x - (rects[0].x + rects[0].w);
        let expected_changes = if gap_px > 1.0 { 2 } else { 1 };
        assert_eq!(change_count, expected_changes, "gap {gap_px} px");
    }
}

#[test]
fn leaving_the_bar_changes_the_hovered_tab_once() {
    // Leaving a hovered tab, through the terminal area or out of the window,
    // reports one change; later moves that hover no tab report none.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let tab = rects[0];
        let inside = Some((tab.x + tab.w / 2.0, tab.y + tab.h / 2.0));
        let terminal_y = if geometry.top_offset_px > 0.0 { 100.0 } else { 300.0 };
        let below = Some((tab.x + tab.w / 2.0, terminal_y));
        let further = Some((tab.x + tab.w / 2.0 + 30.0, terminal_y + 30.0));
        let path = [inside, below, further, None];
        let change_count = path
            .windows(2)
            .filter(|pair| hover_tab_changed(&tabs, geometry, pair[0], pair[1]))
            .count();
        assert_eq!(hovered_tab_at(&tabs, geometry, inside), 0);
        assert_eq!(change_count, 1);
        assert!(hover_tab_changed(&tabs, geometry, inside, None));
    }
}

#[test]
fn bar_space_outside_every_tab_hovers_no_tab() {
    // Empty bar space past the last tab hovers nothing, so moves inside it
    // need no frame even though they fall inside the bar's band.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let last = rects[rects.len() - 1];
        let empty_x = geometry.width_px - 2.0;
        assert!(last.x + last.w < empty_x, "two short tabs leave empty bar space");
        let center_y = last.y + last.h / 2.0;
        assert_eq!(hovered_tab_at(&tabs, geometry, Some((empty_x, center_y))), u32::MAX);
        assert!(!hover_tab_changed(
            &tabs,
            geometry,
            Some((empty_x, center_y)),
            Some((empty_x - 1.0, center_y)),
        ));
        let hidden = TabBarHoverGeometry { visible: false, ..geometry };
        let inside = Some((rects[0].x + 2.0, center_y));
        assert_eq!(hovered_tab_at(&tabs, hidden, inside), u32::MAX);
        assert!(!hover_tab_changed(&tabs, hidden, inside, None));
    }
}

#[test]
fn render_and_set_hover_cursor_share_one_hovered_tab_resolution() {
    // The renderer cannot be built in a unit test, so the source pins the
    // contract: both paths resolve the hovered tab through one method, and a
    // hover update never clears the frame key (the hovered tab is part of it).
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let hover_start = source.find("pub fn set_hover_cursor(").expect("set_hover_cursor");
    let hover_end = hover_start + source[hover_start..].find("\n    }\n").expect("body end");
    let hover_body = &source[hover_start..hover_end];
    assert!(hover_body.contains("tabs: &TabBar"), "callers pass their window's tab bar");
    assert!(hover_body
        .contains("self.hovered_tab_index(tabs, previous) != self.hovered_tab_index(tabs, pos)"));
    let index_start = source.find("fn hovered_tab_index(").expect("hovered_tab_index");
    let index_body = &source[index_start..index_start + 200];
    assert!(index_body.contains("hovered_tab_at(tabs, self.tab_bar_hover_geometry(), cursor)"));
    assert!(!hover_body.contains("last_frame_key"), "a hover move keeps the frame key");
    let render_body = method_body(&source, "    pub(super) fn assemble_frame(");
    assert!(render_body.contains("self.hovered_tab_index(tabs, self.hover_cursor)"));
    assert!(!render_body.contains("t.hover_at("), "render keeps no second hit test");
    assert!(!source.contains("fn hover_change_touches_tab_bar("));
}
