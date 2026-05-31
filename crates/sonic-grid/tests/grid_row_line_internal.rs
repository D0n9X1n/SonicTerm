//! PR-B1 (#319): Grid::visible/scrollback container is now `VecDeque<Line>`
//! internally, but the public API still returns `&Vec<Cell>` via shim.
//! These tests assert behavioural equivalence to the pre-B1 Vec<Cell>
//! storage by exercising write / scroll / resize / erase / insert / delete
//! and comparing the public accessors against a control reference.

use sonic_grid::grid::{CellFlags, Color, Grid};

fn put(g: &mut Grid, s: &str) {
    for ch in s.chars() {
        g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
}

#[test]
fn row_accessor_returns_dense_vec() {
    let mut g = Grid::new(10, 3);
    put(&mut g, "hello");
    let row = g.row(0);
    assert_eq!(row.len(), 10);
    assert_eq!(row[0].ch, 'h');
    assert_eq!(row[4].ch, 'o');
    assert_eq!(row[5].ch, ' ');
}

#[test]
fn row_mut_returns_writable_vec() {
    let mut g = Grid::new(8, 2);
    {
        let r = g.row_mut(0);
        r[3].ch = 'X';
    }
    assert_eq!(g.row(0)[3].ch, 'X');
}

#[test]
fn rows_iter_yields_all_rows_in_order() {
    let mut g = Grid::new(4, 3);
    put(&mut g, "AAAA");
    g.linefeed();
    g.carriage_return();
    put(&mut g, "BBBB");
    let collected: Vec<char> = g.rows_iter().map(|r| r[0].ch).collect();
    assert_eq!(collected, vec!['A', 'B', ' ']);
}

#[test]
fn scroll_up_pushes_to_scrollback_and_blank_row_appears() {
    let mut g = Grid::new(4, 2);
    put(&mut g, "AAAA");
    g.linefeed();
    g.carriage_return();
    put(&mut g, "BBBB");
    g.scroll_up(1);
    assert_eq!(g.scrollback_len(), 1);
    assert_eq!(g.scrollback_row(0).unwrap()[0].ch, 'A');
    // After scroll_up(1): top is the old B-row, bottom is fresh blank.
    assert_eq!(g.row(0)[0].ch, 'B');
    assert_eq!(g.row(1)[0].ch, ' ');
}

#[test]
fn resize_pads_and_clips() {
    let mut g = Grid::new(4, 2);
    put(&mut g, "ABCD");
    g.resize(6, 3);
    let row = g.row(0);
    assert_eq!(row.len(), 6);
    assert_eq!(row[0].ch, 'A');
    assert_eq!(row[5].ch, ' ');
    assert_eq!(g.rows_iter().count(), 3);
    g.resize(2, 2);
    let row = g.row(0);
    assert_eq!(row.len(), 2);
    assert_eq!(row[0].ch, 'A');
    assert_eq!(row[1].ch, 'B');
}

#[test]
fn erase_insert_delete_round_trip() {
    let mut g = Grid::new(6, 1);
    put(&mut g, "ABCDEF");
    g.erase_cells(0, 1, 2);
    assert_eq!(g.row(0)[1].ch, ' ');
    assert_eq!(g.row(0)[2].ch, ' ');
    assert_eq!(g.row(0)[3].ch, 'D');
    g.insert_cells(0, 0, 2);
    assert_eq!(g.row(0)[0].ch, ' ');
    assert_eq!(g.row(0)[2].ch, 'A');
    g.delete_cells(0, 0, 2);
    assert_eq!(g.row(0)[0].ch, 'A');
    assert_eq!(g.row(0)[1].ch, ' ');
}

#[test]
fn row_at_abs_spans_scrollback_and_visible() {
    let mut g = Grid::new(2, 2);
    put(&mut g, "AA");
    g.linefeed();
    g.carriage_return();
    put(&mut g, "BB");
    g.scroll_up(1);
    // Now scrollback has [AA], visible has [BB, blank].
    assert_eq!(g.row_at_abs(0).unwrap()[0].ch, 'A');
    assert_eq!(g.row_at_abs(1).unwrap()[0].ch, 'B');
    assert_eq!(g.row_at_abs(2).unwrap()[0].ch, ' ');
    assert!(g.row_at_abs(99).is_none());
}

#[test]
fn scrollback_iter_oldest_first() {
    let mut g = Grid::new(2, 1);
    put(&mut g, "AA");
    g.scroll_up(1);
    g.carriage_return();
    put(&mut g, "BB");
    g.scroll_up(1);
    let chars: Vec<char> = g.scrollback_iter().map(|r| r[0].ch).collect();
    assert_eq!(chars, vec!['A', 'B']);
}
