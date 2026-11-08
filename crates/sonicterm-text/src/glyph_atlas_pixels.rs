//! Tile insertion: copy a reserved tile's pixels into the atlas, from an owned [`RasterTile`] or
//! from a caller's borrowed BGRA pixels, through one row copy.

use super::*;

/// A tile's metrics and a borrow of its pixels, laid out as [`RasterTile::coverage`] describes.
struct TilePixels<'pixels> {
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
    advance: f32,
    coverage: &'pixels [u8],
    is_color: bool,
    is_subpixel: bool,
}

impl GlyphAtlas {
    /// Look up or insert a premultiplied BGRA8 color tile of `width` x `height` read straight
    /// from `pixels`, without eviction, so the caller's buffer is copied once, into the atlas.
    ///
    /// Behaves as [`Self::get_or_insert_lazy_without_eviction`] with a color tile built from
    /// `pixels`, and also refuses, before any atlas rectangle is reserved, a `pixels` whose length
    /// is not `width * height * 4`.
    pub fn get_or_insert_color_pixels_without_eviction(
        &mut self,
        key: GlyphKey,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Option<GlyphInfo> {
        if let Some(entry) = self.map.get_mut(&key) {
            // When: key is already in map the tile is resident, so cached info is returned
            // and last_used_frame is refreshed to keep it out of the eviction quartile.
            entry.last_used_frame = self.current_frame;
            self.hits += 1;
            return Some(entry.info);
        }
        self.misses += 1;
        if !self.make_entry_room(false) {
            // When: make_entry_room reports no capacity the entry count is at its ceiling
            // and eviction is barred here, so admission is refused before any pixel work.
            return None;
        }
        let (limit_w, limit_h) = self.placement_limit();
        if width == 0 || height == 0 || width > limit_w || height > limit_h {
            // When: width or height is zero or beyond placement_limit the request can never
            // be placed, so it is refused before any slot is reserved.
            return None;
        }
        let expected_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|count| count.checked_mul(BYTES_PER_PIXEL as usize));
        if expected_len != Some(pixels.len()) {
            // When: pixels.len() is not width * height * 4 the rows would be read past or short
            // of the buffer, so the tile is refused before a slot is reserved.
            return None;
        }
        let allocation = self.alloc_rect_growing(width, height)?;
        let tile = TilePixels {
            width,
            height,
            offset_x: 0,
            offset_y: 0,
            advance: width as f32,
            coverage: pixels,
            is_color: true,
            is_subpixel: false,
        };
        Some(self.insert_pixels(key, &tile, allocation))
    }

    /// Insert an owned tile into its reserved slot.
    pub(super) fn insert_tile(
        &mut self,
        key: GlyphKey,
        tile: RasterTile,
        allocation: AtlasAllocation,
    ) -> GlyphInfo {
        let view = TilePixels {
            width: tile.width,
            height: tile.height,
            offset_x: tile.offset_x,
            offset_y: tile.offset_y,
            advance: tile.advance,
            coverage: &tile.coverage,
            is_color: tile.is_color,
            is_subpixel: tile.is_subpixel,
        };
        self.insert_pixels(key, &view, allocation)
    }

    /// Copy `tile` into `allocation`, record its dirty rect and cache its info under `key`.
    fn insert_pixels(
        &mut self,
        key: GlyphKey,
        tile: &TilePixels<'_>,
        allocation: AtlasAllocation,
    ) -> GlyphInfo {
        let AtlasAllocation { x, y, slot_w, slot_h, reused } = allocation;
        // Blit rows into the CPU BGRA buffer. Monochrome tiles arrive
        // as `width*height` alpha bytes — replicate each into the four
        // BGRA channels so the shader can sample a single uniform
        // texture format. Color tiles arrive as `width*height*4` BGRA
        // bytes already premultiplied in sRGB-encoded space; subpixel text tiles arrive as
        // `width*height*4` BGRA coverage. Copy both through verbatim.
        let bpp = BYTES_PER_PIXEL as usize;
        for row in 0..tile.height {
            let dst_off = ((y + row) * self.width + x) as usize * bpp;
            if tile.is_color || tile.is_subpixel {
                let src_off = (row * tile.width) as usize * bpp;
                let len = tile.width as usize * bpp;
                self.pixels[dst_off..dst_off + len]
                    .copy_from_slice(&tile.coverage[src_off..src_off + len]);
            } else {
                // When: tile is neither is_color nor is_subpixel it arrives as one alpha
                // byte per pixel, which is replicated so one sampler serves both formats.
                let src_off = (row * tile.width) as usize;
                for col in 0..tile.width as usize {
                    let a = tile.coverage[src_off + col];
                    let p = dst_off + col * bpp;
                    // Premultiplied "white" alpha: BGRA = (a, a, a, a).
                    // The shader multiplies by the per-instance color
                    // for monochrome glyphs, so storing white here lets
                    // a single texture sample serve both flavors.
                    self.pixels[p] = a;
                    self.pixels[p + 1] = a;
                    self.pixels[p + 2] = a;
                    self.pixels[p + 3] = a;
                }
            }
        }
        let kind = if tile.is_color { AtlasPixelKind::Color } else { AtlasPixelKind::Coverage };
        let dirty = DirtyRect { x, y, w: tile.width, h: tile.height, kind };
        if reused {
            // A reclaimed slot can overlap a pending write from its prior owner; retaining
            // that stale record would let its pixel kind reinterpret the replacement bytes.
            self.dirty.retain(|pending| !dirty_rects_overlap(*pending, dirty));
        }
        self.dirty.push(dirty);
        let info = GlyphInfo {
            uv: [
                x as f32 / self.width as f32,
                y as f32 / self.height as f32,
                (x + tile.width) as f32 / self.width as f32,
                (y + tile.height) as f32 / self.height as f32,
            ],
            px_size: [tile.width, tile.height],
            px_offset: [tile.offset_x, tile.offset_y],
            advance: tile.advance,
            is_color: tile.is_color,
            is_subpixel: tile.is_subpixel,
            missing: false,
            oversize: false,
        };
        self.map.insert(
            key,
            AtlasEntry {
                info,
                last_used_frame: self.current_frame,
                rect: Some((x, y, slot_w, slot_h)),
            },
        );
        info
    }
}

#[cfg(test)]
#[path = "glyph_atlas_pixels_tests.rs"]
mod glyph_atlas_pixels_tests;
