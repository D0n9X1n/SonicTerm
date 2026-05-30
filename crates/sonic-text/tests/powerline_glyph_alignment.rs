//! Regression tests for the Powerline PUA cell-rect anchor (fix for
//! the "arrows misaligned between rows + jagged diagonals" user report).
//!
//! Before this fix the renderer composed every glyph's screen rect as
//! `(cx + px_offset, cy + baseline + px_offset[1], px_size, px_size)`.
//! For the Powerline range U+E0B0..=U+E0BF the `placement.top` / size
//! pair varies *between* glyphs (U+E0B0 is full-bleed, U+E0B1 stroke-
//! only), so adjacent arrows on stacked rows sat at different y inside
//! their cells — the user saw "row N high, row N+1 low or missing".
//!
//! The fix routes Powerline codepoints through
//! [`anchor_powerline_rect`], which forces the rect to exactly the cell
//! box. These tests pin that behaviour for narrow + wide cells, across
//! the entire range, and across two stacked rows.

use sonic_text::swash_rasterizer::{
    anchor_powerline_rect, is_powerline_char, POWERLINE_PUA_FIRST, POWERLINE_PUA_LAST,
};

#[test]
fn powerline_range_classified_inclusively() {
    // Spot-check the canonical separators every powerline theme uses.
    for cp in [0xE0B0u32, 0xE0B1, 0xE0B2, 0xE0B3, 0xE0BC, 0xE0BF] {
        let ch = char::from_u32(cp).unwrap();
        assert!(is_powerline_char(ch), "U+{cp:04X} must be classified as powerline");
    }
    // Range bounds.
    assert!(is_powerline_char(char::from_u32(POWERLINE_PUA_FIRST).unwrap()));
    assert!(is_powerline_char(char::from_u32(POWERLINE_PUA_LAST).unwrap()));

    // Non-powerline characters must NOT be reanchored — otherwise CJK
    // and emoji would lose their baseline-relative placement.
    for ch in ['A', '中', '🎉', '─', '\u{E0A0}', '\u{E0C0}', '\u{F031}'] {
        assert!(!is_powerline_char(ch), "{ch:?} must not be classified as powerline");
    }
}

#[test]
fn anchor_overrides_baseline_offset_for_powerline_glyph() {
    // Simulate a glyph whose natural raster sits well above the cell
    // top (negative offset → would render half outside the cell).
    let (cx, cy, cell_w, cell_h) = (100.0, 200.0, 8.0, 16.0);
    let natural = (cx + 0.5, cy - 4.0, cell_w + 1.0, cell_h - 6.0);

    // Powerline arrow: snaps to cell rect exactly.
    let (gx, gy, gw, gh) = anchor_powerline_rect('\u{E0B0}', cx, cy, cell_w, cell_h, natural);
    assert_eq!((gx, gy, gw, gh), (cx, cy, cell_w, cell_h));

    // Non-powerline (CJK): natural rect passes through unchanged so the
    // baseline trick keeps working for text.
    let (gx, gy, gw, gh) = anchor_powerline_rect('中', cx, cy, cell_w, cell_h, natural);
    assert_eq!((gx, gy, gw, gh), natural);
}

#[test]
fn anchored_rect_is_consistent_across_two_stacked_rows() {
    // The user-visible bug: row N's arrow sat high, row N+1's arrow sat
    // low. Verify both rows produce a rect that starts at `cy + row*ch`
    // exactly, with no drift introduced by per-glyph placement.top.
    let (cx, top_inset, cell_w, cell_h) = (50.0, 4.0, 9.0, 18.0);

    // Different `natural` for each row mimicking U+E0B0 vs U+E0B1
    // having different placement.top — the regression cause.
    let natural_row0 = (cx + 0.3, top_inset + 0.0 * cell_h - 5.0, cell_w + 2.0, cell_h - 8.0);
    let natural_row1 = (cx + 0.0, top_inset + 1.0 * cell_h + 7.0, cell_w - 1.0, cell_h + 3.0);

    let row0 = anchor_powerline_rect(
        '\u{E0B0}',
        cx,
        top_inset + 0.0 * cell_h,
        cell_w,
        cell_h,
        natural_row0,
    );
    let row1 = anchor_powerline_rect(
        '\u{E0B1}',
        cx,
        top_inset + 1.0 * cell_h,
        cell_w,
        cell_h,
        natural_row1,
    );

    // Same x, same width/height — no horizontal drift.
    assert_eq!(row0.0, row1.0);
    assert_eq!(row0.2, row1.2);
    assert_eq!(row0.3, row1.3);
    // y exactly increases by one cell_h. No baseline drift.
    let dy = row1.1 - row0.1;
    assert!(
        (dy - cell_h).abs() < f32::EPSILON,
        "rows must advance by exactly cell_h; got dy={dy}, cell_h={cell_h}"
    );
    // Row 0 top is the cell top (no baseline bias).
    assert!((row0.1 - (top_inset + 0.0 * cell_h)).abs() < f32::EPSILON);
}

#[test]
fn anchor_works_for_wide_cells() {
    // WIDE flag callers pass cell_w * 2.0 as the effective width — the
    // anchor must honour that and not clamp back to one cell.
    let (cx, cy, narrow_w, cell_h) = (0.0, 0.0, 10.0, 20.0);
    let wide = narrow_w * 2.0;
    let natural = (cx, cy, narrow_w, cell_h);
    let (_, _, gw, _) = anchor_powerline_rect('\u{E0B0}', cx, cy, wide, cell_h, natural);
    assert_eq!(gw, wide);
}
