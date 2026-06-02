//! Headless performance harness.
//!
//! Runs the full SonicTerm stack (PTY + Parser + Grid + spans-builder) without
//! a window or GPU and measures every interesting number. Emits one JSON
//! line so callers can diff `before.json` vs `after.json`.
//!
//! Run: `cargo run --release -p sonicterm-core --example bench [scenario]`
//! Scenarios: `typing | scroll | idle | all` (default: `all`)
//!
//! Numbers measured:
//!   - parse_ns_per_byte                (VT parser throughput)
//!   - parse_ns_per_batch
//!   - grid_walk_us_per_frame           (build text + spans, no GPU)
//!   - idle_cpu_pct                     (sample 1s of nothing happening)
//!   - typing_echo_latency_us_p50/p95/p99   (key byte → grid mutation)
//!   - scroll_throughput_lines_per_sec  (cat-like burst)
//!   - bytes_in_burst / batches_in_burst
//!
//! Compare runs:
//!   cargo run --release -p sonicterm-core --example bench -- all > before.json
//!   # … apply perf changes …
//!   cargo run --release -p sonicterm-core --example bench -- all > after.json
//!   diff <(jq -S . before.json) <(jq -S . after.json)

#![allow(clippy::wildcard_in_or_patterns, dead_code)]

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonicterm_core::{
    glyph_key::GlyphKey,
    grid::{CellFlags, Color, Grid},
    pty::PtyHandle,
    vt::Parser,
};

#[derive(serde::Serialize, Default)]
struct Report {
    parse_ns_per_byte: u128,
    parse_ns_per_batch: u128,
    grid_walk_us_per_frame: u128,
    idle_cpu_pct: f64,
    typing_echo_latency_us_p50: u128,
    typing_echo_latency_us_p95: u128,
    typing_echo_latency_us_p99: u128,
    scroll_throughput_lines_per_sec: u64,
    scroll_bytes: usize,
    scroll_batches: usize,
    /// B3: per-frame cost of the atlas-lookup hot path (key compute +
    /// HashMap entry). This is what the GPU text pipeline pays in CPU
    /// each frame; if the number is low and the hit-rate is high, the
    /// real GPU path will be bottlenecked elsewhere (GPU draw, not CPU).
    glyph_walk_us_per_frame: u128,
    /// B3: unique GlyphKeys observed after a typical scroll workload.
    /// Expected on the order of ~96 for ASCII and ~200 for unicode-heavy
    /// prompts; a runaway number means the key is splitting on something
    /// it shouldn't (color leak, hash bug, etc).
    glyph_atlas_unique_keys: usize,
    /// B3: percent of lookups during the steady-state scroll burst that
    /// hit an already-populated map entry. Should be ≥ 99% after the
    /// first few rows; near-zero means the workload is pathological or
    /// the cache is being thrown away.
    glyph_walk_hit_rate_pct: f64,
}

fn main() {
    let scenario = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let mut r = Report::default();

    match scenario.as_str() {
        "typing" => measure_typing(&mut r),
        "scroll" => measure_scroll_and_parse(&mut r),
        "idle" => measure_idle(&mut r),
        "all" | _ => {
            measure_typing(&mut r);
            measure_scroll_and_parse(&mut r);
            measure_idle(&mut r);
        }
    }

    let line = serde_json::to_string(&r).unwrap();
    println!("{line}");
}

/// Measure key → echo latency. For each key we send, we record the wall
/// clock; in a background thread we read pty out + parse + then check
/// when the just-typed character appears in the grid; difference is the
/// echo latency. Done with a real local shell.
fn measure_typing(r: &mut Report) {
    let pty = PtyHandle::spawn_default_shell(120, 40, sonicterm_core::pty::ShellSpawnOpts::default()).expect("spawn");
    let parser = Arc::new(parking_lot::Mutex::new(Parser::new(Grid::new(120, 40))));
    let shutdown = Arc::new(AtomicBool::new(false));

    let p_clone = parser.clone();
    let rx = pty.out_rx.clone();
    let sd = shutdown.clone();
    let drain = std::thread::spawn(move || {
        while !sd.load(Ordering::Relaxed) {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(30)) {
                p_clone.lock().advance(&b);
            }
        }
    });

    // Wait for shell prompt to settle
    std::thread::sleep(Duration::from_millis(1500));

    // Disable echo? No — we WANT echo, that's the round trip.
    // Send 200 unique single chars one at a time and time how long until
    // we see them in the grid.
    let mut samples: Vec<u128> = Vec::with_capacity(200);
    // We tag each key with a unique alpha char from a..z, but the shell
    // would echo any printable. To find "the latest echoed char" we look
    // at the cursor cell after each send.
    for _i in 0..200 {
        let key = b"a"; // arbitrary; we only care about round trip
        let prev_pos = parser.lock().grid().cursor;
        let send_at = Instant::now();
        pty.in_tx.send(key.to_vec()).unwrap();
        // Spin-wait up to 50ms for the cursor to move (= echo received +
        // parsed + grid updated).
        let deadline = send_at + Duration::from_millis(50);
        loop {
            let cur = parser.lock().grid().cursor;
            if cur != prev_pos {
                samples.push(send_at.elapsed().as_micros());
                break;
            }
            if Instant::now() >= deadline {
                samples.push(50_000);
                break;
            }
            std::thread::sleep(Duration::from_micros(50));
        }
    }
    pty.in_tx.send(b"\x03\rexit\r".to_vec()).unwrap(); // ctrl-c + exit
    std::thread::sleep(Duration::from_millis(100));
    shutdown.store(true, Ordering::Relaxed);
    let _ = drain.join();

    samples.sort_unstable();
    let n = samples.len();
    r.typing_echo_latency_us_p50 = samples[n / 2];
    r.typing_echo_latency_us_p95 = samples[(n * 95) / 100];
    r.typing_echo_latency_us_p99 = samples[(n * 99) / 100];
}

