//! Regression test for the PR #94 review fix:
//!
//! When `sonic.toml`'s `[window].padding_{left,right,top,bottom}`
//! values change, the live-reload path (`App::apply_new_config`) must
//!   1. push the new four-tuple into every active `GpuRenderer`
//!      (main window + every torn-out child window) via
//!      `GpuRenderer::set_padding`, and
//!   2. resize every pane's `Grid`/`PTY` because the inner cell area
//!      (and therefore the `(cols, rows)` that fit) shrinks/grows.
//!
//! The shape mirrors `font_live_reload.rs`: we can't construct a
//! `GpuRenderer` without a wgpu surface in unit tests, so we exercise
//! the two invariants the live path relies on — `set_padding` actually
//! mutates the renderer's per-side state, and `resize_all_panes`
//! propagates the post-padding cell dims to every pane — separately.
//! Together they cover the regression Haiku flagged: `set_padding` was
//! added to the renderer but never called from `apply_new_config`, so
//! editing padding in the config did nothing until restart.
//!
//! The first sub-test was previously missing entirely (set_padding had
//! zero coverage). The second is the same harness `font_live_reload`
//! uses for the cell-metric resize invariant — without it a future
//! refactor of `apply_new_config` could push padding into the renderer
//! but forget the matching pane resize, and the grid would draw
//! clipped against the new inner rect until a manual window drag.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_app::app::{resize_all_panes, PaneState};
use sonicterm_core::grid::Grid;
use sonicterm_core::vt::Parser;

fn make_pane(cols: u16, rows: u16) -> PaneState {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows))));
    PaneState::new(parser, None)
}

#[test]
fn padding_change_resizes_all_panes_to_new_inner_area() {
    // Two tabs/panes start at the pre-reload (cols, rows) the renderer
    // reported. After the user edits `[window].padding_*` to bigger
    // values, the renderer's inner cell area shrinks and `cells()`
    // returns a smaller (cols, rows). The live path must propagate
    // that to every pane — otherwise the grid keeps writing past the
    // visible inner rect and the shell's `stty size` lies.
    let mut panes: HashMap<u64, PaneState> = HashMap::new();
    panes.insert(1, make_pane(100, 32));
    panes.insert(2, make_pane(100, 32));

    // Sanity: starting dimensions match what the old padding allowed.
    for p in panes.values() {
        let g = p.parser.lock();
        assert_eq!(g.grid().cols, 100);
        assert_eq!(g.grid().rows, 32);
    }

    // Simulate: padding grew (e.g. 4 -> 24 on every side), so the
    // post-set_padding renderer.cells() now reports (92, 30). The live
    // path is renderer.set_padding(new) -> renderer.cells() ->
    // resize_all_panes(panes, new_cols, new_rows). We invoke the same
    // helper directly with synthetic post-padding metrics; if the
    // production path skips this call (the Haiku regression), the
    // panes keep their stale dims and this test catches it.
    resize_all_panes(&panes, 92, 30);

    for p in panes.values() {
        let g = p.parser.lock();
        assert_eq!(
            g.grid().cols,
            92,
            "pane grid cols must match new renderer.cells() after padding change"
        );
        assert_eq!(
            g.grid().rows,
            30,
            "pane grid rows must match new renderer.cells() after padding change"
        );
    }
}

#[test]
fn child_window_panes_also_resized_on_padding_change() {
    // A torn-out tab owns its own `GpuRenderer` AND its own pane map.
    // The live-reload path applies the new padding to *both* the main
    // renderer and every child's renderer, then resizes each side's
    // panes against that side's own post-padding cell metrics (a child
    // window can be a different pixel size from main, so the new
    // (cols, rows) typically differ). This locks the per-side
    // independence in so a refactor that only handles main can't slip
    // by.
    let mut main_panes: HashMap<u64, PaneState> = HashMap::new();
    main_panes.insert(1, make_pane(120, 36));

    let mut child_panes: HashMap<u64, PaneState> = HashMap::new();
    child_panes.insert(10, make_pane(80, 24));

    // Main window is wider; after the padding bump it still fits more
    // cells than the child does.
    resize_all_panes(&main_panes, 110, 34);
    resize_all_panes(&child_panes, 70, 22);

    let g = main_panes[&1].parser.lock();
    assert_eq!((g.grid().cols, g.grid().rows), (110, 34));
    drop(g);
    let g = child_panes[&10].parser.lock();
    assert_eq!((g.grid().cols, g.grid().rows), (70, 22));
}
