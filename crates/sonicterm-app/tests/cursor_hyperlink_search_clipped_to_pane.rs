//! Regression tests for the cursor / hyperlink-hover / search-match /
//! IME-preedit-underline bleed-through bug class (PR #270 follow-up).
//!
//! PR #270 routed selection quads through `clip_rect_to_pane`. The same
//! overflow class affects every other overlay anchored to the active
//! pane's origin: when the cursor sits in the last cell of a narrowed
//! pane, when a hyperlinked run reaches the pane edge, when a search
//! match spans past the pane edge, when the IME preedit underline
//! anchors near the pane's right edge — each can paint into a
//! neighbouring split pane unless clipped.
//!
//! These tests exercise the shared `clip_rect_to_pane` helper against
//! representative pre-clip rects that mimic what the render emit sites
//! in `crates/sonicterm-shared/src/render/core.rs` would otherwise push to
//! the GPU.

use sonicterm_gpu::core::clip_rect_to_pane;

const CELL_W: f32 = 10.0;
const CELL_H: f32 = 20.0;

// Narrow left pane in a vertical split: 50 cols × 30 rows.
const LEFT_X: f32 = 0.0;
const LEFT_Y: f32 = 0.0;
const LEFT_COLS: u16 = 50;
const LEFT_ROWS: u16 = 30;
const LEFT_W: f32 = LEFT_COLS as f32 * CELL_W; // 500.0
const LEFT_H: f32 = LEFT_ROWS as f32 * CELL_H; // 600.0

/// Cursor at the very last column of the narrow left pane — its
/// `cx + cell_w` lands exactly at the pane's right edge and must not
/// extend past it. A wide-char cursor at col `cols - 1` (which would
/// emit `cx + 2 * cell_w`) must be clipped to the pane's right edge.
#[test]
fn block_cursor_wide_char_at_last_col_clipped_to_pane_right_edge() {
    // Simulate a wide (CJK) cursor that occupies 2 cells at the last
    // grid column: x = (cols - 1) * cell_w, w = 2 * cell_w.
    let cx = f32::from(LEFT_COLS - 1) * CELL_W;
    let cy = 5.0 * CELL_H;
    let wide_w = 2.0 * CELL_W;
    let pre = (cx, cy, wide_w, CELL_H);
    assert!(
        pre.0 + pre.2 > LEFT_W,
        "pre-clip cursor rect must overshoot pane right edge for this test to mean anything"
    );
    let clipped =
        clip_rect_to_pane(pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H).expect("cursor should be visible");
    assert!(
        clipped.0 + clipped.2 <= LEFT_W + 0.001,
        "clipped cursor right edge {} must not exceed pane right edge {}",
        clipped.0 + clipped.2,
        LEFT_W,
    );
    // Visible width is exactly the remainder: pane_right - cx == cell_w.
    assert!((clipped.2 - CELL_W).abs() < 0.001);
}

/// Bar cursor (vertical 2-px strip) at col 0 of the left pane is fully
/// inside the pane — the clip must be a no-op pass-through. Guard
/// against an over-eager clip that would drop a valid in-bounds quad.
#[test]
fn bar_cursor_inside_pane_is_unchanged() {
    let pre = (12.0, 40.0, 2.0, CELL_H);
    let clipped = clip_rect_to_pane(pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H)
        .expect("in-bounds rect must survive");
    assert!((clipped.0 - pre.0).abs() < 0.001);
    assert!((clipped.1 - pre.1).abs() < 0.001);
    assert!((clipped.2 - pre.2).abs() < 0.001);
    assert!((clipped.3 - pre.3).abs() < 0.001);
}

/// Underline cursor at the bottom-right corner of a too-tall stale
/// grid (e.g. cursor row beyond the new pane row count after a resize
/// race) must be entirely dropped, not painted into the neighbour
/// below.
#[test]
fn underline_cursor_past_bottom_edge_is_dropped() {
    let cy = (LEFT_ROWS as f32) * CELL_H + 5.0;
    let pre = (50.0, cy + CELL_H - 2.0, CELL_W, 2.0);
    let clipped = clip_rect_to_pane(pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H);
    assert!(clipped.is_none(), "underline below pane bottom must be dropped, was {clipped:?}");
}

