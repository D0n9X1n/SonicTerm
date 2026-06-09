//! Issue #43 behavioral test: a torn-out CHILD window's deferred-redraw
//! latch (`pending_redraw_windows`, driven by `defer_child_redraw`).
//!
//! The pure gate predicate (`should_defer_streaming_redraw`) is unit-tested
//! in `mod.rs`; this asserts the latch bookkeeping the child render path and
//! `new_events`/`about_to_wait` rely on: deferring records the window, and a
//! distinct window is tracked independently. This is the state that stops a
//! child from busy-spinning the VT thread's parser lock during an `ls -al`
//! burst (smooth in main, previously laggy in a child).

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

#[test]
fn defer_child_redraw_records_then_is_independent_per_window() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let a = app.__test_seed_child_window(&["a"]);
    let b = app.__test_seed_child_window(&["b"]);

    // Nothing deferred initially.
    assert!(!app.__test_child_redraw_deferred(a));
    assert!(!app.__test_child_redraw_deferred(b));

    // Defer a streaming redraw on `a` (was_dirty=false → coalescing path).
    app.defer_child_redraw(a, false);
    assert!(app.__test_child_redraw_deferred(a), "a must be queued for the next frame boundary");
    assert!(!app.__test_child_redraw_deferred(b), "deferral is per-window, b untouched");

    // Defer on `b` too; both tracked independently.
    app.defer_child_redraw(b, false);
    assert!(app.__test_child_redraw_deferred(a));
    assert!(app.__test_child_redraw_deferred(b));
}

#[test]
fn deferred_input_driven_redraw_preserves_dirty_flag() {
    // When an input-driven redraw is deferred (e.g. lost a try_lock race),
    // the was_dirty flag must be preserved so it bypasses the coalescing
    // gate when it re-fires — input must never get stuck behind the vsync
    // deadline.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let w = app.__test_seed_child_window(&["w"]);
    app.defer_child_redraw(w, true);
    assert!(app.__test_child_redraw_deferred(w));
    assert!(app.__test_input_dirty(), "input-driven deferral must keep input_dirty set");
}
