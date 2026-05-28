//! Regression test for per-pane grid sizing under split layouts.
//!
//! Pre-fix, every pane was resized to the whole window's `(cols, rows)`
//! via `resize_all_panes`. With splits this is wrong: an inactive pane
//! whose `PaneRect` only covers half the window thought it was
//! full-window-wide, so TUIs (vim, htop) drew past their visible border
//! and the wrap column was off.
//!
//! After the fix each pane sizes to its own `PaneRect`, as produced by
//! `PaneTree::layout`. See `docs/specs/per-pane-grids.md`.
//!
//! Runs without a live wgpu surface — exercises `resize_panes_to_rects`
//! against a synthetic pane map and an explicit rect list, the same
//! invariant the WindowEvent::Resized and config-live-reload paths use.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use sonic_app::app::{resize_panes_to_rects, PaneState};
use sonic_core::grid::Grid;
use sonic_core::vt::Parser;
use sonic_ui::pane::Rect;

fn make_pane(cols: u16, rows: u16) -> (PaneState, Arc<Mutex<Parser>>) {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows))));
    // pty = None mirrors the no-real-shell test scenarios; the helper
    // must tolerate the missing PTY handle and still resize the grid.
    (PaneState::new(parser.clone(), None), parser)
}

#[test]
fn split_panes_size_to_their_own_rects() {
    // Two panes splitting a 1000×700 logical window vertically.
    // Cell metrics: 10×20. The whole-window count would be 100×35 cells;
    // each half-pane must instead end up at 50×35.
    let (pane_a, parser_a) = make_pane(80, 24);
    let (pane_b, parser_b) = make_pane(80, 24);
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, pane_a);
    panes.insert(2, pane_b);

    let rects = vec![
        (1u64, Rect::new(0.0, 0.0, 500.0, 700.0)),
        (2u64, Rect::new(500.0, 0.0, 500.0, 700.0)),
    ];
    resize_panes_to_rects(&panes, &rects, 10.0, 20.0);

    assert_eq!(parser_a.lock().grid().cols, 50);
    assert_eq!(parser_a.lock().grid().rows, 35);
    assert_eq!(parser_b.lock().grid().cols, 50);
    assert_eq!(parser_b.lock().grid().rows, 35);
}

#[test]
fn empty_rects_is_noop() {
    // The brief window during tab-close can produce an empty rects list;
    // the helper must not panic and must not mutate any grid.
    let (pane, parser) = make_pane(80, 24);
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, pane);

    resize_panes_to_rects(&panes, &[], 10.0, 20.0);
    assert_eq!(parser.lock().grid().cols, 80);
    assert_eq!(parser.lock().grid().rows, 24);
}

#[test]
fn subcell_rect_floors_to_one() {
    // A rect smaller than one cell still must floor to >=1 in each
    // dimension — same invariant as `Renderer::cells()`.
    let (pane, parser) = make_pane(80, 24);
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, pane);

    let rects = vec![(1u64, Rect::new(0.0, 0.0, 3.0, 5.0))];
    resize_panes_to_rects(&panes, &rects, 10.0, 20.0);

    assert!(parser.lock().grid().cols >= 1);
    assert!(parser.lock().grid().rows >= 1);
    assert_eq!(parser.lock().grid().cols, 1);
    assert_eq!(parser.lock().grid().rows, 1);
}

#[test]
fn unknown_pane_id_is_skipped() {
    // Pane id present in the rect list but missing from the pane map
    // (also reachable during tab close) must be silently skipped.
    let (pane, parser) = make_pane(80, 24);
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, pane);

    let rects =
        vec![(99u64, Rect::new(0.0, 0.0, 500.0, 700.0)), (1u64, Rect::new(0.0, 0.0, 300.0, 200.0))];
    resize_panes_to_rects(&panes, &rects, 10.0, 20.0);

    assert_eq!(parser.lock().grid().cols, 30);
    assert_eq!(parser.lock().grid().rows, 10);
}