/// Hyperlink hover run that spans cols 45..=70 in the narrow left pane
/// (last visible col == 49). The pre-clip width is 26 cells; after
/// clipping it must end at the pane's right edge and the underline
/// thickness (a thinner sibling quad) must also clip.
#[test]
fn hyperlink_run_past_pane_right_edge_is_clipped() {
    let col_a = 45_u16;
    let col_b = 70_u16;
    let x = f32::from(col_a) * CELL_W;
    let y = 8.0 * CELL_H;
    let w = f32::from(col_b - col_a + 1) * CELL_W; // 260.0
    let tint_pre = (x, y, w, CELL_H);
    let underline_thickness = (CELL_H * 0.08).max(1.0);
    let underline_pre = (x, y + CELL_H - underline_thickness, w, underline_thickness);

    let tint = clip_rect_to_pane(tint_pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H)
        .expect("partial overlap should keep the visible portion");
    let underline = clip_rect_to_pane(underline_pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H)
        .expect("partial overlap should keep the visible portion");
    assert!(tint.0 + tint.2 <= LEFT_W + 0.001);
    assert!(underline.0 + underline.2 <= LEFT_W + 0.001);
    // Visible portion of the hyperlink: pane_right - x.
    let expected_visible = LEFT_W - x;
    assert!((tint.2 - expected_visible).abs() < 0.001);
}

/// Search-match highlight spanning past the pane edge — the match
/// rect's `w = (col_end - col_start) * cell_w` (note `col_end` is
/// exclusive in the search-match coordinate space). Verify the
/// clipped quad stays inside the pane.
#[test]
fn search_match_past_pane_right_edge_is_clipped() {
    let col_start = 40_u16;
    let col_end = 80_u16; // exclusive
    let x = f32::from(col_start) * CELL_W;
    let y = 4.0 * CELL_H;
    let w = f32::from(col_end - col_start) * CELL_W; // 400.0
    let pre = (x, y, w, CELL_H);
    assert!(pre.0 + pre.2 > LEFT_W);
    let clipped = clip_rect_to_pane(pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H)
        .expect("a match starting inside the pane must yield a visible clipped rect");
    assert!(clipped.0 + clipped.2 <= LEFT_W + 0.001);
    assert!((clipped.2 - (LEFT_W - x)).abs() < 0.001);
}

/// IME preedit underline anchored at the cursor position when the
/// cursor is at the rightmost cell of the narrow pane — the layout
/// underline width may exceed the remaining pane width. After
/// clipping it must not paint into the neighbour pane.
#[test]
fn ime_preedit_underline_near_right_edge_is_clipped() {
    // Cursor at col 48, preedit underline 60 px wide (≈ 6 cells of
    // composition glyphs). 2 cells fit, 4 overshoot the pane.
    let cursor_x = 48.0 * CELL_W;
    let cursor_y = 10.0 * CELL_H;
    let underline_y = cursor_y + CELL_H - 2.0;
    let pre = (cursor_x, underline_y, 60.0, 2.0);
    assert!(pre.0 + pre.2 > LEFT_W);
    let clipped = clip_rect_to_pane(pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H)
        .expect("underline starting inside the pane must yield a visible clipped rect");
    assert!(clipped.0 + clipped.2 <= LEFT_W + 0.001);
    let expected_visible = LEFT_W - cursor_x;
    assert!((clipped.2 - expected_visible).abs() < 0.001);
}

/// Hyperlink hover that begins entirely past the pane's right edge
/// (e.g. cursor over a hyperlink in a stale grid that's wider than the
/// freshly-resized pane) must be dropped, not painted into the
/// neighbour.
#[test]
fn hyperlink_entirely_past_pane_is_dropped() {
    let x = LEFT_W + 10.0;
    let pre = (x, 6.0 * CELL_H, 50.0, CELL_H);
    assert!(clip_rect_to_pane(pre, LEFT_X, LEFT_Y, LEFT_W, LEFT_H).is_none());
}
