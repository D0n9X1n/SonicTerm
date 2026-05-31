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
use sonic_core::keymap::Direction;
use sonic_core::vt::Parser;
use sonic_ui::pane::{PaneTree, Rect};

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
fn newly_split_pane_is_resized_to_subrect_not_whole_window() {
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    let rects = tree.layout(Rect::new(0.0, 0.0, 1000.0, 700.0));

    let (pane_a, parser_a) = make_pane(100, 35);
    let (pane_b, parser_b) = make_pane(100, 35);
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, pane_a);
    panes.insert(2, pane_b);

    resize_panes_to_rects(&panes, &rects, 10.0, 20.0);

    assert_eq!(parser_a.lock().grid().cols, 50);
    assert_eq!(parser_b.lock().grid().cols, 50, "new pane must not keep whole-window cols");
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
fn close_sibling_pane_resizes_survivor_to_full_width() {
    // Regression for #387: after `PaneTree::close` removes one leaf of a
    // horizontal split, the surviving leaf's rect covers the whole
    // parent. The close path in `App::close_active_pane` must re-run
    // `resize_panes_to_rects` so the survivor's Grid + PTY grow to the
    // reclaimed area. Pre-fix the survivor kept its half-window cols and
    // shell output wrapped at the narrow split-time width.
    //
    // This test drives the exact sequence `App::close_active_pane`
    // performs in the surviving-sibling branch (`PaneTree::close` →
    // `panes.remove` → `resize_visible_panes` → `resize_panes_to_rects`)
    // and uses `PtyHandle::for_test` so it can assert the survivor's
    // `pty.resize` closure actually fires with the post-close cell
    // count — the property pre-fix code silently broke and that the
    // earlier `pty = None` version of this test could not catch.
    use sonic_core::pty::PtyHandle;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Mirror `make_pane` but with a spy-pty whose resize() bumps an
    // atomic counter and records the last (cols, rows) it was called
    // with. Built from `PtyHandle::for_test` (doc-hidden, test-only).
    type SpyPane = (PaneState, Arc<Mutex<Parser>>, Arc<AtomicU32>, Arc<Mutex<(u16, u16)>>);
    fn make_pane_with_spy_pty(cols: u16, rows: u16) -> SpyPane {
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows))));
        let calls = Arc::new(AtomicU32::new(0));
        let last = Arc::new(Mutex::new((0u16, 0u16)));
        let calls_c = calls.clone();
        let last_c = last.clone();
        let pty = PtyHandle::for_test(move |c, r| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            *last_c.lock() = (c, r);
        });
        (PaneState::new(parser.clone(), Some(pty)), parser, calls, last)
    }

    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));
    let outer = Rect::new(0.0, 0.0, 1000.0, 700.0);
    let split_rects = tree.layout(outer);

    // After the split each pane is half-width. Each pane gets a spy
    // PtyHandle that records resize calls.
    let (pane_a, parser_a, calls_a, last_a) = make_pane_with_spy_pty(100, 35);
    let (pane_b, _parser_b, _calls_b, _last_b) = make_pane_with_spy_pty(100, 35);
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, pane_a);
    panes.insert(2, pane_b);

    // Initial split-time resize lays both grids at 50 cols and fires
    // resize() on each spy PTY once.
    resize_panes_to_rects(&panes, &split_rects, 10.0, 20.0);
    assert_eq!(parser_a.lock().grid().cols, 50, "left starts at half width");
    assert_eq!(calls_a.load(Ordering::SeqCst), 1, "split-time PTY resize fired once");
    assert_eq!(*last_a.lock(), (50, 35));

    // === Exercise the post-close path. ===
    // This is exactly what App::close_active_pane does in the
    // surviving-sibling branch (spawn_pane.rs::close_active_pane):
    //   1. PaneTree::close removes the closed leaf.
    //   2. panes.remove drops its PaneState (and PtyHandle, which Drop
    //      kills the child).
    //   3. resize_visible_panes recomputes the layout and routes
    //      through resize_panes_to_rects against the new rects.
    assert!(tree.close(2));
    panes.remove(&2);
    let post_close_rects = tree.layout(outer);
    assert_eq!(post_close_rects.len(), 1, "tree collapsed to surviving leaf");
    assert_eq!(post_close_rects[0].0, 1);
    assert_eq!(post_close_rects[0].1.w, 1000.0, "survivor reclaims full width");

    // The fix: resize_panes_to_rects gets called with the post-close
    // layout. Pre-fix this call was missing entirely from
    // close_active_pane — the survivor kept its half-width grid AND
    // its PTY was never told the new size.
    resize_panes_to_rects(&panes, &post_close_rects, 10.0, 20.0);

    // Both halves of the fix:
    // (a) Grid widened to full pane width.
    assert_eq!(
        parser_a.lock().grid().cols,
        100,
        "survivor's Grid must reflow to full pane width after sibling close (#387)"
    );
    assert_eq!(parser_a.lock().grid().rows, 35);
    // (b) PTY was actually told about the resize (the part the
    //     previous pty = None test could not verify).
    assert_eq!(
        calls_a.load(Ordering::SeqCst),
        2,
        "survivor's PtyHandle::resize must fire again after sibling close (#387)"
    );
    assert_eq!(
        *last_a.lock(),
        (100, 35),
        "PtyHandle::resize must receive the post-close (cols, rows)"
    );
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
