//! Regression: SonicTerm #535 + #540.
//!
//! #535 — main-window intra-tabbar reorder used to call only
//! `TabBar::reorder(from, to)`, leaving `tab_states[i]` (which owns the
//! active pane id + the PaneTree of leaf-ids) glued to the old slot.
//! Visible symptom: after dragging tab-0 past tab-1 in the main window,
//! the bar showed `[T1, T0, T2]` but clicking T0 (now at slot 1) drew
//! T1's grid, because slot 1's `TabState` still referenced T1's
//! `PaneState`. Fix: route the main-window `ReorderTab` branch through
//! the same data-shuffling contract that `tab_transfer::reorder_within`
//! exposes (and that `child_window.rs:496-506` already mirrored
//! inline) — Tab + TabState move in lock-step; PaneState stays put in
//! the keyed map (panes are addressed by leaf-id, not by slot index).
//!
//! #540 — drag-to-end made the tab visually vanish. Root cause: the
//! drop-zone math can yield `to == tabs.len()` for a drop past the
//! last slot, and `TabBar::reorder` silently no-ops when `to` is out
//! of range. The bar still rendered the source slot empty (the drag
//! chip was occupying it), so to the user the tab "disappeared". Fix:
//! clamp `to` to `len - 1` in the main-window branch (matching
//! `reorder_within`'s semantics).
//!
//! Test strategy: drive the same data primitive the production
//! `ReorderTab` branch now uses (`tab_transfer::reorder_within`),
//! tagging each pane with a unique sentinel pane id so we can assert
//! "title-N's TabState still owns pane-N's PaneState" — i.e. the
//! content (and PTY handle, when one is attached in production) stayed
//! welded to the title across the reorder.

use parking_lot::Mutex;
use sonicterm_app::app::tab_transfer::{reorder_within, TabContainer, TransferOutcome};
use sonicterm_app::app::PaneState;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;
use std::sync::Arc;

fn make_pane() -> PaneState {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    PaneState::new(parser, None)
}

/// After reordering 3 tabs in the main window, each title's TabState
/// must still own the SAME pane id it had before — i.e. content and
/// PTY handle followed the title, not stayed behind at the old slot.
#[test]
fn main_window_reorder_keeps_title_bound_to_its_pane() {
    let mut c = TabContainer::new();
    // Each pane gets a unique sentinel id we capture at push time.
    let p0 = c.push_tab("T0", make_pane());
    let p1 = c.push_tab("T1", make_pane());
    let p2 = c.push_tab("T2", make_pane());

    // Sanity: index i → pane i.
    assert_eq!(c.tab_states[0].active_pane, p0);
    assert_eq!(c.tab_states[1].active_pane, p1);
    assert_eq!(c.tab_states[2].active_pane, p2);

    // Drag T0 to slot 2 — final arrangement [T1, T2, T0].
    let outcome = reorder_within(&mut c, 0, 2);
    assert_eq!(outcome, TransferOutcome::Moved { target_active: 2, source_empty: false });

    // Titles moved.
    assert_eq!(c.tabs.tabs()[0].title, "T1");
    assert_eq!(c.tabs.tabs()[1].title, "T2");
    assert_eq!(c.tabs.tabs()[2].title, "T0");

    // Pre-fix BUG: tab_states[2].active_pane was p2 (stale), so the
    // newly-titled "T0" tab drew T2's grid. Post-fix: tab_states move
    // with the titles, so slot 2 owns p0 again.
    assert_eq!(c.tab_states[0].active_pane, p1, "slot 0 must own T1's pane");
    assert_eq!(c.tab_states[1].active_pane, p2, "slot 1 must own T2's pane");
    assert_eq!(c.tab_states[2].active_pane, p0, "slot 2 must own T0's pane (the moved tab)");

    // Pane map untouched — panes are keyed by id, not by slot.
    assert_eq!(c.panes.len(), 3);
    assert!(c.panes.contains_key(&p0));
    assert!(c.panes.contains_key(&p1));
    assert!(c.panes.contains_key(&p2));
}

/// #540 — drag-to-end edge case. The drop-zone math can yield `to ==
/// tabs.len()` ( == 3 for a 3-tab bar), which `TabBar::reorder` would
/// silently drop. `reorder_within` clamps to `len - 1`; the inline
/// mirror in `window_event.rs` now does the same. Assert:
///   1. Order ends up [T1, T2, T0], NOT [T1, T2] (no data loss).
///   2. T0's pane id is still present in the pane map (no PTY leak).
///   3. T0's TabState still references its original sentinel pane id
///      (content follows title to the new last slot).
#[test]
fn main_window_reorder_drag_to_end_clamps_no_data_loss() {
    let mut c = TabContainer::new();
    let p0 = c.push_tab("T0", make_pane());
    let p1 = c.push_tab("T1", make_pane());
    let p2 = c.push_tab("T2", make_pane());

    // Simulate the production clamp: `to = to.min(len - 1)`. The
    // drop-zone gave us 3 (one past end), production now clamps to 2.
    let len = c.tabs.len();
    assert_eq!(len, 3);
    let raw_to = len; // 3 — one past last
    let to = raw_to.min(len - 1); // → 2

    let outcome = reorder_within(&mut c, 0, to);
    assert_eq!(outcome, TransferOutcome::Moved { target_active: 2, source_empty: false });

    // 1. No tab vanished — still 3 tabs, T0 is at the end.
    assert_eq!(c.tabs.len(), 3);
    assert_eq!(c.tabs.tabs()[0].title, "T1");
    assert_eq!(c.tabs.tabs()[1].title, "T2");
    assert_eq!(c.tabs.tabs()[2].title, "T0");

    // 2. T0's PTY/PaneState handle was not released.
    assert!(c.panes.contains_key(&p0), "T0's PaneState must not be dropped on drag-to-end");
    assert!(c.panes.contains_key(&p1));
    assert!(c.panes.contains_key(&p2));

    // 3. T0's sentinel content followed the title.
    assert_eq!(c.tab_states[2].active_pane, p0, "T0's content (sentinel) must be at the new end");
}
