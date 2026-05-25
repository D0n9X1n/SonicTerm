//! Headless performance harness.
//!
//! Runs the full Sonic stack (PTY + Parser + Grid + spans-builder) without
//! a window or GPU and measures every interesting number. Emits one JSON
//! line so callers can diff `before.json` vs `after.json`.
//!
//! Run: `cargo run --release -p sonic-core --example bench [scenario]`
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
//!   cargo run --release -p sonic-core --example bench -- all > before.json
//!   # … apply perf changes …
//!   cargo run --release -p sonic-core --example bench -- all > after.json
//!   diff <(jq -S . before.json) <(jq -S . after.json)

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonic_core::{
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
    let pty = PtyHandle::spawn_default_shell(120, 40).expect("spawn");
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

/// Measure a heavy output burst (1000 lines via `for`-loop echo) +
/// parser throughput + grid walk throughput.
fn measure_scroll_and_parse(r: &mut Report) {
    let pty = PtyHandle::spawn_default_shell(120, 40).expect("spawn");
    let mut parser = Parser::new(Grid::new(120, 40));
    std::thread::sleep(Duration::from_millis(800));
    while let Ok(b) = pty.out_rx.try_recv() {
        parser.advance(&b);
    }

    pty.in_tx
        .send(b"for i in {1..2000}; do echo \"line $i with some content\"; done\r".to_vec())
        .unwrap();

    let mut total_parse = Duration::ZERO;
    let mut bytes = 0;
    let mut batches = 0;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(6) {
        if let Ok(b) = pty.out_rx.recv_timeout(Duration::from_millis(20)) {
            bytes += b.len();
            batches += 1;
            let t = Instant::now();
            parser.advance(&b);
            total_parse += t.elapsed();
        }
    }
    pty.in_tx.send(b"exit\r".to_vec()).unwrap();

    r.scroll_bytes = bytes;
    r.scroll_batches = batches;
    r.scroll_throughput_lines_per_sec =
        (2000.0 / start.elapsed().as_secs_f64().max(0.001)) as u64;
    r.parse_ns_per_byte = total_parse.as_nanos() / bytes.max(1) as u128;
    r.parse_ns_per_batch = total_parse.as_nanos() / batches.max(1) as u128;

    // Grid walk throughput (this is what the renderer does per frame
    // before glyph shaping — a pure-CPU cost we control).
    let g = parser.grid();
    let mut total_walk = Duration::ZERO;
    const FRAMES: usize = 1000;
    for _ in 0..FRAMES {
        let t = Instant::now();
        let _ = walk_grid(g);
        total_walk += t.elapsed();
    }
    r.grid_walk_us_per_frame = (total_walk.as_micros()) / FRAMES as u128;
}

fn measure_idle(r: &mut Report) {
    // We can't measure our own CPU% from inside the process cheaply
    // without sampling threads; the value reported here is "what the
    // VT thread consumed sampling pty for 1s of quiescence" (proxy for
    // total idle behavior since the renderer isn't in the loop).
    let pty = PtyHandle::spawn_default_shell(120, 40).expect("spawn");
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
