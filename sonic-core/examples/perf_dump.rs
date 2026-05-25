//! Microbenchmark for the rendering hot path: walk an N-row grid and time
//! how long it takes to build the text + spans that go into
//! `Buffer::set_rich_text`. This is what the real renderer does every frame.
//!
//! Run with: `cargo run --example perf_dump -p sonic-core --release`

use std::time::Instant;

use sonic_core::{
    grid::{CellFlags, Color, Grid},
    pty::PtyHandle,
    vt::Parser,
};

fn main() {
    // 1. parse-side: how fast does Parser::advance handle a burst?
    let pty = PtyHandle::spawn_default_shell(120, 40).expect("spawn");
    let mut parser = Parser::new(Grid::new(120, 40));
    std::thread::sleep(std::time::Duration::from_millis(800));
    while let Ok(b) = pty.out_rx.try_recv() {
        parser.advance(&b);
    }

    // Send a sizable burst
    pty.in_tx
        .send(b"for i in {1..2000}; do echo \"line $i with some content here\"; done\r".to_vec())
        .unwrap();

    let mut total_parse = std::time::Duration::ZERO;
    let mut total_bytes = 0;
    let mut batches = 0;
    let start = Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(6) {
        if let Ok(b) = pty.out_rx.recv_timeout(std::time::Duration::from_millis(20)) {
            total_bytes += b.len();
            batches += 1;
            let t = Instant::now();
            parser.advance(&b);
            total_parse += t.elapsed();
        }
    }
    println!("--- VT parse path ---");
    println!("  batches  = {batches}");
    println!("  bytes    = {total_bytes}");
    println!("  parse    = {:?} total", total_parse);
    println!("  per byte = {:?}", total_parse / total_bytes.max(1) as u32);
    println!("  per batch= {:?}", total_parse / batches.max(1) as u32);

    // 2. render-side: how fast can we walk the grid into spans?
    // Simulate exactly what render.rs does: produce (text, span_descriptors,
    // underlines) per frame.
    let grid = parser.grid();
    let mut total_walk = std::time::Duration::ZERO;
    const FRAMES: usize = 1000;
    for _ in 0..FRAMES {
        let t = Instant::now();
        let _ = walk_grid(grid);
        total_walk += t.elapsed();
    }
    println!();
    println!("--- Grid walk (no glyphon, no GPU) ---");
    println!("  frames   = {FRAMES}");
    println!("  total    = {:?}", total_walk);
    println!("  per frame= {:?}", total_walk / FRAMES as u32);
    println!("  fps cap  = {:.0}", 1.0 / (total_walk.as_secs_f64() / FRAMES as f64));

    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
}

struct SpanDesc {
    range: std::ops::Range<usize>,
    fg: (u8, u8, u8, u8),
    weight: u16,
    italic: bool,
}

fn walk_grid(grid: &Grid) -> (String, Vec<SpanDesc>, Vec<(u16, u16, u16)>) {
    let mut text = String::with_capacity((grid.cols as usize + 1) * grid.rows as usize);
    let mut spans: Vec<SpanDesc> = Vec::new();
    let mut underlines: Vec<(u16, u16, u16)> = Vec::new();

    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut run_start = text.len();
        let mut run_fg = (255_u8, 255, 255, 255);
        let mut run_weight = 400_u16;
        let mut run_italic = false;
        let mut run_has = false;
        let mut ul_start: Option<u16> = None;
        let mut last_col = 0_u16;
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                continue;
            }
            let fg = match cell.fg {
                Color::Rgb(r, g, b) => (r, g, b, 255),
                _ => (255, 255, 255, 255),
            };
            let weight = if cell.flags.contains(CellFlags::BOLD) { 700 } else { 400 };
            let italic = cell.flags.contains(CellFlags::ITALIC);
            if run_has && (fg != run_fg || weight != run_weight || italic != run_italic) {
                spans.push(SpanDesc {
                    range: run_start..text.len(),
                    fg: run_fg,
                    weight: run_weight,
                    italic: run_italic,
                });
                run_start = text.len();
                run_fg = fg;
                run_weight = weight;
                run_italic = italic;
                run_has = false;
            }
            if !run_has {
                run_fg = fg;
                run_weight = weight;
                run_italic = italic;
            }
            text.push(cell.ch);
            run_has = true;
            last_col = col as u16;
            if cell.flags.contains(CellFlags::UNDERLINE) {
                if ul_start.is_none() {
                    ul_start = Some(col as u16);
                }
            } else if let Some(s) = ul_start.take() {
                underlines.push((r, s, last_col.saturating_sub(1)));
            }
        }
        if let Some(s) = ul_start.take() {
            underlines.push((r, s, last_col));
        }
        if run_has {
            spans.push(SpanDesc {
                range: run_start..text.len(),
                fg: run_fg,
                weight: run_weight,
                italic: run_italic,
            });
        }
        text.push('\n');
    }
    (text, spans, underlines)
}
