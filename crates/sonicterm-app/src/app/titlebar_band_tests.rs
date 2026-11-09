//! Titlebar double-click counting, on every host.

use super::*;

/// The platform's own click count decides when it has one; otherwise two presses within the
/// system's double-click time and rectangle count, and a slower or farther second press does not.
#[test]
fn a_double_click_needs_two_close_presses_in_time() {
    let start = Instant::now();
    let first = Some((start, (100.0, 10.0)));
    let limits = (Duration::from_millis(500), (4.0, 4.0));
    assert!(is_double_click(Some(2), None, start, (0.0, 0.0), limits));
    assert!(!is_double_click(Some(1), first, start, (100.0, 10.0), limits));
    let soon = start + Duration::from_millis(300);
    assert!(is_double_click(None, first, soon, (102.0, 11.0), limits));
    assert!(!is_double_click(None, first, soon, (110.0, 10.0), limits), "moved too far");
    let wide = (Duration::from_millis(500), (8.0, 2.0));
    assert!(is_double_click(None, first, soon, (107.0, 10.0), wide), "the system width");
    let tall = (Duration::from_millis(500), (2.0, 5.0));
    assert!(!is_double_click(None, first, soon, (104.0, 10.0), tall), "a narrow width refuses");
    assert!(is_double_click(None, first, soon, (101.0, 14.0), tall), "a tall height accepts");
    let late = start + Duration::from_millis(600);
    assert!(!is_double_click(None, first, late, (100.0, 10.0), limits), "too slow");
    let slow = (Duration::from_millis(900), (4.0, 4.0));
    assert!(is_double_click(None, first, late, (100.0, 10.0), slow), "the system time");
    assert!(!is_double_click(None, None, soon, (100.0, 10.0), limits), "no first press");
}

/// Each edge shows the matching two-way resize cursor.
#[test]
fn resize_edges_show_their_cursor() {
    assert_eq!(edge_cursor(ResizeEdge::North), CursorIcon::NsResize);
    assert_eq!(edge_cursor(ResizeEdge::West), CursorIcon::EwResize);
    assert_eq!(edge_cursor(ResizeEdge::NorthEast), CursorIcon::NeswResize);
    assert_eq!(edge_cursor(ResizeEdge::SouthEast), CursorIcon::NwseResize);
}

/// A caption button acts only when released over itself with no overlay opened while it was held;
/// moving off it, releasing over another button, or a palette opening cancels it.
#[test]
fn a_caption_press_acts_only_on_its_own_release() {
    use sonicterm_ui::window_band::CaptionButton::{Close, Maximize};
    assert_eq!(release_action(Some(Close), Some(Close), false), Some(Close));
    assert_eq!(release_action(Some(Close), Some(Maximize), false), None, "another button");
    assert_eq!(release_action(Some(Close), None, false), None, "released off the buttons");
    assert_eq!(release_action(Some(Close), Some(Close), true), None, "a palette opened");
    assert_eq!(
        release_action(None, Some(Close), false),
        None,
        "cancelled by a leave or focus loss"
    );
}
