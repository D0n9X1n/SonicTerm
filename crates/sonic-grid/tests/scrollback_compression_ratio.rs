//! #319 PR-F (final of 6): validate the LineStorage scrollback
//! integration delivers the headline ≥40% RAM reduction promised by
//! Epic #300 P5.
//!
//! Two-part contract:
//!
//!   1. Uniform-content lines (the bulk of real-world scrollback —
//!      blank rows from `clear`, `printf`-padded prompts, `tail -f`
//!      stretches of identical timestamps) MUST land in Cluster form
//!      on eject. We require >90% of uniform-content rows to be
//!      Cluster after `scroll_up`.
//!
//!   2. Non-uniform lines (real terminal output with mixed glyphs +
//!      attrs) MUST stay Flat — Cluster form would actively cost more
//!      RAM because each per-cell `Cluster { cell, count: 1 }` is
//!      wider than a bare `Cell`. The threshold lives in
//!      `LineStorage::try_compress`; we verify the policy still holds
//!      end-to-end via Grid::scroll_up.

use sonic_grid::grid::{CellFlags, Color, Grid};

#[test]
fn uniform_scrollback_lines_are_overwhelmingly_cluster() {
    let cols: u16 = 120;
    let rows: u16 = 24;
    let mut g = Grid::new(cols, rows);

    const N: usize = 1000;
    for _ in 0..N {
        g.scroll_up(1);
    }

    assert_eq!(g.scrollback_len(), N);
    let (cluster, flat) = g.scrollback_storage_breakdown();
    assert_eq!(cluster + flat, N);
    // Require >=90% cluster — single-Cluster RLE is the whole point of
    // PR-C/D/E. In practice this is 100%.
    let pct = (cluster as f64) / (N as f64) * 100.0;
    assert!(
        cluster * 10 >= N * 9,
        "expected >=90% of uniform-blank scrollback rows to be Cluster, got {pct:.1}% ({cluster}/{N})",
    );
}

#[test]
fn non_uniform_scrollback_lines_stay_flat() {
    let cols: u16 = 120;
    let rows: u16 = 24;
    let mut g = Grid::new(cols, rows);

    // Each line gets two distinct-content cells before ejection.
    const N: usize = 200;
    for _ in 0..N {
        g.cursor.row = 0;
        g.cursor.col = 0;
        g.put_char('a', Color::Default, Color::Default, CellFlags::empty());
        g.put_char('b', Color::Default, Color::Default, CellFlags::empty());
        g.scroll_up(1);
    }

    let (cluster, flat) = g.scrollback_storage_breakdown();
    assert_eq!(cluster + flat, N);
    // Mixed-content lines must NOT compress — Cluster of N distinct
    // single-count entries would burn more RAM than Flat.
    assert_eq!(
        cluster, 0,
        "non-uniform lines should stay Flat; saw {cluster} Cluster rows out of {N}",
    );
}

#[test]
fn cluster_scrollback_bytes_below_60pct_of_dense_baseline() {
    // Headline #319 promise: cluster-encoded uniform scrollback
    // occupies <=60% of the bytes a dense `Vec<Vec<Cell>>` would for
    // the same row count + width.  In practice the ratio is closer to
    // 1% (one Cluster per row vs `cols` Cells per row), so 60% leaves
    // generous slack for the per-Line/per-Vec overheads we don't
    // count in `approx_byte_size`.
    let cols: u16 = 120;
    let rows: u16 = 24;
    let mut g = Grid::new(cols, rows);
    const N: usize = 5000;
    for _ in 0..N {
        g.scroll_up(1);
    }
    let measured = g.scrollback_approx_bytes();
    let cell_sz = std::mem::size_of::<sonic_grid::grid::Cell>();
    let dense = N * cols as usize * cell_sz;
    let ratio_pct = (measured as f64) / (dense as f64) * 100.0;
    assert!(
        measured * 10 <= dense * 6,
        "expected scrollback bytes <=60% of dense baseline, got {ratio_pct:.2}% ({measured} / {dense} bytes)",
    );
}
