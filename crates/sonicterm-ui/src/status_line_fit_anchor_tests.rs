//! The window name sits on the strip's midline, and every list stays clear of the window buttons.

use super::*;
use crate::status_line::status_line_tests::*;

/// A one-part segment for anchoring tests.
fn segment(id: StatusSegmentId, side: StatusSide, text: &str) -> StatusSegmentView {
    StatusSegmentView {
        id,
        side,
        priority: id.meta().default_priority,
        protected: id == StatusSegmentId::Window,
        parts: vec![StatusPart::new(text, StatusTone::Value)],
        compact: None,
        compact_last: false,
        small: false,
        click: None,
        spoken: SpokenSegment::default(),
    }
}

/// Monospace metrics `width` columns wide with the given insets.
fn metrics(width: f32, left_inset: f32, right_inset: f32) -> FitMetrics {
    FitMetrics { left_inset, right_inset, ..tight(width) }
}

/// The placed segment of `id`, as `(left, width)`.
fn placed_of(view: &StatusLineView, placed: &[PlacedSegment], id: StatusSegmentId) -> (f32, f32) {
    let segment = placed.iter().find(|segment| view.segments[segment.index].id == id).unwrap();
    (segment.left, segment.width)
}

/// The name's centre is the strip's midline whether or not a link or selection appears beside it,
/// so the name does not move when the pointer reaches a link.
#[test]
fn the_window_name_stays_on_the_midline_when_neighbours_appear() {
    use StatusSegmentId as Seg;
    let alone = StatusLineView::new(vec![segment(Seg::Window, StatusSide::Center, "#1 Work")]);
    let (left, width) =
        placed_of(&alone, &fit(&alone, &metrics(100.0, 0.0, 0.0), columns), Seg::Window);
    assert_eq!(left + width / 2.0, 50.0);

    let with_neighbours = StatusLineView::new(vec![
        segment(Seg::Sel, StatusSide::Center, "Sel: 3"),
        segment(Seg::Window, StatusSide::Center, "#1 Work"),
        segment(Seg::Link, StatusSide::Center, "https://example.com"),
    ]);
    let placed = fit(&with_neighbours, &metrics(100.0, 0.0, 0.0), columns);
    let (left, width) = placed_of(&with_neighbours, &placed, Seg::Window);
    assert_eq!(left + width / 2.0, 50.0, "the name keeps its place");
    let (sel_left, sel_width) = placed_of(&with_neighbours, &placed, Seg::Sel);
    assert!(sel_left + sel_width <= left, "a segment listed before the name runs to its left");
    let (link_left, _) = placed_of(&with_neighbours, &placed, Seg::Link);
    assert!(link_left >= left + width, "a segment listed after it runs to its right");
}

/// Insets push the side lists in from the window buttons, and the centre clamp keeps the name
/// clear of them; with room, the name still centres on the whole window, not the space left over.
#[test]
fn insets_keep_every_list_clear_of_the_window_buttons() {
    use StatusSegmentId as Seg;
    let view = StatusLineView::new(vec![
        segment(Seg::Mode, StatusSide::Left, "COPY"),
        segment(Seg::Window, StatusSide::Center, "#1 Work"),
        segment(Seg::Mem, StatusSide::Right, "Mem: 1 MB"),
    ]);
    let placed = fit(&view, &metrics(100.0, 20.0, 15.0), columns);
    assert_eq!(placed_of(&view, &placed, Seg::Mode).0, 20.0, "left list starts after the inset");
    let (mem_left, mem_width) = placed_of(&view, &placed, Seg::Mem);
    assert_eq!(mem_left + mem_width, 85.0, "right list ends before the inset");
    let (left, width) = placed_of(&view, &placed, Seg::Window);
    assert_eq!(left + width / 2.0, 50.0, "the name centres on the window");

    // A left inset wider than half the strip pushes the name right, just past the left list.
    let placed = fit(&view, &metrics(100.0, 45.0, 0.0), columns);
    let (mode_left, mode_width) = placed_of(&view, &placed, Seg::Mode);
    assert_eq!(mode_left, 45.0);
    assert_eq!(placed_of(&view, &placed, Seg::Window).0, mode_left + mode_width + 1.0);
}

/// Insets count as used width: a strip that fits without them sheds segments with them, and
/// nothing is ever placed inside an inset.
#[test]
fn insets_take_width_from_fitting() {
    use StatusSegmentId as Seg;
    let view = StatusLineView::new(vec![
        segment(Seg::Scroll, StatusSide::Left, "Scroll: 3"),
        segment(Seg::Window, StatusSide::Center, "#1 Work"),
    ]);
    assert_eq!(fit(&view, &metrics(17.0, 0.0, 0.0), columns).len(), 2);
    let placed = fit(&view, &metrics(17.0, 10.0, 0.0), columns);
    assert_eq!(placed.len(), 1, "the unprotected left segment drops first");
    assert!(placed.iter().all(|segment| segment.left >= 10.0));
}
