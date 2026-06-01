//! Foundation invariants for the per-row dirty bitset.
//!
//! These tests pin the public API surface (`Grid::mark_all_dirty`,
//! `Grid::dirty_rows`, `Grid::clear_dirty`) and verify that every
//! grid-level trigger that the upcoming RowCache will key off of
//! correctly marks the bitset.
//!
//! The triggers exercised here are the ones the grid itself owns —
//! resize, alt-screen swap, scroll, cursor move. The four
//! presentation-only triggers that live *outside* the grid
//! (theme/font/focus/selection) are exercised here via the explicit
//! public hook `mark_all_dirty()`, which is what `sonicterm-shared` calls
//! from its app loop when those events fire (see
//! `mark_all_panes_dirty` in `sonicterm-shared/src/app.rs`).
//!
//! Added in `perf/dirty-bitset-foundation` — see PR for the renderer
//! work it unblocks.

use sonicterm_core::grid::Grid;
use sonicterm_core::vt::Parser;

const COLS: u16 = 20;
const ROWS: u16 = 8;

fn fresh_grid() -> Grid {
    let mut g = Grid::new(COLS, ROWS);
    g.clear_dirty();
    assert_eq!(g.dirty_count(), 0, "post-clear baseline should be all-clean");
    g
}

fn dirty_row_vec(g: &Grid) -> Vec<usize> {
    g.dirty_rows().collect()
}

#[test]
fn fresh_grid_is_fully_dirty_before_first_clear() {
    let g = Grid::new(COLS, ROWS);
    assert_eq!(g.dirty_count(), ROWS as usize);
    assert_eq!(dirty_row_vec(&g), (0..ROWS as usize).collect::<Vec<_>>());
}

#[test]
fn mark_all_dirty_marks_every_row() {
    let mut g = fresh_grid();
    g.mark_all_dirty();
    assert_eq!(g.dirty_count(), ROWS as usize);
    assert_eq!(dirty_row_vec(&g), (0..ROWS as usize).collect::<Vec<_>>());
}

#[test]
fn dirty_rows_iterator_yields_ascending_indices() {
    let g = Grid::new(COLS, ROWS);
    let rows: Vec<usize> = g.dirty_rows().collect();
    assert!(rows.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn clear_dirty_then_mark_all_dirty_round_trip() {
    let mut g = fresh_grid();
    assert_eq!(g.dirty_count(), 0);
    g.mark_all_dirty();
    assert_eq!(g.dirty_count(), ROWS as usize);
    g.clear_dirty();
    assert_eq!(g.dirty_count(), 0);
    assert_eq!(dirty_row_vec(&g), Vec::<usize>::new());
}

// ---------- triggers the grid itself owns ----------

#[test]
fn trigger_resize_marks_all_dirty() {
    let mut g = fresh_grid();
    g.resize(COLS + 4, ROWS + 2);
    assert_eq!(
        g.dirty_count(),
        (ROWS + 2) as usize,
        "resize must mark every row of the new grid dirty"
    );
}

#[test]
fn trigger_alt_screen_enter_marks_all_dirty() {
    let mut g = fresh_grid();
    g.enter_alt_screen();
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn trigger_alt_screen_leave_marks_all_dirty() {
    let mut g = fresh_grid();
    g.enter_alt_screen();
    g.clear_dirty();
    g.leave_alt_screen();
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn trigger_scroll_up_marks_all_dirty() {
    let mut g = fresh_grid();
    g.scroll_up(1);
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn trigger_linefeed_at_bottom_marks_all_dirty() {
    // The "scroll down content" path in this codebase is reached via
    // a linefeed at the last row, which routes through `scroll_up`.
    let mut g = fresh_grid();
    g.goto(ROWS - 1, 0);
    g.clear_dirty();
    g.linefeed();
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn trigger_cursor_move_marks_current_row() {
    // A printed char on a fresh post-clear grid lands on row 0 and
    // therefore must mark at least row 0 dirty.
    let mut p = Parser::new(Grid::new(COLS, ROWS));
    p.grid_mut().clear_dirty();
    p.advance(b"x");
    let rows = dirty_row_vec(p.grid());
    assert!(rows.contains(&0), "cursor-move trigger must mark its row, got {rows:?}");
}

// ---------- presentation triggers (theme/font/focus/selection) ----------
//
// These four triggers live *outside* the grid (they mutate renderer
// state or app state, not cell content). The contract the
// `sonicterm-shared` app loop honours is: after each fires, call
// `mark_all_dirty()` on every pane's grid. The hook is exercised here
// directly to pin the API the app calls.

#[test]
fn presentation_hook_theme_change_marks_all_dirty() {
    let mut g = fresh_grid();
    g.mark_all_dirty(); // app.rs calls this after theme swap
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn presentation_hook_font_change_marks_all_dirty() {
    // Font change goes through grid resize in the app loop, which
    // already marks all dirty. Validate the resize path here too so
    // any future divergence is caught.
    let mut g = fresh_grid();
    g.resize(COLS + 1, ROWS); // any dim change resets the dirty bitset
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn presentation_hook_focus_change_marks_all_dirty() {
    let mut g = fresh_grid();
    g.mark_all_dirty(); // app.rs calls this on Focused(_) event
    assert_eq!(g.dirty_count(), ROWS as usize);
}

#[test]
fn presentation_hook_selection_change_marks_all_dirty() {
    let mut g = fresh_grid();
    g.mark_all_dirty(); // app.rs calls this on every selection mutation
    assert_eq!(g.dirty_count(), ROWS as usize);
}
