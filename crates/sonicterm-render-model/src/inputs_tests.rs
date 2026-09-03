use super::{
    CursorView, DragGhost, HoveredUrlCells, HoveredUrlSpan, OverlayData, PaneViewModel,
    RenderInputs, SearchView, SelectionView, TabBarSnapshot,
};
use sonicterm_types::{Cell, CellFlags, Color};

fn cell(ch: char) -> Cell {
    Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())
}

/// Multi-row hover containment covers every ordered fragment with half-open columns.
#[test]
fn hovered_url_cells_contains_first_middle_and_final_spans() {
    for active in [false, true] {
        let hovered = HoveredUrlCells::new(
            7,
            [
                HoveredUrlSpan { row: 3, start_col: 5, end_col: 10 },
                HoveredUrlSpan { row: 4, start_col: 0, end_col: 12 },
                HoveredUrlSpan { row: 5, start_col: 0, end_col: 4 },
            ],
            active,
        )
        .expect("ordered non-empty spans");

        assert!(hovered.contains(3, 5), "first start is inclusive");
        assert!(hovered.contains(4, 11), "middle end predecessor is included");
        assert!(hovered.contains(5, 3), "final fragment is included");
        assert!(!hovered.contains(3, 10), "first end is exclusive");
        assert!(!hovered.contains(5, 4), "final end is exclusive");
        assert!(!hovered.contains(2, 7), "row before the chain is excluded");
    }
}

/// Invalid, duplicate, out-of-order, and ninth fragments fail closed at construction.
#[test]
fn hovered_url_cells_rejects_noncanonical_or_overlong_span_sets() {
    for spans in [
        vec![HoveredUrlSpan { row: 0, start_col: 8, end_col: 8 }],
        vec![HoveredUrlSpan { row: 0, start_col: 9, end_col: 4 }],
        vec![
            HoveredUrlSpan { row: 1, start_col: 0, end_col: 4 },
            HoveredUrlSpan { row: 1, start_col: 4, end_col: 8 },
        ],
        vec![
            HoveredUrlSpan { row: 2, start_col: 0, end_col: 4 },
            HoveredUrlSpan { row: 1, start_col: 0, end_col: 4 },
        ],
        (0..9).map(|row| HoveredUrlSpan { row, start_col: 0, end_col: 1 }).collect(),
    ] {
        assert!(HoveredUrlCells::new(7, spans, true).is_none());
    }
}

#[test]
fn pane_view_models_keep_each_panes_rows_cursor_and_scroll_offset_independent() {
    let first_rows = vec![vec![cell('a'), cell('b')]];
    let second_rows = vec![vec![cell('x')], vec![cell('y')]];
    let panes = vec![
        PaneViewModel {
            rows: &first_rows,
            cursor: CursorView { row: 0, col: 1, blink_on: true },
            scroll_offset: 0,
        },
        PaneViewModel {
            rows: &second_rows,
            cursor: CursorView { row: 1, col: 0, blink_on: false },
            scroll_offset: 7,
        },
    ];
    let inputs = RenderInputs {
        panes,
        tab_bar: TabBarSnapshot::default(),
        overlays: OverlayData::default(),
        selection: Some(SelectionView { start: (4, 9), end: (2, 1) }),
        search: Some(SearchView { matches: vec![(0, 0, 1), (1, 0, 1)], current: 1 }),
        ..RenderInputs::default()
    };

    assert_eq!(inputs.panes[0].rows[0][1].ch, 'b');
    assert_eq!((inputs.panes[0].cursor.row, inputs.panes[0].cursor.col), (0, 1));
    assert!(inputs.panes[0].cursor.blink_on);
    assert_eq!(inputs.panes[0].scroll_offset, 0);

    assert_eq!(inputs.panes[1].rows[1][0].ch, 'y');
    assert_eq!((inputs.panes[1].cursor.row, inputs.panes[1].cursor.col), (1, 0));
    assert!(!inputs.panes[1].cursor.blink_on);
    assert_eq!(inputs.panes[1].scroll_offset, 7);

    let selection = inputs.selection.as_ref().unwrap();
    assert_eq!((selection.start, selection.end), ((4, 9), (2, 1)));
    let search = inputs.search.as_ref().unwrap();
    assert_eq!(search.matches[search.current], (1, 0, 1));
}

#[test]
fn drag_ghost_default_encodes_feedback_constants_without_claiming_a_source_or_destination() {
    let ghost = DragGhost::default();

    assert_eq!(ghost.top_left, (0.0, 0.0));
    assert!(ghost.title.is_empty());
    assert_eq!(ghost.alpha, DragGhost::GHOST_ALPHA);
    assert_eq!(ghost.source_alpha, DragGhost::SOURCE_ALPHA);
    assert_eq!(ghost.insertion_gap_px, DragGhost::INSERTION_GAP_PX);
    assert_eq!(ghost.source_tab_idx, None);
    assert_eq!(ghost.insertion_slot, None);
}
