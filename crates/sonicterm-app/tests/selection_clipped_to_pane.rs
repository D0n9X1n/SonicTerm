//! Regression test for the split-pane selection bleed-through bug.
//!
//! Repro: in a split-right layout (two panes side by side, left pane
//! active), the user mouse-selects from col 0 in the LEFT pane and drags
//! beyond the pane's last visible column. Before the fix, the selection
//! quad's width was `(end_col - start_col + 1) * cell_w`, anchored at the
//! left pane's origin — so a drag that ran off the right edge produced a
//! quad that extended into the right pane's grid area, painting a gray
//! translucent rectangle across both panes' content.
//!
//! The fix clips selection quads to the active pane's rect after they're
//! computed. This test verifies the contract via the two pure helpers
//! exposed from `sonicterm_shared::render::core` (`selection_quad_rects` +
//! `clip_rect_to_pane`).

use sonicterm_shared::render::{clip_rect_to_pane, selection_quad_rects};
use sonicterm_ui::selection::Selection;

const CELL_W: f32 = 10.0;
const CELL_H: f32 = 20.0;

const LEFT_ORIGIN_X: f32 = 0.0;
const LEFT_ORIGIN_Y: f32 = 0.0;
const LEFT_COLS: u16 = 100;
const LEFT_ROWS: u16 = 40;

const LEFT_PANE_W: f32 = LEFT_COLS as f32 * CELL_W; // 1000.0
const LEFT_PANE_H: f32 = LEFT_ROWS as f32 * CELL_H; // 800.0

/// User selected from col 0 to col 200 on row 5 in a 100-col pane. The
/// unclipped quad would be 2010 px wide (col 0..=200 == 201 cells × 10 px),
/// extending 1010 px past the pane's right edge. After clipping, the quad
/// must end exactly at the pane's right edge.
#[test]
fn selection_past_right_edge_is_clipped_to_pane() {
    let sel = Selection { start: (5, 0), end: (5, 200) };
    let rects = selection_quad_rects(
        &sel,
        LEFT_ROWS,
        LEFT_COLS, // grid `cols` clamps internal `col_b` to 99, but
        LEFT_ORIGIN_X,
        LEFT_ORIGIN_Y,
        CELL_W,
        CELL_H,
    );
    assert_eq!(rects.len(), 1, "single-row selection should emit one quad");
    let pre = rects[0];
    // The pre-clip rect deliberately exceeds the pane width when the
    // selection's end column overshoots the grid — this is the bug class
    // the clip exists to fix. Verify it's measurably too wide so the test
    // would notice if a future caller silently pre-clamped.
    assert!(
        pre.0 + pre.2 > LEFT_PANE_W,
        "pre-clip width {} should exceed pane width {} for this overshoot \
         selection (otherwise the test isn't exercising the clip)",
        pre.2,
        LEFT_PANE_W,
    );
    let clipped = clip_rect_to_pane(pre, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H)
        .expect("rect should be visible");
    assert!(
        clipped.0 + clipped.2 <= LEFT_PANE_W + 0.001,
        "clipped right edge {} must not exceed pane right edge {}",
        clipped.0 + clipped.2,
        LEFT_PANE_W,
    );
}

/// A pathological case: a selection rect intentionally constructed past the
/// pane's right edge (simulating a future caller that bypasses the
/// per-row col clamp) must be entirely clipped away or shrunk to the pane.
#[test]
fn quad_entirely_past_right_edge_is_dropped() {
    let r = (LEFT_PANE_W + 50.0, 0.0, 200.0, CELL_H);
    let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H);
    assert!(clipped.is_none(), "rect entirely outside pane should be dropped");
}

/// A rect that straddles the pane's right edge should have its width
/// trimmed so `x + w == pane_x + pane_w`.
#[test]
fn quad_straddling_right_edge_is_trimmed() {
    let r = (LEFT_PANE_W - 30.0, 0.0, 200.0, CELL_H);
    let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H)
        .expect("rect partially inside pane should survive");
    assert!((clipped.0 + clipped.2 - LEFT_PANE_W).abs() < 0.001);
    assert!((clipped.2 - 30.0).abs() < 0.001);
}

