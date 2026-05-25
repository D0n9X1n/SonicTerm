//! Reproduce the "Enter needs N presses" symptom in a headless way.
//!
//! Simulates the v0.6 app loop: a VT thread that throttles redraw requests
//! and a "main" thread that reads the grid on redraw. We then send "ls\n"
//! to the shell and verify that the FINAL pty batch (which carries the
//! prompt redraw) is followed by a redraw flush within a short timeout.
//!
//! Run: `cargo run --example enter_repro -p sonic-core --release`
//! Exits 0 if the trailing redraw fires within 100ms after the last batch,
//! 1 otherwise.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonic_core::{grid::Grid, pty::PtyHandle, vt::Parser};

fn main() {
    let pty = PtyHandle::spawn_default_shell(120, 40).expect("spawn");
    let parser = Arc::new(parking_lot::Mutex::new(Parser::new(Grid::new(120, 40))));

    let redraws = Arc::new(AtomicUsize::new(0));
    let last_redraw_at = Arc::new(parking_lot::Mutex::new(Instant::now()));
    let last_batch_at = Arc::new(parking_lot::Mutex::new(Instant::now()));
    let shutdown = Arc::new(AtomicBool::new(false));

    // ---- VT thread (same logic as sonic-shared/src/app.rs) ----
    let p = parser.clone();
    let out_rx = pty.out_rx.clone();
    let redraws_clone = redraws.clone();
    let last_redraw_clone = last_redraw_at.clone();
    let last_batch_clone = last_batch_at.clone();
    let shutdown_clone = shutdown.clone();
    let handle = std::thread::spawn(move || {
        let mut last_request = Instant::now() - Duration::from_secs(1);
        let mut pending = false;
        let min_interval = Duration::from_millis(16);
        loop {
            if shutdown_clone.load(Ordering::Relaxed) {
                break;
            }
            let timeout =
                if pending { min_interval } else { Duration::from_millis(200) };
            match out_rx.recv_timeout(timeout) {
                Ok(bytes) => {
                    *last_batch_clone.lock() = Instant::now();
                    let mut g = p.lock();
                    g.advance(&bytes);
                    drop(g);
                    if last_request.elapsed() >= min_interval {
                        redraws_clone.fetch_add(1, Ordering::Relaxed);
                        *last_redraw_clone.lock() = Instant::now();
                        last_request = Instant::now();
                        pending = false;
                    } else {
                        pending = true;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if pending {
                        redraws_clone.fetch_add(1, Ordering::Relaxed);
                        *last_redraw_clone.lock() = Instant::now();
                        last_request = Instant::now();
                        pending = false;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Give the shell time to print its prompt
    std::thread::sleep(Duration::from_millis(1500));
    let pre_ls_redraws = redraws.load(Ordering::Relaxed);
    println!("[startup] {pre_ls_redraws} redraws after shell init");

    // Type "ls\r"
    pty.in_tx.send(b"echo HELLO-ENTER-TEST\r".to_vec()).unwrap();

    // Wait 500ms and inspect
    std::thread::sleep(Duration::from_millis(500));
    let post_ls_redraws = redraws.load(Ordering::Relaxed);
    let last_batch = *last_batch_at.lock();
    let last_redraw = *last_redraw_at.lock();
    let lag = last_redraw.saturating_duration_since(last_batch);

    println!("[after echo] {} new redraws", post_ls_redraws - pre_ls_redraws);
    println!("[after echo] last_redraw is {:?} AFTER last_batch (should be small)", lag);

    // Verify HELLO-ENTER-TEST landed in the grid
    let g = parser.lock();
    let mut found = false;
    for r in 0..g.grid().rows {
        let row: String = g.grid().row(r).iter().map(|c| c.ch).collect();
        if row.contains("HELLO-ENTER-TEST") {
            found = true;
            println!("[grid] found at row {}: {}", r, row.trim());
        }
    }
    drop(g);
    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    shutdown.store(true, Ordering::Relaxed);
    let _ = handle.join();

    if !found {
        eprintln!("FAIL: HELLO-ENTER-TEST never landed in grid");
        std::process::exit(1);
    }
    // Lag must be <= min_interval + slack (32ms). If lag > 200ms the trailing
    // redraw was lost.
    if lag > Duration::from_millis(100) {
        eprintln!("FAIL: last redraw {} after last batch — Enter bug present", format!("{lag:?}"));
        std::process::exit(1);
    }
    println!("[enter_repro] OK");
}
