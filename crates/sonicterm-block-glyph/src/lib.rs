// `customglyph` carries its own vendor-attribution header (moved from
// the head of T5's lib.rs stub when T7 landed the 6036-LOC paste).
// The license text lives at
// crates/sonicterm-block-glyph/LICENSE-WEZTERM.
//
// T6 landed the substitution-boundary glue types (Bitmap, BgraPixel,
// Point/Rect/Size, and the `BitmapImage` trait) in `glue`. T7 lands
// the verbatim paste of wezterm-gui/src/customglyph.rs in
// `customglyph`, with `use crate::glue::{BitmapImage, Bitmap as Image,
// Point, Rect, Size, BgraPixel as SrgbaPixel}` for the substitution.
//
// `CellMetrics` is re-exported from `sonicterm-engine`; `DimensionContext`
// is re-exported from `sonicterm-cfg::dimension`. Those live outside this
// crate so non-customglyph callers can use them without paying for the
// glyph-geometry surface.

#![allow(dead_code)]

pub mod customglyph;
pub mod glue;

// Re-export the public surface T9 (flush_shape_run) consumes.
pub use customglyph::{block_sprite, BlockKey, SizedBlockKey};

/// Rasterize a WezTerm custom block glyph using SonicTerm's cell metrics.
///
/// Callers pass plain Sonic values (`underline_height` as raster px). This
/// keeps pixel-unit glue inside this crate instead of leaking it into the GPU
/// renderer.
pub fn block_sprite_with_cell_metrics(
    sized_key: SizedBlockKey,
    underline_height: isize,
    anti_alias: bool,
) -> anyhow::Result<glue::BlockRasterTile> {
    let metrics = glue::BlockCellMetrics {
        descender: glue::PixelLength::new(0.),
        descender_row: 0,
        descender_plus_two: 0,
        underline_height,
        strike_row: 0,
        cell_size: sized_key.size,
    };
    block_sprite(&metrics, sized_key, anti_alias)
}