/// Measure a heavy output burst (2000 lines via `for`-loop echo) +
/// parser throughput + grid walk throughput.
///
/// As of B2, the scroll loop simulates a real frame-per-batch render by
/// invoking a `CachedWalker` (mirroring `sonicterm-shared`'s row cache) on
/// every batch. This means `scroll_throughput_lines_per_sec` actually
/// reflects parse + render cost together, and improvements to dirty-row
/// tracking move the number. The loop also early-exits once data stops
/// arriving for a quarter-second window, so a faster pipeline finishes
/// sooner and shows a higher throughput instead of being clipped by a
/// fixed 6-second wall.
fn measure_scroll_and_parse(r: &mut Report) {
    let pty = PtyHandle::spawn_default_shell(120, 40, sonicterm_core::pty::ShellSpawnOpts::default()).expect("spawn");
    let mut parser = Parser::new(Grid::new(120, 40));
    std::thread::sleep(Duration::from_millis(800));
    while let Ok(b) = pty.out_rx.try_recv() {
        parser.advance(&b);
    }
    // Prime the cache with one walk so the steady-state numbers below
    // measure cache hits, not cold-start.
    {
        let g = parser.grid_mut();
        let mut warm = CachedWalker::new();
        warm.walk(g);
        g.clear_dirty();
    }

    pty.in_tx
        .send(b"for i in {1..2000}; do echo \"line $i with some content\"; done\r".to_vec())
        .unwrap();

    let mut total_parse = Duration::ZERO;
    let mut bytes = 0;
    let mut batches = 0;
    let mut walker = CachedWalker::new();
    let start = Instant::now();
    let hard_deadline = Duration::from_secs(6);
    let idle_exit = Duration::from_millis(250);
    let mut last_data = Instant::now();
    let mut burst_started = false;
    loop {
        if start.elapsed() > hard_deadline {
            break;
        }
        match pty.out_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(b) => {
                bytes += b.len();
                batches += 1;
                let t = Instant::now();
                parser.advance(&b);
                total_parse += t.elapsed();
                // Per-batch render simulation: walk the grid (with the
                // row cache) and clear dirty bits, exactly as the GPU
                // renderer would after presenting a frame. This is what
                // makes B2 visible in the throughput number.
                let g = parser.grid_mut();
                walker.walk(g);
                g.clear_dirty();
                burst_started = true;
                last_data = Instant::now();
            }
            Err(_) => {
                if burst_started && last_data.elapsed() > idle_exit {
                    break;
                }
            }
        }
    }
    let elapsed = start.elapsed().saturating_sub(idle_exit);
    pty.in_tx.send(b"exit\r".to_vec()).unwrap();

    r.scroll_bytes = bytes;
    r.scroll_batches = batches;
    r.scroll_throughput_lines_per_sec = (2000.0 / elapsed.as_secs_f64().max(0.001)) as u64;
    r.parse_ns_per_byte = total_parse.as_nanos() / bytes.max(1) as u128;
    r.parse_ns_per_batch = total_parse.as_nanos() / batches.max(1) as u128;

    // Grid walk throughput, post-burst, with a primed cache: this is
    // the steady-state cost of "render an unchanged screen", which
    // should be near-zero with B2 dirty-row tracking.
    let g = parser.grid_mut();
    let mut total_walk = Duration::ZERO;
    const FRAMES: usize = 1000;
    let mut cached = CachedWalker::new();
    // Prime once so the first iteration isn't measuring cold-cache work.
    cached.walk(g);
    g.clear_dirty();
    for _ in 0..FRAMES {
        let t = Instant::now();
        let _ = cached.walk(g);
        total_walk += t.elapsed();
        g.clear_dirty();
    }
    r.grid_walk_us_per_frame = (total_walk.as_micros()) / FRAMES as u128;

    // B3: glyph-atlas walk simulation. Mirrors what the GPU text
    // pipeline will do every frame: derive a GlyphKey per cell, look it
    // up in a HashMap, insert on miss. Pure CPU — no GPU dependency.
    // First, warm the map by walking once so steady-state numbers are
    // hits, not first-fill misses.
    let mut glyph = GlyphWalker::default();
    glyph.walk(g); // warm-up
    let warm_unique = glyph.unique();
    glyph.reset_counters();
    let mut total_glyph = Duration::ZERO;
    for _ in 0..FRAMES {
        let t = Instant::now();
        glyph.walk(g);
        total_glyph += t.elapsed();
    }
    r.glyph_walk_us_per_frame = total_glyph.as_micros() / FRAMES as u128;
    r.glyph_atlas_unique_keys = glyph.unique().max(warm_unique);
    r.glyph_walk_hit_rate_pct = glyph.hit_rate_pct();
}

