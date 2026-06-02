//! Regression: SonicTerm #535 + #540.
//!
//! #535 — main-window intra-tabbar reorder used to call only
//! `TabBar::reorder(from, to)`, leaving `tab_states[i]` (which owns the
//! active pane id + the PaneTree of leaf-ids) glued to the old slot.
//! Visible symptom: after dragging tab-0 past tab-1 in the main window,
//! the bar showed `[T1, T0, T2]` but clicking T0 (now at slot 1) drew
//! T1's grid, because slot 1's `TabState` still referenced T1's
//! `PaneState`.
//!
//! #540 — drag-to-end made the tab visually vanish. Root cause: the
//! drop-zone math can yield `to == tabs.len()` for a drop past the
//! last slot, and `TabBar::reorder` silently no-ops when `to` is out
//! of range. The bar still rendered the source slot empty (the drag
//! chip was occupying it), so to the user the tab "disappeared". Fix:
//! clamp `to` to `len - 1` in the main-window branch.
//!
//! Test strategy: drive `WindowState::reorder_tab` — the SAME method
//! the production main-window `ReorderTab` branch in
//! `window_event.rs:1215` now calls (PR #543 Step-4 REVISE). Stashing
//! the production extraction makes these tests fail, which is the
//! whole point: they actually guard the changed path now, not the
//! already-correct `tab_transfer::reorder_within` helper.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use sonicterm_app::app::{PaneState, TabState, WindowRole, WindowState};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::ime::ImeState;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::Parser;

fn make_pane() -> PaneState {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    PaneState::new(parser, None)
}

fn make_ws() -> WindowState {
    WindowState {
        role: WindowRole::Terminal,
        window: None,
        renderer: None,
        tabs: TabBar::new(),
        tab_states: Vec::new(),
        panes: HashMap::new(),
        cursor_pos: (0.0, 0.0),
        mouse_down: false,
        selection: None,
        copy_mode: None,
        modifiers: Default::default(),
        last_render: Instant::now(),
        hover_link: false,
        pressed_tab: None,
        drag_session: None,
        drag_target: None,
        scale_factor: 1.0,
        ime: ImeState::new(),
        ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
        hovered_url: None,
        hidden: false,
        scrollbar_drag: None,
        scrollbar_vis: HashMap::new(),
        test_drag_chip_marker: None,
    }
}

/// Seed `n` tabs with unique sentinel pane ids and return the ids.
fn seed_tabs(ws: &mut WindowState, titles: &[&str]) -> Vec<u64> {
    let mut ids = Vec::with_capacity(titles.len());
    for (i, t) in titles.iter().enumerate() {
        let pane_id = (1000 + i) as u64;
        ws.tabs.push(Tab::new(*t));
        ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        ws.panes.insert(pane_id, make_pane());
        ids.push(pane_id);
    }
    ids
}

/// #535 — after the production `ReorderTab` branch fires
/// `WindowState::reorder_tab(0, 2)`, slot N must own the pane id of
/// the title now at slot N. Pre-fix the branch only moved `tabs`, so
/// `tab_states[i].active_pane` stayed at the old slot's id and this
/// assertion fails for slots 0 and 2.
#[test]
fn main_window_reorder_keeps_title_bound_to_its_pane() {
    let mut ws = make_ws();
    let ids = seed_tabs(&mut ws, &["T0", "T1", "T2"]);
    let (p0, p1, p2) = (ids[0], ids[1], ids[2]);

    // Drive the same method production drives.
    let mutated = ws.reorder_tab(0, 2);
    assert!(mutated, "reorder must report mutation when from != to");

    // Titles moved.
    assert_eq!(ws.tabs.tabs()[0].title, "T1");
    assert_eq!(ws.tabs.tabs()[1].title, "T2");
    assert_eq!(ws.tabs.tabs()[2].title, "T0");

    // Pre-fix: tab_states[2].active_pane was p2 (stale), so the
    // newly-titled "T0" tab drew T2's grid. Post-fix: tab_states
    // moved with the titles, so slot 2 owns p0 again.
    assert_eq!(ws.tab_states[0].active_pane, p1, "slot 0 must own T1's pane");
    assert_eq!(ws.tab_states[1].active_pane, p2, "slot 1 must own T2's pane");
    assert_eq!(ws.tab_states[2].active_pane, p0, "slot 2 must own T0's pane (the moved tab)");

    // Pane map untouched — panes are keyed by id, not by slot.
    assert_eq!(ws.panes.len(), 3);
    assert!(ws.panes.contains_key(&p0));
    assert!(ws.panes.contains_key(&p1));
    assert!(ws.panes.contains_key(&p2));
}

/// #540 — drop-zone math yields `to == tabs.len()` (3 for a 3-tab
/// bar). Pre-fix the production branch passed that straight to
/// `TabBar::reorder` which silently no-ops, then drew the source slot
/// empty (drag-chip overlay) — visually "the tab vanished". Post-fix
/// `WindowState::reorder_tab` clamps `to` to `len - 1`. Assert no data
/// loss + sentinel pane followed the title to the new last slot.
#[test]
fn main_window_reorder_drag_to_end_clamps_no_data_loss() {
    let mut ws = make_ws();
    let ids = seed_tabs(&mut ws, &["T0", "T1", "T2"]);
    let (p0, p1, p2) = (ids[0], ids[1], ids[2]);

    let len = ws.tabs.len();
    assert_eq!(len, 3);
    // Pass `to = len` — exactly what `compute_action` hands the
    // production branch on drag-past-last. The extracted method must
    // clamp internally.
    let mutated = ws.reorder_tab(0, len);
    assert!(mutated, "drag-to-end must still mutate after clamping");

    // 1. No tab vanished — still 3 tabs, T0 is at the end.
    assert_eq!(ws.tabs.len(), 3);
    assert_eq!(ws.tabs.tabs()[0].title, "T1");
    assert_eq!(ws.tabs.tabs()[1].title, "T2");
    assert_eq!(ws.tabs.tabs()[2].title, "T0");

    // 2. T0's PTY/PaneState handle was not released.
    assert!(ws.panes.contains_key(&p0), "T0's PaneState must not be dropped on drag-to-end");
    assert!(ws.panes.contains_key(&p1));
    assert!(ws.panes.contains_key(&p2));

    // 3. T0's sentinel content followed the title.
    assert_eq!(ws.tab_states[2].active_pane, p0, "T0's content (sentinel) must be at new end");
}

/// Out-of-range `from` is a no-op (defensive — production never sends
/// this, but the contract should be explicit since the extracted
/// method is now part of the public surface).
#[test]
fn main_window_reorder_out_of_range_from_is_noop() {
    let mut ws = make_ws();
    seed_tabs(&mut ws, &["T0", "T1"]);
    assert!(!ws.reorder_tab(5, 0));
    assert_eq!(ws.tabs.tabs()[0].title, "T0");
    assert_eq!(ws.tabs.tabs()[1].title, "T1");
}
