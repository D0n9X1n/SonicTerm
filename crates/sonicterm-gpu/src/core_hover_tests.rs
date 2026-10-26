//! Renderer hover contracts, sharing one root fixture owner.

use super::*;

#[test]
fn plain_url_hover_does_not_need_accent_palette() {
    use sonicterm_render_model::inputs::HoveredUrlCells;

    assert!(!hovered_url_needs_accent(None));
    assert!(!hovered_url_needs_accent(HoveredUrlCells::single(7, 0, 1, 5, false)));
    assert!(hovered_url_needs_accent(HoveredUrlCells::single(7, 0, 1, 5, true)));
}

/// Active wrapped fragments perturb only their owning pane and intersecting row cache keys.
#[test]
fn wrapped_hover_cache_identity_is_pane_and_row_local() {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};

    let hovered = HoveredUrlCells::new(
        7,
        [
            HoveredUrlSpan { row: 2, start_col: 3, end_col: 10 },
            HoveredUrlSpan { row: 3, start_col: 0, end_col: 4 },
        ],
        true,
    )
    .unwrap();
    let first = hovered_url_for_pane_row(Some(hovered), 7, 2).unwrap();
    let second = hovered_url_for_pane_row(Some(hovered), 7, 3).unwrap();

    // Each row folds only its own fragment's columns into its content key.
    assert_eq!(hovered_url_row_key_span(Some(first), 2), Some((3, 10)));
    assert_eq!(hovered_url_row_key_span(Some(second), 3), Some((0, 4)));
    assert_eq!(hovered_url_row_key_span(Some(first), 3), None, "no fragment on that row");
    assert!(hovered_url_for_pane_row(Some(hovered), 8, 2).is_none());
    assert!(hovered_url_for_pane_row(Some(hovered), 7, 1).is_none());
}

/// Wrapped hover fragments project to exact snapped rectangles and reject invisible coverage.
#[test]
fn wrapped_hover_fragments_project_exact_underline_rectangles() {
    use sonicterm_render_model::inputs::HoveredUrlSpan;

    let edges = build_snapped_cell_x(10.0, 7.5, 8);
    assert_eq!(
        hovered_url_span_rect(
            HoveredUrlSpan { row: 1, start_col: 2, end_col: 8 },
            8,
            3,
            10.0,
            20.0,
            7.5,
            16.0,
            &edges,
        ),
        Some((edges[2], 36.0, edges[8] - edges[2], 16.0))
    );
    assert_eq!(
        hovered_url_span_rect(
            HoveredUrlSpan { row: 3, start_col: 0, end_col: 2 },
            8,
            3,
            10.0,
            20.0,
            7.5,
            16.0,
            &edges,
        ),
        None
    );
    assert_eq!(
        hovered_url_span_rect(
            HoveredUrlSpan { row: 1, start_col: 8, end_col: 9 },
            8,
            3,
            10.0,
            20.0,
            7.5,
            16.0,
            &edges,
        ),
        None
    );
}

/// Hint-only wrapped fragments reuse ordinary glyph rows because they change only overlay geometry.
#[test]
fn wrapped_hover_hint_keeps_plain_row_cache_identity() {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};

    let hovered = HoveredUrlCells::new(
        7,
        [
            HoveredUrlSpan { row: 2, start_col: 3, end_col: 10 },
            HoveredUrlSpan { row: 3, start_col: 0, end_col: 4 },
        ],
        false,
    )
    .unwrap();
    let row = hovered_url_for_pane_row(Some(hovered), 7, 3).unwrap();

    assert_eq!(hovered_url_row_key_span(Some(row), 3), None, "a hint folds nothing");
}
