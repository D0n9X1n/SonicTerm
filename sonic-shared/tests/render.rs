use sonic_core::{
    grid::{Cell, CellFlags, Color, Grid},
    hyperlink::HyperlinkId,
};

use sonic_shared::render::collect_hyperlink_runs;

#[test]
fn collect_hyperlink_runs_coalesces_three_contiguous_cells() {
    let mut g = Grid::new(8, 1);
    let hid = HyperlinkId(42);
    for c in 0..3u16 {
        g.row_mut(0)[c as usize] = Cell {
            ch: 'x',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: Some(hid),
            extras: None,
        };
    }
    let runs = collect_hyperlink_runs(&g);
    assert_eq!(runs, vec![(0u16, 0u16, 2u16)]);
}

#[test]
fn collect_hyperlink_runs_splits_on_different_id() {
    let mut g = Grid::new(6, 1);
    let a = HyperlinkId(1);
    let b = HyperlinkId(2);
    for (c, h) in [(0usize, a), (1, a), (3, b), (4, b)] {
        g.row_mut(0)[c] = Cell {
            ch: 'x',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: Some(h),
            extras: None,
        };
    }
    let runs = collect_hyperlink_runs(&g);
    assert_eq!(runs, vec![(0, 0, 1), (0, 3, 4)]);
}

#[test]
fn collect_hyperlink_runs_empty_when_no_links() {
    let g = Grid::new(4, 2);
    assert!(collect_hyperlink_runs(&g).is_empty());
}
