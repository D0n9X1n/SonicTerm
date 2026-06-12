
use super::HoveredUrlCells;

#[test]
fn hovered_url_cells_contains_matches_inside_range_only() {
    // URL on viewport row 3, columns 5..10 (5,6,7,8,9 inclusive).
    let h = HoveredUrlCells { row: 3, start_col: 5, end_col: 10, active: true };

    // Inclusive start, interior, and last-included column hit.
    assert!(h.contains(3, 5), "start_col is inclusive");
    assert!(h.contains(3, 7), "interior column");
    assert!(h.contains(3, 9), "end_col - 1 is the last included column");

    // Exclusive end and out-of-span columns miss.
    assert!(!h.contains(3, 10), "end_col is exclusive");
    assert!(!h.contains(3, 4), "column before the span");
    assert!(!h.contains(3, 11), "column past the span");

    // Wrong row never matches, even for in-span columns.
    assert!(!h.contains(2, 7), "row above");
    assert!(!h.contains(4, 7), "row below");
}

#[test]
fn hovered_url_cells_empty_span_contains_nothing() {
    // Degenerate start == end span: end_col is exclusive so no
    // column can satisfy `start_col <= col < end_col`.
    let h = HoveredUrlCells { row: 0, start_col: 8, end_col: 8, active: true };
    assert!(!h.contains(0, 8));
    assert!(!h.contains(0, 7));
}
