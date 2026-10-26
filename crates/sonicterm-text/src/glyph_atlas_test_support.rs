//! Shared synthetic missing-glyph and subpixel rasters for atlas contracts.

use super::*;

pub(super) struct SubpixelRasterizer;

impl Rasterizer for SubpixelRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![10, 20, 30, 40],
            is_color: false,
            is_subpixel: true,
        })
    }
}

pub(super) struct MissingRasterizer;

impl Rasterizer for MissingRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        None
    }
}