/// A multi-row selection (spans across rows) must produce one quad per
/// covered row, and every quad must end at or before the pane's right edge.
#[test]
fn multirow_selection_every_quad_within_pane() {
    let sel = Selection { start: (3, 50), end: (7, 200) };
    let rects = selection_quad_rects(
        &sel,
        LEFT_ROWS,
        LEFT_COLS,
        LEFT_ORIGIN_X,
        LEFT_ORIGIN_Y,
        CELL_W,
        CELL_H,
    );
    assert_eq!(rects.len(), 5, "rows 3..=7 inclusive == 5 quads");
    for r in rects {
        let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H)
            .expect("selection rect should be visible inside the pane");
        assert!(
            clipped.0 + clipped.2 <= LEFT_PANE_W + 0.001,
            "row quad {:?} extends past pane right edge {}",
            clipped,
            LEFT_PANE_W,
        );
        assert!(clipped.0 >= LEFT_ORIGIN_X - 0.001);
        assert!(clipped.1 >= LEFT_ORIGIN_Y - 0.001);
        assert!(clipped.1 + clipped.3 <= LEFT_ORIGIN_Y + LEFT_PANE_H + 0.001);
    }
}

// ---- Haiku audit: 4 explicitly-named axis-aligned clip cases ----
//
// These four tests pin down the four canonical positions a selection rect can
// occupy relative to the pane boundary. They use `clip_rect_to_pane` directly
// so the contract is exercised at the helper's API surface, independent of
// `selection_quad_rects`.

/// Bottom-edge overshoot: a rect that extends below the pane's bottom must be
/// trimmed so `y + h == pane_y + pane_h`.
#[test]
fn selection_past_bottom_edge_clipped() {
    let r = (10.0, LEFT_PANE_H - 5.0, CELL_W, 100.0);
    let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H)
        .expect("rect straddling bottom edge should survive (trimmed)");
    assert!(
        (clipped.1 + clipped.3 - LEFT_PANE_H).abs() < 0.001,
        "clipped bottom {} should equal pane bottom {}",
        clipped.1 + clipped.3,
        LEFT_PANE_H,
    );
}

/// Left-edge underflow: a rect whose `x` is negative (left of the pane) must
/// have its `x` snapped to the pane's left edge and its width reduced accordingly.
#[test]
fn selection_before_left_edge_clipped() {
    let r = (-50.0, 20.0, 200.0, CELL_H);
    let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H)
        .expect("rect straddling left edge should survive (trimmed)");
    assert!(
        clipped.0 >= LEFT_ORIGIN_X - 0.001,
        "clipped x {} should not be left of pane origin {}",
        clipped.0,
        LEFT_ORIGIN_X,
    );
    assert!(
        (clipped.2 - 150.0).abs() < 0.001,
        "clipped width should be 200 - 50 == 150, got {}",
        clipped.2,
    );
}

/// A rect wholly inside the pane must pass through unchanged.
#[test]
fn selection_wholly_inside_pane_unchanged() {
    let r = (10.0, 20.0, 100.0, CELL_H);
    let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H)
        .expect("interior rect should survive clipping");
    assert!((clipped.0 - r.0).abs() < 0.001);
    assert!((clipped.1 - r.1).abs() < 0.001);
    assert!((clipped.2 - r.2).abs() < 0.001);
    assert!((clipped.3 - r.3).abs() < 0.001);
}

/// A rect entirely outside the pane (in any axis) must be dropped (returns None).
#[test]
fn selection_wholly_outside_pane_emits_nothing() {
    let r = (LEFT_PANE_W + 100.0, LEFT_PANE_H + 100.0, 50.0, 50.0);
    let clipped = clip_rect_to_pane(r, LEFT_ORIGIN_X, LEFT_ORIGIN_Y, LEFT_PANE_W, LEFT_PANE_H);
    assert!(clipped.is_none(), "rect entirely outside pane must produce no quad");
}

/// Empty selection emits zero quads (no degenerate 0-width quad pushed to GPU).
#[test]
fn empty_selection_emits_nothing() {
    let sel = Selection { start: (0, 0), end: (0, 0) };
    let rects = selection_quad_rects(
        &sel,
        LEFT_ROWS,
        LEFT_COLS,
        LEFT_ORIGIN_X,
        LEFT_ORIGIN_Y,
        CELL_W,
        CELL_H,
    );
    assert!(rects.is_empty());
}
