//! Regression tests for #348 — DECSTBM/CSI S/T region scrolls must
//! mark every row in the scrolling region dirty so the renderer's
//! `LineQuadCache` does not serve stale per-row quads.

use sonic_grid::grid::Grid;
use sonic_types::cell::{CellFlags, Color};

fn write_unique_rows(g: &mut Grid) {
    // Each row gets distinct characters so we can tell whether the
    // physical shift actually moved them.
    let rows = g.rows;
    let cols = g.cols;
    for r in 0..rows {
        let tag = char::from(b'A' + (r as u8 % 26));
        g.goto(r, 0);
        for _ in 0..cols.min(8) {
            g.put_char(tag, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    g.clear_dirty();
    assert_eq!(g.dirty_count(), 0, "clear_dirty must zero the bitset");
}

#[test]
fn scroll_region_up_marks_every_row_in_region_dirty() {
    let mut g = Grid::new(20, 30);
    write_unique_rows(&mut g);

    g.scroll_region_up(5, 19, 3);

    // Inside [5, 19] every row must be dirty.
    for r in 5..=19 {
        assert!(
            g.is_row_dirty(r),
            "row {r} inside scroll region must be dirty after scroll_region_up"
        );
    }
}

#[test]
fn scroll_region_up_leaves_outside_rows_unchanged() {
    let mut g = Grid::new(20, 30);
    write_unique_rows(&mut g);

    // Row 0 starts with 'A's. Snapshot it.
    let r0_before: String = g.row(0).iter().map(|c| c.ch).collect();
    let r25_before: String = g.row(25).iter().map(|c| c.ch).collect();

    g.scroll_region_up(5, 19, 3);

    let r0_after: String = g.row(0).iter().map(|c| c.ch).collect();
    let r25_after: String = g.row(25).iter().map(|c| c.ch).collect();

    assert_eq!(r0_before, r0_after, "row above region must not move");
    assert_eq!(r25_before, r25_after, "row below region must not move");
}

#[test]
fn scroll_region_up_shifts_content_and_clears_bottom() {
    let mut g = Grid::new(20, 30);
    write_unique_rows(&mut g);

    // Snapshot row 8 — after scroll_region_up(5, 19, 3) it should
    // appear at row 5.
    let r8_before: String = g.row(8).iter().map(|c| c.ch).collect();

    g.scroll_region_up(5, 19, 3);

    let r5_after: String = g.row(5).iter().map(|c| c.ch).collect();
    assert_eq!(r8_before, r5_after, "row 8 must have shifted to row 5");

    // Bottom 3 rows of region (17, 18, 19) must be blank.
    for r in 17..=19 {
        for c in 0..g.cols {
            assert_eq!(
                g.row(r)[c as usize].ch,
                ' ',
                "bottom-of-region row {r} must be blank after scroll"
            );
        }
    }
}

#[test]
fn three_successive_region_scrolls_keep_region_dirty() {
    // Models the nvim `j j j` behavior from #348: every CSI S leaves
    // every destination row dirty so the LineQuadCache invalidator
    // (which iterates `grid.dirty_rows()` between frames) drops every
    // cached entry that could be served stale.
    let mut g = Grid::new(20, 30);
    write_unique_rows(&mut g);

    for _ in 0..3 {
        g.scroll_region_up(5, 19, 1);
        for r in 5..=19 {
            assert!(g.is_row_dirty(r), "row {r} must be dirty after each region scroll");
        }
        g.clear_dirty();
    }
}

#[test]
fn scroll_region_down_marks_region_dirty_and_clears_top() {
    let mut g = Grid::new(20, 30);
    write_unique_rows(&mut g);

    g.scroll_region_down(5, 19, 2);

    for r in 5..=19 {
        assert!(g.is_row_dirty(r), "row {r} dirty after scroll_region_down");
    }
    for r in 5..=6 {
        for c in 0..g.cols {
            assert_eq!(g.row(r)[c as usize].ch, ' ', "top of region blank");
        }
    }
}
