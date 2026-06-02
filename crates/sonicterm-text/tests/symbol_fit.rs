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

// ---------------------------------------------------------------------------
// #537: Box Drawing cell-stretch + Block Element geometry wiring.
// ---------------------------------------------------------------------------

#[test]
fn classify_box_drawing_u2500_to_u257f_returns_box_drawing_cell_fill() {
    for cp in 0x2500u32..=0x257Fu32 {
        let ch = char::from_u32(cp).expect("valid scalar");
        assert_eq!(
            classify_symbol(ch),
            SymbolFit::BoxDrawingCellFill,
            "U+{:04X} should be BoxDrawingCellFill",
            cp
        );
    }
}

#[test]
fn box_drawing_u2500_horizontal_stretches_to_cell_w() {
    // Simulate a font glyph whose natural advance is narrower than
    // cell_w (typical for box-drawing in Nerd Font patched faces):
    // natural rect is 6.0 wide inside a 10.0-wide cell.
    let cell_origin = (0.0_f32, 0.0_f32);
    let cell_size = (10.0_f32, 20.0_f32);
    let natural = (2.0_f32, 8.0_f32, 6.0_f32, 4.0_f32); // gx, gy, gw, gh
    let (gx, gy, gw, gh) =
        apply_symbol_fit(natural, cell_origin, cell_size, classify_symbol('\u{2500}'));
    // X must snap to cell left, width must equal cell_w.
    assert!((gx - cell_origin.0).abs() < 1e-4, "gx should snap to cell.x");
    assert!((gw - cell_size.0).abs() < 1e-4, "gw should equal cell_w, got {gw}");
    // Y / H preserved from natural placement.
    assert!((gy - natural.1).abs() < 1e-4, "gy should preserve natural placement");
    assert!((gh - natural.3).abs() < 1e-4, "gh should preserve natural placement");
}

#[test]
fn block_element_u2588_full_block_emits_full_cell_via_geometry() {
    // Full-block U+2588 routed through block_element_rect / primary_rect
    // (the renderer-side wiring) must produce a rect covering the entire
    // cell, regardless of what the font's natural glyph looks like.
    let cell_origin = (5.0_f32, 7.0_f32);
    let cell_size = (10.0_f32, 20.0_f32);
    let geom = sonicterm_text::block_element_geometry::block_element_rect(
        '\u{2588}',
        cell_origin,
        cell_size,
    )
    .expect("U+2588 must have geometry");
    let (gx, gy, gw, gh) = sonicterm_text::block_element_geometry::primary_rect(&geom);
    assert!((gx - cell_origin.0).abs() < 1e-4);
    assert!((gy - cell_origin.1).abs() < 1e-4);
    assert!((gw - cell_size.0).abs() < 1e-4, "U+2588 quad width must == cell_w");
    assert!((gh - cell_size.1).abs() < 1e-4, "U+2588 quad height must == cell_h");
}

#[test]
fn nf_icon_fit_decision_is_logged_via_tracing() {
    // Capture tracing output via a minimal custom Subscriber. We avoid
    // pulling in `tracing-subscriber` as a dev-dep — the workspace
    // already depends on `tracing`, and the harness contract is just
    // "the IconCellFit decision emits a log line on the expected target".
    use std::sync::{Arc, Mutex};
    use tracing::span;
    use tracing::subscriber::with_default;
    use tracing::{Event, Metadata, Subscriber};

    #[derive(Default)]
    struct Capture {
        events: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Subscriber for Capture {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _attrs: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
        fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let meta = event.metadata();
            if meta.target() == "sonic::render::glyph::nf_icon_fit" {
                self.events.lock().unwrap().push(meta.target());
            }
        }
        fn enter(&self, _span: &span::Id) {}
        fn exit(&self, _span: &span::Id) {}
    }

    let capture = Capture::default();
    let events = capture.events.clone();
    with_default(capture, || {
        // U+F0001 is in NF Plane-1 PUA → IconCellFit.
        sonicterm_text::swash_rasterizer::log_nf_icon_fit_decision(
            char::from_u32(0xF0001).unwrap(),
            Some(2),
            7.5,
            10.0,
        );
        // 'A' is Natural — log still fires (the decision is "no fit applied").
        sonicterm_text::swash_rasterizer::log_nf_icon_fit_decision('A', Some(0), 6.0, 10.0);
    });

    let lines = events.lock().unwrap().clone();
    assert!(
        lines.len() >= 2,
        "expected >=2 nf_icon_fit log events, got {}: {:?}",
        lines.len(),
        lines
    );
    assert!(
        lines.iter().all(|l| *l == "sonic::render::glyph::nf_icon_fit"),
        "all captured lines must come from nf_icon_fit target: {:?}",
        lines
    );
}
