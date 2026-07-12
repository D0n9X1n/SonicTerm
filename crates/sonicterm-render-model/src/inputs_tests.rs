use super::{
    CursorView, DragGhost, HoveredUrlCells, OverlayData, PaneViewModel, RenderInputs, SearchView,
    SelectionView, TabBarSnapshot,
};
use sonicterm_types::{Cell, CellFlags, Color};

fn cell(ch: char) -> Cell {
    Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())
}

#[test]
fn hovered_url_cells_contains_exactly_its_half_open_row_span_regardless_of_active_hint() {
    for active in [false, true] {
        let hovered = HoveredUrlCells { row: 3, start_col: 5, end_col: 10, active };

        assert!(hovered.contains(3, 5), "start is inclusive");
        assert!(hovered.contains(3, 9), "last column before end is included");
        assert!(!hovered.contains(3, 4), "column before start is excluded");
        assert!(!hovered.contains(3, 10), "end is exclusive");
        assert!(!hovered.contains(2, 7), "row above is excluded");
        assert!(!hovered.contains(4, 7), "row below is excluded");
    }
}

#[test]
fn hovered_url_cells_empty_or_reversed_span_contains_nothing() {
    for hovered in [
        HoveredUrlCells { row: 0, start_col: 8, end_col: 8, active: true },
        HoveredUrlCells { row: 0, start_col: 9, end_col: 4, active: true },
    ] {
        assert!(!(0..=u16::MAX).any(|col| hovered.contains(0, col)));
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
