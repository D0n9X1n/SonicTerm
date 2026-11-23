//! A pane's colors: the window theme under the colors its program set with OSC 4, 10-12 and 21.
//!
//! Every cell color path resolves through [`PaneColors`], so a pane's glyphs, inverse and dim
//! cells, backgrounds, decorations and cursor agree. A pane without program colors carries no
//! overrides and resolves exactly as the theme does, with no extra work.

use super::*;
use sonicterm_types::{ColorOverrides, DynamicColor, Rgb};
use std::hash::{Hash, Hasher};

/// The theme and, while its program set any, a pane's color overrides.
#[derive(Clone, Copy)]
pub struct PaneColors<'frame> {
    theme: &'frame Theme,
    overrides: Option<&'frame ColorOverrides>,
}

impl<'frame> From<&'frame Theme> for PaneColors<'frame> {
    fn from(theme: &'frame Theme) -> Self {
        Self { theme, overrides: None }
    }
}

/// An 8-bit override as an opaque chrome color.
fn chrome((red, green, blue): Rgb) -> ChromeColor {
    ChromeColor::rgb(red, green, blue)
}

impl<'frame> PaneColors<'frame> {
    /// The colors of a pane drawn with `theme` under `overrides`.
    #[must_use]
    pub fn new(theme: &'frame Theme, overrides: Option<&'frame ColorOverrides>) -> Self {
        Self { theme, overrides }
    }

    fn dynamic(&self, target: DynamicColor) -> Option<ChromeColor> {
        self.overrides.and_then(|overrides| overrides.dynamic(target)).map(chrome)
    }

    /// The pane's default foreground: the program's, else the theme's.
    #[must_use]
    pub fn foreground(&self) -> ChromeColor {
        self.dynamic(DynamicColor::Foreground)
            .unwrap_or_else(|| hex_to_chrome_color(self.theme.colors.foreground.0.as_str()))
    }

    /// The pane's default background: the program's, else the theme's.
    #[must_use]
    pub fn background(&self) -> ChromeColor {
        self.dynamic(DynamicColor::Background)
            .unwrap_or_else(|| hex_to_chrome_color(self.theme.colors.background.0.as_str()))
    }

    /// The foreground a default-colored glyph draws with: the program's, else `theme_default`,
    /// the renderer's precomputed theme foreground.
    #[must_use]
    pub fn default_foreground(&self, theme_default: ChromeColor) -> ChromeColor {
        self.dynamic(DynamicColor::Foreground).unwrap_or(theme_default)
    }

    /// Palette entry `index`: the program's, else the theme's.
    #[must_use]
    pub fn indexed(&self, index: u8) -> Option<ChromeColor> {
        self.overrides
            .and_then(|overrides| overrides.palette(index))
            .map(chrome)
            .or_else(|| indexed(index, self.theme))
    }

    /// The cursor fill: the program's OSC 12 color, else `theme_cursor`, both opaque premultiplied
    /// linear.
    #[must_use]
    pub fn cursor_rgba(&self, theme_cursor: [f32; 4]) -> [f32; 4] {
        self.dynamic(DynamicColor::Cursor).map_or(theme_cursor, chrome_color_to_linear_rgba)
    }

    /// The fill under the pane's default-background cells when the program set a background,
    /// premultiplied at the window's background `opacity` as the theme clear is.
    #[must_use]
    pub fn background_fill(&self, opacity: f32) -> Option<[f32; 4]> {
        let (red, green, blue) = self.overrides?.dynamic(DynamicColor::Background)?;
        let alpha = opacity.clamp(0.0, 1.0);
        let lut = crate::color::srgb_u8_to_linear_lut();
        Some([
            lut[usize::from(red)] * alpha,
            lut[usize::from(green)] * alpha,
            lut[usize::from(blue)] * alpha,
            alpha,
        ])
    }

    /// The row-cache style revision for this pane: `base` without overrides, else `base` mixed
    /// with the override generation, so a cached row never replays another color state.
    #[must_use]
    pub fn style_rev(&self, base: u64) -> u64 {
        let Some(overrides) = self.overrides else {
            // When: `overrides` is absent, the pane shares the theme rows every pane draws.
            return base;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (base, overrides.generation()).hash(&mut hasher);
        hasher.finish()
    }
}

/// The frame key's color identity: 0 while no pane has program colors, else a digest of each
/// overridden pane's id and override generation, so any color change repaints the whole window.
#[must_use]
pub fn pane_colors_identity<'frame>(
    panes: impl IntoIterator<Item = (u64, Option<&'frame ColorOverrides>)>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    for (pane_id, overrides) in panes {
        if let Some(overrides) = overrides {
            // A pane with program colors adds its id and generation to the digest.
            any = true;
            (pane_id, overrides.generation()).hash(&mut hasher);
        }
    }
    if any {
        hasher.finish()
    } else {
        // When: `any` stayed false, no pane has program colors and the identity is 0.
        0
    }
}

#[cfg(test)]
#[path = "core_pane_colors_tests.rs"]
mod core_pane_colors_tests;
