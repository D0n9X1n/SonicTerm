//! #319 PR-F (final of 6): structural memory report for scrollback.
//!
//! Run with:
//!
//!     cargo run --example scrollback_memory_report -p sonic-grid --release
//!
//! Builds a Grid at the standard 120x24, scrolls 10K uniform-blank
//! lines into scrollback, then reports:
//!
//!   * measured cluster-encoded bytes (via `Grid::scrollback_approx_bytes`)
//!   * equivalent dense `Vec<Vec<Cell>>` baseline (rows * cols * sizeof Cell)
//!   * compression ratio (target: <=60%, i.e. >=40% reduction)
//!   * Cluster vs Flat row breakdown
//!
//! Exits non-zero if the ratio regresses past 60%, so this doubles as
//! a CI gate when wired into `scripts/bench.sh` (it is, as the
//! `scrollback_ram_mb` metric).

use sonic_grid::grid::{Cell, Grid};

fn main() {
    let cols: u16 = 120;
    let rows: u16 = 24;
    let n_lines: usize = 10_000;

    let mut g = Grid::new(cols, rows);
    g.set_scrollback_limit(n_lines + 100);
    for _ in 0..n_lines {
        g.scroll_up(1);
    }

    let (cluster, flat) = g.scrollback_storage_breakdown();
    let measured = g.scrollback_approx_bytes();
    let cell_sz = std::mem::size_of::<Cell>();
    let dense = g.scrollback_len() * cols as usize * cell_sz;
    let ratio_pct = (measured as f64) / (dense as f64) * 100.0;
    let measured_mb = measured as f64 / 1_048_576.0;
    let dense_mb = dense as f64 / 1_048_576.0;

    println!("# scrollback memory report  (cols={cols} rows={rows} lines={n_lines})");
    println!("scrollback_len            = {}", g.scrollback_len());
    println!("cluster_rows              = {cluster}");
    println!("flat_rows                 = {flat}");
    println!("sizeof::<Cell>            = {cell_sz} B");
    println!("dense_baseline_bytes      = {dense} ({dense_mb:.3} MiB)");
    println!("measured_cluster_bytes    = {measured} ({measured_mb:.3} MiB)");
    println!("ratio                     = {ratio_pct:.2}%  (target <=60%)");
    // Machine-readable line for scripts/bench.sh — keep stable.
    println!("BENCH scrollback_ram_mb {measured_mb:.4}");

    if measured * 10 > dense * 6 {
        eprintln!(
            "FAIL: scrollback compression ratio {ratio_pct:.2}% exceeds 60% threshold",
        );
        std::process::exit(1);
    }
    println!("OK: scrollback ratio within target ({:.2}% reduction)", 100.0 - ratio_pct);
}