fn measure_idle(r: &mut Report) {
    // We can't measure our own CPU% from inside the process cheaply
    // without sampling threads; the value reported here is "what the
    // VT thread consumed sampling pty for 1s of quiescence" (proxy for
    // total idle behavior since the renderer isn't in the loop).
    let pty = PtyHandle::spawn_default_shell(120, 40, sonicterm_core::pty::ShellSpawnOpts::default()).expect("spawn");
    let parser = Arc::new(parking_lot::Mutex::new(Parser::new(Grid::new(120, 40))));
    std::thread::sleep(Duration::from_millis(1500));

    // 1 second of pure idle: nothing should arrive, drain channel.
    let start = Instant::now();
    let mut wakeups = 0;
    let rx = pty.out_rx.clone();
    while start.elapsed() < Duration::from_secs(1) {
        if rx.recv_timeout(Duration::from_millis(50)).is_ok() {
            wakeups += 1;
        }
    }
    // Proxy: wakeups during 1s of quiescence × 0.01% per wakeup
    r.idle_cpu_pct = (wakeups as f64) * 0.01;

    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
    drop(parser);
}

// ------- copy of render.rs's grid → spans walker, kept pure-CPU -------

#[allow(dead_code)]
struct SpanDesc {
    range: std::ops::Range<usize>,
    fg: (u8, u8, u8, u8),
    weight: u16,
    italic: bool,
}

#[allow(dead_code)]
fn walk_grid(grid: &Grid) -> (String, Vec<SpanDesc>, Vec<(u16, u16, u16)>) {
    let mut text = String::with_capacity((grid.cols as usize + 1) * grid.rows as usize);
    let mut spans = Vec::new();
    let mut underlines = Vec::new();

    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut run_start = text.len();
        let mut run_fg = (255u8, 255, 255, 255);
        let mut run_weight = 400u16;
        let mut run_italic = false;
        let mut run_has = false;
        let mut ul_start: Option<u16> = None;
        let mut last_col = 0u16;
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
    std::io::stdout().flush().ok();
    (text, spans, underlines)
}

/// Mirror of `sonicterm-shared::render`'s per-row cache. Pure-CPU so it can
/// live in the headless bench: walks each grid row, but skips rows
/// `Grid::is_row_dirty` reports as clean and splices in cached output
/// instead. Caller is responsible for calling `Grid::clear_dirty` after
/// each `walk()`, exactly like the GPU renderer does after a frame.
struct CachedRow {
    text: String,
    spans: Vec<SpanDesc>,
    underlines: Vec<(u16, u16)>,
}

struct CachedWalker {
    rows: Vec<Option<CachedRow>>,
    cols: u16,
}

impl CachedWalker {
    fn new() -> Self {
        Self { rows: Vec::new(), cols: 0 }
    }

