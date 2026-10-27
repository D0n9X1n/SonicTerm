//! Theme responsibilities of the renderer, preserving its single owner.

use super::*;

pub(super) fn cursor_color_from_theme(theme: &Theme) -> [f32; 4] {
    hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0)
}

pub(super) fn cursor_text_color_from_theme(theme: &Theme) -> [f32; 4] {
    hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0)
}

pub(super) fn active_cursor_color(base: [f32; 4]) -> [f32; 4] {
    base
}

pub(crate) fn glyph_flags(is_color: bool, is_subpixel: bool) -> [f32; 4] {
    [if is_color { 1.0 } else { 0.0 }, if is_subpixel { 1.0 } else { 0.0 }, 0.0, 0.0]
}

pub(super) fn effective_font_weight_scale(scale: f32) -> f32 {
    if scale.is_finite() && (0.5..=5.0).contains(&scale) {
        scale
    } else {
        // When: `scale` is NaN, infinite, or outside 0.5..=5.0. 1.0 is the
        // weight the font was drawn at, so a bad config still renders.
        1.0
    }
}

#[cfg(test)]
#[path = "core_theme_tests.rs"]
mod core_theme_tests;
