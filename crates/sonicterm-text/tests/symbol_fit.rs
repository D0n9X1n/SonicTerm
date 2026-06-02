//! Regression tests for the NerdFont / PUA cell-fit policy added in
//! #438. See `crates/sonicterm-text/src/swash_rasterizer.rs` for the
//! production helpers under test.

use sonicterm_text::swash_rasterizer::{apply_symbol_fit, classify_symbol, SymbolFit};

#[test]
fn classify_powerline_e0b0_to_e0bf_returns_powerline_cell_fill() {
    for cp in 0xE0B0u32..=0xE0BFu32 {
        let ch = char::from_u32(cp).expect("valid scalar");
        assert_eq!(
            classify_symbol(ch),
            SymbolFit::PowerlineCellFill,
            "U+{:04X} should be PowerlineCellFill",
            cp
        );
    }
}

#[test]
fn classify_nf_pua_returns_icon_cell_fit() {
    // BMP PUA, excluding the Powerline subset (0xE0B0..=0xE0BF).
    for cp in [0xE000u32, 0xE0AFu32, 0xE0C0u32, 0xF8FFu32, 0xF0001u32] {
        let ch = char::from_u32(cp).expect("valid scalar");
        assert_eq!(
            classify_symbol(ch),
            SymbolFit::IconCellFit,
            "U+{:04X} should be IconCellFit",
            cp
        );
    }
}

#[test]
fn classify_filled_arrows_return_icon_cell_fit() {
    for cp in [0x25B6u32, 0x25C0u32] {
        let ch = char::from_u32(cp).expect("valid scalar");
        assert_eq!(
            classify_symbol(ch),
            SymbolFit::IconCellFit,
            "U+{:04X} should be IconCellFit",
            cp
        );
    }
}

#[test]
fn classify_filled_arrows_full_range_returns_icon_cell_fit() {
    // PR #456 cycle 2: full U+25B6..=U+25C1 range (12 codepoints), not
    // just the original 4. Catches Haiku review finding that 25B8..25BF
    // were silently falling through to Natural.
    for cp in 0x25B6u32..=0x25C1u32 {
        let ch = char::from_u32(cp).expect("valid scalar");
        assert_eq!(
            classify_symbol(ch),
            SymbolFit::IconCellFit,
            "U+{:04X} should be IconCellFit",
            cp
        );
    }
}

#[test]
fn classify_text_returns_natural() {
    for ch in ['a', 'A', '中', '中', '🍅'] {
        assert_eq!(classify_symbol(ch), SymbolFit::Natural, "{:?} should be Natural", ch);
    }
}

#[test]
fn apply_symbol_fit_powerline_returns_exact_cell_rect() {
    // Natural glyph rect deliberately differs from cell rect.
    let natural = (1.5_f32, 2.5_f32, 8.0_f32, 9.0_f32);
    let out = apply_symbol_fit(natural, (10.0, 20.0), (12.0, 16.0), SymbolFit::PowerlineCellFill);
    assert_eq!(out, (10.0, 20.0, 12.0, 16.0));
}

#[test]
fn apply_symbol_fit_icon_fills_cell_width_and_target_band_height() {
    // #461 PR-B2b: IconCellFit no longer preserves aspect ratio for NF
    // PUA icons. It fills cell_w exactly and target_h * cell_h vertically,
    // matching Windows Terminal's builtinGlyphs behavior so icons fill
    // the cell as designed.
    let natural = (0.0_f32, 0.0_f32, 4.0_f32, 6.0_f32);
    let (x, y, w, h) = apply_symbol_fit(natural, (0.0, 0.0), (12.0, 16.0), SymbolFit::IconCellFit);
    // Width fills cell exactly.
    assert!((w - 12.0).abs() < 0.001, "expected w=12 (full cell), got {}", w);
    // Height lands in [0.85, 1.0] * 16.
    assert!((0.85 * 16.0..=16.0).contains(&h), "height {} out of [0.85*16, 16]", h);
    // Horizontally centered: x + w/2 == cell_w / 2.
    let cx_glyph = x + w * 0.5;
    assert!((cx_glyph - 6.0).abs() < 0.001, "glyph not horizontally centered: cx={}", cx_glyph);
    // Vertically centered: y + h/2 == cell_h / 2.
    let cy_glyph = y + h * 0.5;
    assert!((cy_glyph - 8.0).abs() < 0.001, "glyph not vertically centered: cy={}", cy_glyph);
}

#[test]
fn apply_symbol_fit_natural_returns_input_unchanged() {
    let natural = (1.5_f32, 2.5_f32, 8.0_f32, 9.0_f32);
    let out = apply_symbol_fit(natural, (10.0, 20.0), (12.0, 16.0), SymbolFit::Natural);
    assert_eq!(out, natural);
}
