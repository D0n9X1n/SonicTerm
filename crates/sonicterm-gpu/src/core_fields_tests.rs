//! Renderer fields contracts, sharing one root fixture owner.

use super::*;

/// A literal bar glyph typed into the palette query is query text: the renderer
/// positions the caret from the field display offset and never strips or splits on it.
#[test]
fn palette_query_keeps_literal_bar_glyph() {
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
    let bar = '\u{258f}';
    assert!(!CORE_SRC.contains(&format!(".replace('{bar}'")), "renderer strips the bar glyph");
    assert!(!CORE_SRC.contains(&format!(".split('{bar}')")), "renderer splits on the bar glyph");
    assert!(CORE_SRC.contains("FieldText::palette("), "palette caret comes from the field display");
}

// Two-line command text shares equal outer margins instead of pushing details against the highlight.
#[test]
fn palette_detail_block_has_balanced_padding() {
    for scale in [1.0, 1.5, 2.0] {
        let height = (sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT
            + sonicterm_render_model::boundary::ui::overlays::PALETTE_DETAIL_HEIGHT)
            * scale;
        let label = 13.0 * scale;
        let detail = 12.0 * scale;
        let gap = 4.0 * scale;
        let (label_baseline, detail_top, detail_baseline) =
            palette_detail_positions(height, label, detail, gap);
        let top = label_baseline - label * 0.8;
        let bottom = height - (detail_baseline + detail * 0.2);
        assert!((top - bottom).abs() < 0.001);
        assert!((detail_top - (top + label) - gap).abs() < 0.001);
        assert!((bottom - 7.5 * 0.8 * scale).abs() < 0.001);
    }
}

// Oversized fonts retain a visible subtitle clip inside the fixed-height row.
#[test]
fn palette_detail_large_fonts_keep_a_visible_clip() {
    for scale in [1.0, 1.5, 2.0] {
        for size in [22.0, 34.0] {
            let height = 38.0 * scale;
            let detail_size = (size - 1.0) * scale;
            let (_, detail_top, baseline) =
                palette_detail_positions(height, size * scale, detail_size, 4.0 * scale);
            assert!(detail_top >= 0.0);
            assert!(height - detail_top >= detail_size - 0.001);
            assert!(baseline < height);
        }
    }
}

#[test]
fn palette_footer_is_one_logical_pixel_smaller_and_native_at_windows_scales() {
    // Contract: footer text stays one logical pixel smaller and rasterizes at native scale.
    assert_eq!(palette_footer_font_size(13.0), 12.0);
    assert_eq!(palette_footer_font_size(1.0), 1.0);

    for scale in [1.0_f32, 1.25, 1.5, 1.75] {
        let requested_font_size = palette_footer_font_size(13.0) * scale;
        let native_em = palette_footer_font_size(13.0) * scale;

        assert_eq!(requested_font_size, 12.0 * scale);
        assert_eq!(
            requested_font_size.to_bits(),
            native_em.to_bits(),
            "footer {requested_font_size}px rescales a {native_em}px atlas tile at {scale}x"
        );
    }
}

#[test]
fn palette_footer_uses_native_regular_natural_spacing_and_scaled_geometry() {
    // Contract: footer rendering uses its native regular stack, natural advances, and inset clip.
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
    let start = CORE_SRC
        .find("if let Some(footer_stack) = self.palette_footer_font_stack.as_ref()")
        .expect("footer must use its native stack");
    let end = CORE_SRC[start..]
        .find("// Inline IME preedit at the TERMINAL CURSOR")
        .map(|offset| start + offset)
        .expect("footer block must end before inline IME rendering");
    let footer = &CORE_SRC[start..end];

    assert!(footer.contains("let mut footer_rasterizer = footer_stack.clone();"));
    assert!(footer.contains("let footer_native_em = footer_font_size;"));
    assert!(footer.contains("chrome_text::layout_with_raster_variant("));
    assert!(footer.contains("GlyphRasterVariant::PaletteFooter"));
    assert!(footer.contains("ChromeAttrs::default(),"), "footer text remains regular");
    assert!(!footer.contains("tracking"), "footer must keep the font's natural advances");
    assert!(!footer.contains("bold: true"), "footer must not request a bold face");
    assert!(footer.contains("self.chrome_px(PALETTE_FOOTER_INSET_X)"));
    assert!(footer.contains("Some(ChromeClip {"));
}
