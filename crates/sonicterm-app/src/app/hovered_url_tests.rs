use super::HoveredUrl;
use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};

fn hovered(active: bool) -> HoveredUrl {
    HoveredUrl {
        cells: HoveredUrlCells::new(
            7,
            [
                HoveredUrlSpan { row: 4, start_col: 6, end_col: 21 },
                HoveredUrlSpan { row: 5, start_col: 0, end_col: 3 },
            ],
            active,
        )
        .unwrap(),
        url: "https://example.com".to_string(),
    }
}

/// Renderer projection preserves every ordered fragment while dropping target text.
#[test]
fn to_cells_preserves_all_fragments_and_drops_url() {
    let cells = hovered(true).to_cells();

    assert_eq!(cells.pane_id, 7);
    assert_eq!(
        cells.spans(),
        &[
            HoveredUrlSpan { row: 4, start_col: 6, end_col: 21 },
            HoveredUrlSpan { row: 5, start_col: 0, end_col: 3 },
        ]
    );
    assert!(cells.active);
}

/// Multi-row containment keeps half-open edges and active state independent.
#[test]
fn to_cells_contains_every_fragment_and_preserves_active_state() {
    for active in [false, true] {
        let cells = hovered(active).to_cells();

        assert_eq!(cells.active, active);
        assert!(cells.contains(4, 6));
        assert!(cells.contains(4, 20));
        assert!(cells.contains(5, 2));
        assert!(!cells.contains(4, 21));
        assert!(!cells.contains(5, 3));
        assert!(!cells.contains(3, 10));
    }
}
