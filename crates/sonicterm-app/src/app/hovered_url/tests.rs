
use super::HoveredUrl;

#[test]
fn to_cells_preserves_range_and_drops_url() {
    let h = HoveredUrl {
        row: 4,
        start_col: 6,
        end_col: 21,
        url: "https://example.com".to_string(),
        active: true,
    };
    let cells = h.to_cells();
    assert_eq!(cells.row, 4);
    assert_eq!(cells.start_col, 6);
    assert_eq!(cells.end_col, 21);
}

#[test]
fn to_cells_then_contains_detects_inside_and_outside() {
    // URL occupies viewport row 4, columns 6..21.
    let h = HoveredUrl {
        row: 4,
        start_col: 6,
        end_col: 21,
        url: "https://example.com".to_string(),
        active: true,
    };
    let cells = h.to_cells();

    // A cell inside the span on the correct row is detected.
    assert!(cells.contains(4, 6), "inclusive start");
    assert!(cells.contains(4, 20), "last included column (end_col - 1)");

    // Exclusive end and columns outside the span are not.
    assert!(!cells.contains(4, 21), "exclusive end");
    assert!(!cells.contains(4, 5), "before the span");

    // The same columns on a different row never match.
    assert!(!cells.contains(3, 10), "row above");
    assert!(!cells.contains(5, 10), "row below");
}
