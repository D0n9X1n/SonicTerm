//! Status-strip cap height, baseline and native text-size calculations.

use super::*;

/// The cap height the strip centers by, from the drawn capital's ink box when there is one:
/// `drawn` is its `(top, height)` relative to the baseline, so the result is twice its center's
/// height above the baseline. Ink that reaches below the baseline therefore lowers it instead of
/// shifting the capitals a whole pixel. Without a drawn capital it is the font's own measurement
/// `reported`, and without either, 0.7 × `text_px`.
pub fn resolve_cap_height(drawn: Option<(f32, f32)>, reported: Option<f32>, text_px: f32) -> f32 {
    match (drawn, reported) {
        (Some((top, height)), _) => -(2.0 * top + height),
        (None, Some(reported)) => reported,
        (None, None) => text_px * 0.7,
    }
}

/// The cap height for `text_px`: `kept` when it was measured at that size, otherwise `measure`'s
/// answer. Returns the `(text size bits, cap height)` the renderer keeps, so the same size is
/// measured once and a new size never reuses another size's value.
pub fn kept_cap_height(
    kept: Option<(u32, f32)>,
    text_px: f32,
    measure: impl FnOnce() -> f32,
) -> (u32, f32) {
    let bits = text_px.to_bits();
    match kept {
        Some((kept_bits, cap_height)) if kept_bits == bits => (bits, cap_height),
        _ => (bits, measure()),
    }
}

/// The strip font's cap height at `text_px`, in raster px: a capital `H` rasterized through the
/// strip on baseline 0 and measured by its ink box, so it describes exactly what the strip draws
/// on every rasterizer. The font's own measurement is used when no `H` is drawn.
pub fn strip_cap_height(
    font_stack: &sonicterm_engine::FontStack,
    rasterizer: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    text_px: f32,
) -> f32 {
    let screen = (text_px * 4.0, text_px * 4.0);
    let reference = chrome_text::layout_with_raster_variant(
        font_stack,
        rasterizer,
        atlas,
        "H",
        ChromeColor::WHITE,
        ChromeAttrs { bold: false, italic: false },
        text_px,
        text_px,
        (0.0, 0.0),
        screen,
        None,
        GlyphRasterVariant::StatusLine,
    );
    let drawn = reference.glyphs.first().map(|glyph| {
        let (_, ink_top, _, ink_height) =
            crate::cursor::ndc_rect_px(glyph.rect, screen.0, screen.1);
        (ink_top, ink_height)
    });
    let reported = font_stack.cap_height_raster_px().map(|cap_height| cap_height as f32);
    resolve_cap_height(drawn, reported, text_px)
}

/// The strip's text baseline: capitals centered in the band area outside the hairline, so the
/// baseline is that area's top plus half of its height and the cap height together.
pub fn strip_baseline(geometry: &StripGeometry) -> f32 {
    let thickness = geometry.scale_factor.max(0.5).round().max(1.0);
    let area_top = match geometry.position {
        StatusPosition::Top => geometry.top,
        StatusPosition::Bottom => geometry.top + thickness,
    };
    let area_h = (geometry.height - thickness).max(0.0);
    area_top + (area_h + geometry.cap_height) / 2.0
}

/// The default `[status_line] font_scale`, 0.8.
pub fn default_font_scale() -> f32 {
    StatusFontScale::DEFAULT.as_f32()
}

/// The strip's native font size in logical points for a `body_font_size` live terminal font at
/// `font_scale`, never under 1 pt. Times the display scale it is [`StripGeometry::text_px`], so
/// the strip's stack rasterizes at exactly the size it draws.
pub fn status_line_font_size(body_font_size: f32, font_scale: f32) -> f32 {
    (body_font_size * font_scale).max(MIN_TEXT_PT)
}

#[cfg(test)]
#[path = "status_strip_text_tests.rs"]
mod status_strip_text_tests;
