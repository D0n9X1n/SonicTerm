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
    // Regression for #387: after the active pane is closed in a
    // horizontal split, the surviving sibling's PaneRect grows to cover
    // the whole parent. `App::close_active_pane` must re-run the layout
    // -> resize_panes_to_rects flow so the survivor's Grid + PtyHandle
    // grow to the reclaimed area. Pre-fix the survivor kept its
    // split-time half-width grid AND its PTY was never told the new
    // size, so shell output wrapped at the narrow column count until the
    // OS window was resized.
    //
    // This test goes through the production entry point
    // `App::close_active_pane` (via the doc-hidden
    // `__test_invoke_close_active_pane` shim, matching the pattern of
    // every other test-only invoker on `App`). The renderer-derived
    // viewport metrics that `compute_active_pane_rects` and
    // `resize_visible_panes` normally read from `main_renderer()` are
    // substituted via `App::test_viewport_override` so the test runs
    // without a live wgpu surface — the close + resize wiring under
    // test is the same production code path either way (PR #393
    // cycle 3 review feedback: previous cycles bypassed
    // close_active_pane by calling PaneTree::close + resize_panes_to_rects
    // directly; this cycle drives the real entry point).
    use sonic_app::app::{App, TabState};
    use sonic_core::config::Config;
    use sonic_core::keymap::{Keymap, Meta};
    use sonic_core::pty::PtyHandle;
    use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
    use sonic_ui::tabs::Tab;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn hex() -> Hex {
        Hex("#000000".to_string())
    }
    fn ansi() -> AnsiColors {
        AnsiColors {
            black: hex(),
            red: hex(),
            green: hex(),
            yellow: hex(),
            blue: hex(),
            magenta: hex(),
            cyan: hex(),
            white: hex(),
        }
    }
    fn synth_theme() -> Theme {
        Theme {
            name: "test".into(),
            appearance: Appearance::Dark,
            colors: Palette {
                background: hex(),
                foreground: hex(),
                cursor: hex(),
                cursor_text: hex(),
                selection_bg: hex(),
                selection_fg: hex(),
                ansi: ansi(),
                bright: ansi(),
                tab: TabColors {
                    bar_bg: hex(),
                    active_bg: hex(),
                    active_fg: hex(),
                    inactive_bg: hex(),
                    inactive_fg: hex(),
                    hover_bg: hex(),
                    hover_fg: hex(),
                    close_button_fg: hex(),
                },
            },
        }
    }
    fn synth_app() -> App {
        let keymap =
            Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
        App::new(synth_theme(), Config::default(), keymap)
    }

    // Build a spy PtyHandle whose `resize` closure increments a counter
    // and records the last (cols, rows). Production close path is the
    // only thing that should bump this after split-time setup.
    type Spy = (PaneState, Arc<Mutex<Parser>>, Arc<AtomicU32>, Arc<Mutex<(u16, u16)>>);
    fn make_pane_with_spy_pty(cols: u16, rows: u16) -> Spy {
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

    // === Set up an App with one tab whose pane tree is a horizontal
    // split of two leaves (pane ids 1 and 2). Pane 2 is active — the
    // one close_active_pane should close. ===
    let mut app = synth_app();
    // __test_synthetic_main creates the main WindowState entry without
    // a real winit window / wgpu renderer; viewport metrics come from
    // test_viewport_override below.
    app.__test_synthetic_main();

    let (pane_a, parser_a, calls_a, last_a) = make_pane_with_spy_pty(100, 35);
    let (pane_b, _parser_b, _calls_b, _last_b) = make_pane_with_spy_pty(100, 35);

    // Build the split tree directly: PaneTree::leaf(1) then split right
    // to add pane 2 — the same shape that App::split_active_pane would
    // produce after a Cmd+D on a single-pane tab.
    let mut tree = PaneTree::leaf(1);
    assert!(tree.split(1, Direction::Right, 2));

    let ws = app.main_mut().expect("synthetic main exists");
    ws.tabs.push(Tab::new("test"));
    ws.tab_states.push(TabState::new(tree, /*active_pane=*/ 2));
    ws.panes.insert(1, pane_a);
    ws.panes.insert(2, pane_b);

    // Inject the viewport: 1000x700 logical, 10x20 cell metrics. This is
    // what production code would read from main_renderer().
    let outer = Rect::new(0.0, 0.0, 1000.0, 700.0);
    app.test_viewport_override = Some((outer, 10.0, 20.0));

    // Split-time resize: route through the SAME production helper
    // (window_event.rs / config_apply.rs use resize_panes_to_rects on
    // the layout produced by PaneTree::layout). After this pane_a is 50
    // cols wide and its spy PTY has fired exactly once.
    let split_rects = {
        let st = &app.main().unwrap().tab_states[0];
        st.tree.layout(outer)
    };
    resize_panes_to_rects(app.main_panes().unwrap(), &split_rects, 10.0, 20.0);
    assert_eq!(parser_a.lock().grid().cols, 50, "left starts at half width");
    assert_eq!(calls_a.load(Ordering::SeqCst), 1, "split-time PTY resize fired once");
    assert_eq!(*last_a.lock(), (50, 35));

    // === Exercise the production close path. ===
    // __test_invoke_close_active_pane calls App::close_active_pane
    // directly — the same entry point Cmd+W reaches via keymap dispatch
    // (keymap_dispatch.rs::Action::CloseActivePane). It:
    //   1. Removes the focused (=pane 2) leaf from the tree.
    //   2. Drops pane 2's PaneState (Drop on PtyHandle kills the child).
    //   3. Calls resize_visible_panes -> compute_active_pane_rects ->
    //      resize_panes_to_rects on the new layout, which MUST fire the
    //      survivor's Grid.resize + PtyHandle.resize.
    //   4. Requests a redraw on the main window (no-op here — synthetic
    //      WindowState has window=None, which `if let Some(w) = ...`
    //      tolerates).
    let pre_close_calls = calls_a.load(Ordering::SeqCst);
    app.__test_invoke_close_active_pane();
    let post_close_calls = calls_a.load(Ordering::SeqCst);

    // Both halves of the #387 fix:
    // (a) Spy PTY's resize was called exactly once more by the production
    //     close path. This is the property the previous pty=None test
    //     could not verify.
    assert_eq!(
        post_close_calls - pre_close_calls,
        1,
        "App::close_active_pane must invoke survivor's PtyHandle::resize exactly once"
    );
    // (b) The PTY received the survivor's full-pane dims (100x35 — the
    //     reclaimed full window).
    assert_eq!(
        *last_a.lock(),
        (100, 35),
        "PtyHandle::resize must receive the post-close (cols, rows)"
    );
    // (c) Grid widened to full pane width.
    assert_eq!(
        parser_a.lock().grid().cols,
        100,
        "survivor's Grid must reflow to full pane width after sibling close (#387)"
    );
    assert_eq!(parser_a.lock().grid().rows, 35);

    // Tree collapsed and the closed pane's state was dropped.
    let ws = app.main().unwrap();
    let st = &ws.tab_states[0];
    assert_eq!(st.tree.leaves(), vec![1], "tree collapsed to surviving leaf");
    assert!(!ws.panes.contains_key(&2), "closed pane's PaneState dropped");
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