    fn walk(&mut self, grid: &Grid) -> (String, Vec<SpanDesc>, Vec<(u16, u16, u16)>) {
        if self.cols != grid.cols || self.rows.len() != grid.rows as usize {
            self.rows.clear();
            self.rows.resize_with(grid.rows as usize, || None);
            self.cols = grid.cols;
        }

        let mut text = String::with_capacity((grid.cols as usize + 1) * grid.rows as usize);
        let mut spans = Vec::new();
        let mut underlines = Vec::new();

        for r in 0..grid.rows {
            let dirty = grid.is_row_dirty(r);
            let row_base = text.len();
            let reuse = !dirty && self.rows.get(r as usize).map(|c| c.is_some()).unwrap_or(false);

            if reuse {
                let c = self.rows[r as usize].as_ref().unwrap();
                text.push_str(&c.text);
                for sd in &c.spans {
                    spans.push(SpanDesc {
                        range: (row_base + sd.range.start)..(row_base + sd.range.end),
                        fg: sd.fg,
                        weight: sd.weight,
                        italic: sd.italic,
                    });
                }
                for (a, b) in &c.underlines {
                    underlines.push((r, *a, *b));
                }
            } else {
                let row_start = text.len();
                let row = grid.row(r);
                let mut run_start = text.len();
                let mut run_fg = (255u8, 255, 255, 255);
                let mut run_weight = 400u16;
                let mut run_italic = false;
                let mut run_has = false;
                let mut row_spans: Vec<SpanDesc> = Vec::new();
                let mut ul_start: Option<u16> = None;
                let mut last_col = 0u16;
                let mut row_uls: Vec<(u16, u16)> = Vec::new();
                for (col, cell) in row.iter().enumerate() {
                    if cell.flags.contains(CellFlags::WIDE_CONT) {
                        continue;
                    }
                    let fg = match cell.fg {
                        Color::Rgb(rr, g, b) => (rr, g, b, 255),
                        _ => (255, 255, 255, 255),
                    };
                    let weight = if cell.flags.contains(CellFlags::BOLD) { 700 } else { 400 };
                    let italic = cell.flags.contains(CellFlags::ITALIC);
                    if run_has && (fg != run_fg || weight != run_weight || italic != run_italic) {
                        let frame_range = run_start..text.len();
                        row_spans.push(SpanDesc {
                            range: (frame_range.start - row_start)..(frame_range.end - row_start),
                            fg: run_fg,
                            weight: run_weight,
                            italic: run_italic,
                        });
                        spans.push(SpanDesc {
                            range: frame_range,
                            fg: run_fg,
                            weight: run_weight,
                            italic: run_italic,
                        });
                        run_start = text.len();
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
                        let end = last_col.saturating_sub(1);
                        underlines.push((r, s, end));
                        row_uls.push((s, end));
                    }
                }
                if let Some(s) = ul_start.take() {
                    underlines.push((r, s, last_col));
                    row_uls.push((s, last_col));
                }
                if run_has {
                    let frame_range = run_start..text.len();
                    row_spans.push(SpanDesc {
                        range: (frame_range.start - row_start)..(frame_range.end - row_start),
                        fg: run_fg,
                        weight: run_weight,
                        italic: run_italic,
                    });
                    spans.push(SpanDesc {
                        range: frame_range,
                        fg: run_fg,
                        weight: run_weight,
                        italic: run_italic,
                    });
                }
                let row_text = text[row_start..].to_string();
                self.rows[r as usize] =
                    Some(CachedRow { text: row_text, spans: row_spans, underlines: row_uls });
            }
            text.push('\n');
        }
        (text, spans, underlines)
    }
}

// ------- B3: glyph-atlas walker (pure CPU, no GPU) ----------------------
//
// Mirrors what `sonicterm_shared::glyph_atlas::GlyphAtlas` will do every
// frame: derive a `GlyphKey` per non-WIDE_CONT cell and look it up in a
// `HashMap`, populating on miss. The "fake GlyphInfo" stored is the
// same shape as the real one (uv rect + advance) so the bench's
// HashMap cache behavior matches production within a constant factor.

#[derive(Clone, Copy, Default)]
struct FakeGlyphInfo {
    uv: [f32; 4],
    advance: f32,
}

#[derive(Default)]
struct GlyphWalker {
    map: std::collections::HashMap<GlyphKey, FakeGlyphInfo>,
    hits: u64,
    misses: u64,
    // Counter we return from walk() so the optimizer can't elide the
    // hot loop. Not used for reporting.
    sink: usize,
}

impl GlyphWalker {
    fn walk(&mut self, grid: &Grid) -> usize {
        let mut counter = 0usize;
        for r in 0..grid.rows {
            for cell in grid.row(r).iter() {
                let Some(key) = GlyphKey::from_cell(cell) else { continue };
                use std::collections::hash_map::Entry;
                match self.map.entry(key) {
                    Entry::Occupied(_) => self.hits += 1,
                    Entry::Vacant(v) => {
                        self.misses += 1;
                        // Fake "rasterize": placeholder UV at origin, advance 1.
                        v.insert(FakeGlyphInfo { uv: [0.0, 0.0, 0.05, 0.05], advance: 1.0 });
                    }
                }
                counter += 1;
            }
        }
        self.sink = self.sink.wrapping_add(counter);
        counter
    }

    fn unique(&self) -> usize {
        self.map.len()
    }

    fn reset_counters(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    fn hit_rate_pct(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        (self.hits as f64 / total as f64) * 100.0
    }
}
