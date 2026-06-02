//! PR-B2 (#319) smoke: ensure render-path inputs that now take `&Line`
//! directly (via `Grid::row` / `Grid::row_at_abs` / `Grid::rows_iter`) still
//! produce identical cell sequences after the slice→Line accessor swap.
//!
//! This is a behavioural guard, not a pixel test: we drive a known cell
//! pattern through the public Grid API and assert that the snapshot we read
//! back via the new `&Line` accessors matches the snapshot we'd have read
//! via the (now-removed-from-the-API) `&[Cell]` flat-slice shim.

use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid};
use sonicterm_grid::line::Line;

fn put(g: &mut Grid, r: u16, c: u16, ch: char) {
    g.cursor.row = r;
    g.cursor.col = c;
    g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
}

#[test]
fn row_accessor_returns_line_with_identical_cells() {
    let mut g = Grid::new(10, 3);
    for (col, ch) in "hello".chars().enumerate() {
        put(&mut g, 1, col as u16, ch);
    }

    let row: &Line = g.row(1);
    assert_eq!(row.len(), 10);
    // Iteration via Line::iter (slice::Iter under the hood in B2)
    let collected: String = row.iter().map(|c| c.ch).collect();
    assert!(collected.starts_with("hello"));
    // Indexing via Line::Index<usize>
    assert_eq!(row[0].ch, 'h');
    // Range access via Line::get_range (Index<Range> removed for cluster-transparency)
    let prefix: String = row.get_range(0, 5).map(|c| c.ch).collect();
    assert_eq!(prefix, "hello");
}

#[test]
fn rows_iter_yields_line_refs() {
    let mut g = Grid::new(4, 2);
    put(&mut g, 0, 0, 'A');
    put(&mut g, 1, 0, 'B');
    let first_chars: Vec<char> = g.rows_iter().map(|row: &Line| row[0].ch).collect();
    assert_eq!(first_chars, vec!['A', 'B']);
}

#[test]
fn row_at_abs_returns_line_across_scrollback_boundary() {
    let mut g = Grid::new(4, 2);
    put(&mut g, 0, 0, 'X');
    put(&mut g, 1, 0, 'Y');
    // Push a row into scrollback by scrolling up.
    g.scroll_up(1);
    put(&mut g, 1, 0, 'Z');

    // abs 0 → scrollback row (X)
    let sb: &Line = g.row_at_abs(0).expect("scrollback row");
    assert_eq!(sb[0].ch, 'X');
    // abs ≥ sb_len → visible row
    let live: &Line = g.row_at_abs(g.scrollback_len() as u64 + 1).expect("visible row");
    assert_eq!(live[0].ch, 'Z');
}

#[test]
fn row_mut_round_trip_via_line_api() {
    let mut g = Grid::new(6, 1);
    {
        let row: &mut Line = g.row_mut(0);
        // mut indexing via Line::IndexMut<usize>
        row[0] = Cell::plain('A', Color::Default, Color::Default, CellFlags::empty());
        // set() also works
        row.set(1, Cell::plain('B', Color::Default, Color::Default, CellFlags::empty()));
    }
    let row = g.row(0);
    assert_eq!(row[0].ch, 'A');
    assert_eq!(row[1].ch, 'B');
}
