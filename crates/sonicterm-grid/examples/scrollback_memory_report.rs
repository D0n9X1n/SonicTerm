//! #319 PR-F (final of 6): structural memory report for scrollback.
//!
//! Run with:
//!
//!     cargo run --example scrollback_memory_report -p sonicterm-grid --release
//!
//! Builds a Grid at the standard 120x24, scrolls 10K uniform-blank
//! lines into scrollback, then reports:
//!
//!   * measured cluster-encoded heap bytes (Vec capacity + struct overhead)
//!   * equivalent dense `Vec<Vec<Cell>>` heap baseline using Vec capacity
//!   * compression ratio (target: <=60%, i.e. >=40% reduction)
//!   * Cluster vs Flat row breakdown
//!
//! Exits non-zero if the ratio regresses past 60%, so this doubles as
//! a CI gate when wired into `scripts/bench.sh` (it is, as the
//! `scrollback_ram_mb` metric).

use std::collections::VecDeque;

use sonicterm_grid::{
    grid::{Cell, Grid},
    line::Line,
};

fn dense_scrollback_heap_bytes(row_slots: usize, rows: usize, cols: usize) -> usize {
    // Methodology: this measures user-visible heap shape, not just payload
    // bytes. The dense baseline includes the outer scrollback container,
    // reserved outer row slots, one inner Vec<Cell> header per live row, plus
    // each row buffer's Vec::capacity()-sized Cell allocation. The measured
    // clustered side uses Grid::scrollback_heap_bytes, which applies the same
    // accounting to the actual LineStorage representation.
    let dense_row_capacity = Vec::<Cell>::with_capacity(cols).capacity();
    std::mem::size_of::<VecDeque<Line>>()
        + row_slots * std::mem::size_of::<Line>()
        + rows * std::mem::size_of::<Vec<Cell>>()
        + rows * dense_row_capacity * std::mem::size_of::<Cell>()
}

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
    let measured = g.scrollback_heap_bytes();
    let cell_sz = std::mem::size_of::<Cell>();
    let line_sz = std::mem::size_of::<Line>();
    let vec_sz = std::mem::size_of::<Vec<Cell>>();
    let row_slots = g.scrollback_capacity();
    let dense = dense_scrollback_heap_bytes(row_slots, g.scrollback_len(), cols as usize);
    let ratio_pct = (measured as f64) / (dense as f64) * 100.0;
    let measured_mb = measured as f64 / 1_048_576.0;
    let dense_mb = dense as f64 / 1_048_576.0;

    println!("# scrollback memory report  (cols={cols} rows={rows} lines={n_lines})");
    println!("scrollback_len            = {}", g.scrollback_len());
    println!("scrollback_capacity       = {row_slots}");
    println!("cluster_rows              = {cluster}");
    println!("flat_rows                 = {flat}");
    println!("sizeof::<Cell>            = {cell_sz} B");
    println!("sizeof::<Line>            = {line_sz} B");
    println!("sizeof::<Vec<Cell>>       = {vec_sz} B");
    println!("dense_baseline_bytes      = {dense} ({dense_mb:.3} MiB)");
    println!("measured_cluster_bytes    = {measured} ({measured_mb:.3} MiB)");
    println!("ratio                     = {ratio_pct:.2}%  (target <=60%)");
    // Machine-readable line for scripts/bench.sh — keep stable.
    println!("BENCH scrollback_ram_mb {measured_mb:.4}");

    if measured * 10 > dense * 6 {
        eprintln!("FAIL: scrollback compression ratio {ratio_pct:.2}% exceeds 60% threshold",);
        std::process::exit(1);
    }
    println!("OK: scrollback ratio within target ({:.2}% reduction)", 100.0 - ratio_pct);
}
