//! Regression test for the Haiku finding on PR #274.
//!
//! Pre-fix the clip bounds in `render::core` were derived from
//! `grid.cols * cell_w` / `grid.rows * cell_h`. When a pane has been
//! resized (window grew, split rebalanced) but the grid resync has not
//! yet propagated through the PTY, the derived extent is *smaller*
//! than the real pane rect. A cursor / hyperlink-hover / search-match
//! quad sitting inside the trailing edge of the *new* pane rect but
//! past the *old* grid extent was incorrectly clipped away — the very
//! bleed-through PR #274 set out to fix would silently re-appear during
//! the resize race window.
//!
//! This test pins the contract: `clip_rect_to_pane` must accept a quad
//! that is inside `pane.rect_px.{w,h}` even when the same quad would be
//! outside `grid.cols * cell_w`. The render sites in `core.rs` now feed
//! `rect_px` directly (see `PaneView.rect_w` / `rect_h`).
//!
//! The scenario:
//! - pane.rect_px.w = 800 (the real, just-resized pane width in px)
//! - grid.cols = 80, cell_w = 9   → grid extent = 720 (stale)
//! - cursor quad at x = 750, w = 9 (column 83 of the new wider pane)
//!
//! Under the old broken code the clip bound was 720 and the quad at
//! x=750 was dropped. Under the fix the clip bound is 800 and the quad
//! passes through unchanged.

use sonic_shared::render::clip_rect_to_pane;

const CELL_W: f32 = 9.0;
const CELL_H: f32 = 18.0;
const GRID_COLS: u16 = 80;
const GRID_ROWS: u16 = 24;

const PANE_X: f32 = 0.0;
const PANE_Y: f32 = 0.0;
// Real (post-resize) pane width: 800px — wider than the stale grid
// extent of 720px (80 * 9). Same idea on the vertical axis.
const PANE_W: f32 = 800.0;
const PANE_H: f32 = 500.0;

const STALE_GRID_W: f32 = GRID_COLS as f32 * CELL_W; // 720.0
const STALE_GRID_H: f32 = GRID_ROWS as f32 * CELL_H; // 432.0

#[test]
fn clip_uses_pane_rect_not_stale_grid_extent_horizontal() {
    // Sanity: the stale grid extent must really be smaller than the
    // new pane rect, otherwise the test would be vacuous.
    const _: () = assert!(STALE_GRID_W < PANE_W);

    // Cursor quad at x=750 — past the stale grid extent (720) but well
    // inside the real pane rect (800).
    let pre = (750.0, 2.0 * CELL_H, CELL_W, CELL_H);

    // Under the (broken) derived bound the rect would be dropped:
    let dropped_under_old =
        clip_rect_to_pane(pre, PANE_X, PANE_Y, STALE_GRID_W, STALE_GRID_H);
    assert!(
        dropped_under_old.is_none(),
        "sanity: old (grid-derived) bound must drop this quad — \
         otherwise the test does not pin the regression"
    );

    // Under the (fixed) pane.rect_px-derived bound the rect survives
    // unchanged.
    let kept = clip_rect_to_pane(pre, PANE_X, PANE_Y, PANE_W, PANE_H)
        .expect("quad inside pane.rect_px must survive");
    assert!((kept.0 - pre.0).abs() < 0.001);
    assert!((kept.1 - pre.1).abs() < 0.001);
    assert!((kept.2 - pre.2).abs() < 0.001);
    assert!((kept.3 - pre.3).abs() < 0.001);
}

#[test]
fn clip_uses_pane_rect_not_stale_grid_extent_vertical() {
    const _: () = assert!(STALE_GRID_H < PANE_H);

    // Hyperlink/search highlight one row past the stale grid bottom
    // but still inside the new pane rect.
    let pre = (5.0 * CELL_W, STALE_GRID_H + 4.0, CELL_W, CELL_H);

    let dropped_under_old =
        clip_rect_to_pane(pre, PANE_X, PANE_Y, STALE_GRID_W, STALE_GRID_H);
    assert!(
        dropped_under_old.is_none(),
        "sanity: old (grid-derived) bound must drop this quad"
    );

    let kept = clip_rect_to_pane(pre, PANE_X, PANE_Y, PANE_W, PANE_H)
        .expect("quad inside pane.rect_px must survive");
    assert!((kept.0 - pre.0).abs() < 0.001);
    assert!((kept.1 - pre.1).abs() < 0.001);
    assert!((kept.2 - pre.2).abs() < 0.001);
    assert!((kept.3 - pre.3).abs() < 0.001);
}
