//! Settings responsibilities of the renderer, preserving its single owner.

use super::*;

/// Renderer initialization settings derived from config.
#[derive(Debug, Clone, Copy)]
pub struct RendererSettings<'a> {
    /// Font family to use for terminal text.
    pub font_family: &'a str,
    /// Packaged directories searched before platform-native font discovery.
    pub font_dirs: &'a [PathBuf],
    /// Font size in points.
    pub font_size: f32,
    /// Line-height multiplier.
    pub line_height_mult: f32,
    /// Regular-text coverage scale.
    pub font_weight_scale: f32,
    /// Requested LCD subpixel coverage order.
    pub subpixel_aa: SubpixelAaMode,
    /// Window padding in logical pixels: left, right, top, bottom.
    pub padding: [f32; 4],
    /// Surface/backdrop settings.
    pub appearance: SurfaceAppearance,
    /// Stable renderer role used by memory and timing diagnostics.
    pub role: &'static str,
    /// Size the renderer's glyph atlas starts at before it grows on demand.
    pub glyph_atlas_start: GlyphAtlasStart,
}

/// Which start size a renderer's glyph atlas takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlyphAtlasStart {
    /// The measured start size for the window's scale factor.
    #[default]
    Normal,
    /// The 256-pixel floor, for warm-pool renderers that may never draw.
    Minimum,
}

/// Square start dimension of a glyph atlas for `scale_factor` and `start`.
///
/// `Normal` takes the scale-1 start up to 1.5 and the scale-2 start above it; `Minimum` is the
/// floor. A later scale change resets the atlas in place at its current size and never shrinks it.
#[must_use]
pub fn start_dim(scale_factor: f32, start: GlyphAtlasStart) -> u32 {
    let GlyphAtlasStart::Normal = start else {
        // When: start is Minimum the renderer may never draw, so its atlas takes the 256 floor.
        return sonicterm_text::glyph_atlas::MIN_ATLAS_DIM;
    };
    if scale_factor <= 1.5 {
        sonicterm_text::glyph_atlas::START_ATLAS_DIM_1X
    } else {
        // When: scale_factor is above 1.5 the window rasterizes at scale 2, so it takes that start.
        sonicterm_text::glyph_atlas::START_ATLAS_DIM_2X
    }
}
