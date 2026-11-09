//! Titlebar double-click counting, on every host.

use super::*;

/// The platform's own click count decides when it has one; otherwise two presses within the
/// double-click time and rectangle count, and a slower or farther second press does not.
#[test]
fn a_double_click_needs_two_close_presses_in_time() {
    let start = Instant::now();
    let first = Some((start, (100.0, 10.0)));
    assert!(is_double_click(Some(2), None, start, (0.0, 0.0), 1.0));
    assert!(!is_double_click(Some(1), first, start, (100.0, 10.0), 1.0));
    let soon = start + Duration::from_millis(300);
    assert!(is_double_click(None, first, soon, (102.0, 11.0), 1.0));
    assert!(!is_double_click(None, first, soon, (110.0, 10.0), 1.0), "moved too far");
    assert!(is_double_click(None, first, soon, (107.0, 10.0), 2.0), "the rectangle scales");
    let late = start + Duration::from_millis(600);
    assert!(!is_double_click(None, first, late, (100.0, 10.0), 1.0), "too slow");
    assert!(!is_double_click(None, None, soon, (100.0, 10.0), 1.0), "no first press");
}

/// Each edge shows the matching two-way resize cursor.
#[test]
fn resize_edges_show_their_cursor() {
    assert_eq!(edge_cursor(ResizeEdge::North), CursorIcon::NsResize);
    assert_eq!(edge_cursor(ResizeEdge::West), CursorIcon::EwResize);
    assert_eq!(edge_cursor(ResizeEdge::NorthEast), CursorIcon::NeswResize);
    assert_eq!(edge_cursor(ResizeEdge::SouthEast), CursorIcon::NwseResize);
}
