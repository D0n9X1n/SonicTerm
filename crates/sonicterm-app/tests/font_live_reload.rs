//! Tests for the font live-reload review fixes on PR #53:
//!
//! 1. After a font change the per-pane Grid (and PTY, when present)
//!    must be resized to match the renderer's new cell metrics —
//!    otherwise the grid keeps drawing at the old `(cols, rows)` and
//!    the shell believes the window is a different size from what is
//!    actually painted.
//!
//! 2. The config watcher must actively wake the (idle) event loop on
//!    every delivery, not just push into a channel that the main loop
//!    will drain "eventually" on the next OS event. Without the wake,
//!    a reload sits queued under `winit::ControlFlow::Wait` until an
//!    unrelated key/mouse/PTY event happens to fire.
//!
//! Both tests run without a live wgpu surface — the resize invariant
//! is exercised against the `resize_all_panes` helper that the live
//! path calls; the wake invariant is exercised against
//! `ConfigWatcher::spawn_with_wake`, the same hook the App uses to
//! send `UserEvent::ConfigChanged` through its `EventLoopProxy`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use sonicterm_app::app::{resize_all_panes, PaneState};
use sonicterm_app::config_watch::ConfigWatcher;
use sonicterm_core::grid::Grid;
use sonicterm_core::vt::Parser;

fn make_pane(cols: u16, rows: u16) -> PaneState {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows))));
    // pty = None mirrors test scenarios that don't spawn a real shell;
    // resize_all_panes must tolerate the missing handle (we still
    // resize the grid).
    PaneState::new(parser, None)
}

#[test]
fn font_change_resizes_all_panes() {
    // Simulate two panes (different tabs) starting at the old cell
    // grid (80x24) and then a font change that yields new metrics
    // fitting (120x36) inside the same window.
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, make_pane(80, 24));
    panes.insert(2, make_pane(80, 24));

    // Sanity: starting dimensions.
    for p in panes.values() {
        let g = p.parser.lock();
        assert_eq!(g.grid().cols, 80);
        assert_eq!(g.grid().rows, 24);
    }

    // The live path is: renderer.set_font(...) → renderer.cells()
    // returns (new_cols, new_rows) → resize_all_panes(panes, new_cols,
    // new_rows). We invoke the same helper directly with synthetic
    // post-font-change metrics.
    resize_all_panes(&panes, 120, 36);

    for p in panes.values() {
        let g = p.parser.lock();
        assert_eq!(g.grid().cols, 120, "pane grid cols must match new renderer.cells()");
        assert_eq!(g.grid().rows, 36, "pane grid rows must match new renderer.cells()");
    }
}

fn write_atomic(path: &std::path::Path, body: &str) {
    let mut f = fs::File::create(path).expect("create config");
    f.write_all(body.as_bytes()).expect("write config");
    f.sync_all().ok();
}

#[test]
fn watcher_wakes_loop_on_delivery() {
    // The app wires `EventLoopProxy::send_event(UserEvent::ConfigChanged)`
    // into `spawn_with_wake`. We can't construct a real proxy in a unit
    // test (winit requires an EventLoop on the main thread), so we
    // substitute the same shape: a `Fn() + Send + 'static` callback
    // that flips a flag. If the wake hook fires, the flag is true
    // within the same delivery window the channel publishes.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sonicterm.toml");
    write_atomic(
        &path,
        r#"
theme = "dracula"
keymap = "sonicterm"

[font]
family = "JetBrains Mono"
size = 13.0
line_height = 1.2
"#,
    );

    let woken = Arc::new(AtomicBool::new(false));
    let woken_clone = woken.clone();
    let w = ConfigWatcher::spawn_with_wake(path.clone(), move || {
        woken_clone.store(true, Ordering::SeqCst);
    })
    .expect("spawn watcher");

    // Let the watcher settle + drain any pre-watch FSEvents replays
    // (macOS) so we measure the response to OUR write, not the
    // registration echo.
    std::thread::sleep(Duration::from_millis(250));
    while w.recv_timeout(Duration::from_millis(50)).is_some() {}
    woken.store(false, Ordering::SeqCst);

    write_atomic(
        &path,
        r#"
theme = "nord"
keymap = "sonicterm"

[font]
family = "JetBrains Mono"
size = 14.0
line_height = 1.2
"#,
    );

    // Drain (mirrors what the app's user_event handler does after a
    // wake) — proves a config did land on the channel.
    let got = w.recv_timeout(Duration::from_millis(1500)).expect("config delivered");
    assert_eq!(got.theme, "nord");

    // The wake callback must have fired by the time the config is
    // available (it's called immediately after the channel send on the
    // watcher thread). A small grace window covers the cross-thread
    // ordering between the channel send and the AtomicBool store.
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while !woken.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        woken.load(Ordering::SeqCst),
        "watcher wake callback must fire on every delivery so the idle event loop is woken"
    );
}
