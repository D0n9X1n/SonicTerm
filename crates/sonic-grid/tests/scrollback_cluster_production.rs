//! PR-C (#319): scroll_up ejects Lines into scrollback in Cluster form
//! when they are whole-line-uniform, else they stay Flat. Renderer
//! reads stay cluster-transparent (verified by PR-B2 already).

use sonic_grid::grid::{Cell, CellFlags, Color, Grid};
use sonic_grid::line::{Cluster, Line, LineStorage};

fn default_cell() -> Cell {
    Cell::default()
}

#[test]
fn uniform_blank_lines_become_cluster_on_eject() {
    let cols: u16 = 80;
    let rows: u16 = 24;
    let mut g = Grid::new(cols, rows);
    // Scroll 100 default-cell rows into scrollback.
    for _ in 0..100 {
        g.scroll_up(1);
    }
    assert_eq!(g.scrollback_len(), 100);
    for r in 0..100 {
        let row = g.scrollback_row(r).expect("scrollback row in range");
        assert!(
            row.is_clustered(),
            "scrollback row {r} should be Cluster (uniform blanks) but is Flat"
        );
        assert_eq!(row.len(), cols as usize);
        let c = row.get(0).cloned().unwrap_or_default();
        assert_eq!(c, default_cell());
    }
}

#[test]
fn mixed_attr_lines_stay_flat_on_eject() {
    let cols: u16 = 80;
    let rows: u16 = 24;
    let mut g = Grid::new(cols, rows);
    // Fill each row with a couple distinct cells before scrolling it
    // out. Use put_char + manual cursor moves.
    for row_idx in 0..100u16 {
        // Two-character "hi" on row 0 of visible, then scroll.
        g.cursor.row = 0;
        g.cursor.col = 0;
        g.put_char('h', Color::Default, Color::Default, CellFlags::empty());
        g.put_char('i', Color::Default, Color::Default, CellFlags::empty());
        let _ = row_idx;
        g.scroll_up(1);
    }
    assert_eq!(g.scrollback_len(), 100);
    for r in 0..100 {
        let row = g.scrollback_row(r).expect("scrollback row in range");
        assert!(!row.is_clustered(), "scrollback row {r} mixed attrs should stay Flat");
    }
}

#[test]
fn cluster_iter_reads_back_correctly() {
    let mut cell = default_cell();
    cell.ch = 'x';
    let line = Line::from_clusters(vec![Cluster { cell: cell.clone(), count: 7 }]);
    assert_eq!(line.len(), 7);
    let collected: Vec<_> = line.iter().cloned().collect();
    assert_eq!(collected.len(), 7);
    assert!(collected.iter().all(|c| c == &cell));
}

#[test]
fn try_compress_one_cell_line() {
    let mut line = Line::flat_filled(1, default_cell());
    assert!(line.try_compress());
    assert!(line.is_clustered());
    assert_eq!(line.len(), 1);
}

#[test]
fn try_compress_empty_line_noop() {
    let mut line = Line::from_flat(Vec::new());
    assert!(!line.try_compress());
    assert!(!line.is_clustered());
}

#[test]
fn try_compress_already_cluster_noop() {
    let mut line = Line::from_clusters(vec![Cluster { cell: default_cell(), count: 10 }]);
    assert!(!line.try_compress());
    assert!(line.is_clustered());
}

#[test]
fn try_compress_max_width() {
    let mut line = Line::flat_filled(4096, default_cell());
    assert!(line.try_compress());
    assert!(line.is_clustered());
    assert_eq!(line.len(), 4096);
    if let LineStorage::Cluster(cs) = line.storage() {
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].count, 4096);
    } else {
        panic!("expected Cluster storage");
    }
}

#[test]
fn try_compress_non_uniform_stays_flat() {
    let mut a = default_cell();
    a.ch = 'a';
    let mut b = default_cell();
    b.ch = 'b';
    let mut cells = vec![a; 10];
    cells.extend(vec![b; 10]);
    let mut line = Line::from_flat(cells);
    assert!(!line.try_compress());
    assert!(!line.is_clustered());
    assert_eq!(line.len(), 20);
}

#[test]
fn ensure_flat_on_cluster_degrades() {
    let mut line = Line::from_clusters(vec![Cluster { cell: default_cell(), count: 8 }]);
    assert!(line.is_clustered());
    line.ensure_flat();
    assert!(!line.is_clustered());
    assert_eq!(line.len(), 8);
}

#[test]
fn scroll_up_recycled_row_path_safe_with_cluster_scrollback() {
    // Limit scrollback so recycle path runs. Recycled row was Cluster
    // (uniform blanks); ensure_flat in scroll_up must keep us correct.
    let cols: u16 = 40;
    let rows: u16 = 5;
    let mut g = Grid::new(cols, rows);
    g.set_scrollback_limit(3);
    for _ in 0..20 {
        g.scroll_up(1);
    }
    assert_eq!(g.scrollback_len(), 3);
    for r in 0..3 {
        let row = g.scrollback_row(r).expect("scrollback row");
        assert_eq!(row.len(), cols as usize);
        // All should be uniform blanks → Cluster.
        assert!(row.is_clustered());
    }
}
