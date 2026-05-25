//! GPU glyph atlas: a single R8 texture that stores rasterized glyph
//! coverage masks, keyed by [`sonic_core::glyph_key::GlyphKey`].
//!
//! The atlas is the centerpiece of the B3 GPU text path. Once warm, a
//! cell renders by:
//!   1. computing its `GlyphKey`            (≈1 ns)
//!   2. looking up the `GlyphInfo`          (≈30 ns, HashMap hit)
//!   3. emitting an instance into the text pipeline's vertex buffer
//!
//! Only step 3 grows with frame complexity, and the GPU does it in
//! parallel. Compare to glyphon-on-rich-text, which re-shapes the full
//! screen of text and re-uploads tiles into its own atlas on every
//! cache miss; the wins are biggest during heavy scrollback bursts
//! where the *same* ASCII glyphs reappear thousands of times.
//!
//! ## Design choices, in one line each
//!
//! - Single 2048×2048 R8Unorm texture: 4 MiB, fits ~16k 16×16 tiles, way
//!   more than any real terminal session needs. v0.7 grows by allocating
//!   a new texture; v0.7 panics on overflow (loud failure beats silent
//!   corruption).
//! - Shelf packer: simplest layout that still gets ≥80 % occupancy on
//!   monospace fonts, where all tiles are roughly the same height.
//!   Atlas growth is O(1) amortized.
//! - `Rasterizer` trait: the atlas does NOT depend on swash directly.
//!   Production wires a swash-backed rasterizer; tests wire a synthetic
//!   one (NxN ramp pattern) so atlas behavior is verifiable without a
//!   font on disk.
//! - Color lives on the instance, not on the tile. See `GlyphKey` docs.

use std::collections::HashMap;

use sonic_core::glyph_key::GlyphKey;

/// Default atlas dimensions. R8Unorm, so 4 MiB on the GPU.
pub const ATLAS_DIM: u32 = 2048;

/// Information about a glyph the renderer needs each frame: where its
/// tile lives in the atlas (in normalized 0..1 UVs) and how far the
/// pen should advance after drawing it (in pixels at the atlas's
/// design size).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphInfo {
    /// `[u_min, v_min, u_max, v_max]` — normalized to atlas dimensions
    /// so callers don't need to know the texture size.
    pub uv: [f32; 4],
    /// Pixel size of the glyph tile (width, height).
    pub px_size: [u32; 2],
    /// Tile origin offset in pixels relative to the cell box's top-left
    /// (positive = right/down). Cells are taller than the visible
    /// pixels of, say, an 'a', so the renderer needs this to position
    /// the quad correctly.
    pub px_offset: [i32; 2],
    /// Horizontal pen advance in pixels. The renderer uses cell-grid
    /// positioning so this is informational for proportional fallback,
    /// not for the main grid path.
    pub advance: f32,
}

/// A single rasterized glyph: alpha coverage mask + the metrics needed
/// to build the `GlyphInfo`.
#[derive(Debug, Clone)]
pub struct RasterTile {
    pub width: u32,
    pub height: u32,
    /// Top-left offset of the visible pixels relative to the cell box.
    pub offset_x: i32,
    pub offset_y: i32,
    pub advance: f32,
    /// `width * height` bytes of 8-bit coverage, row-major.
    pub coverage: Vec<u8>,
}

impl RasterTile {
    /// True when the tile carries any pixels worth uploading. A
    /// zero-sized tile (e.g. a space or a control character) still
    /// counts as a hit in the atlas — its `GlyphInfo` reports a UV
    /// rect of zero area and the renderer skips the draw instance.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.coverage.is_empty()
    }
}

/// Anything that can turn a `GlyphKey` into a `RasterTile`.
///
/// Implementors are typically font-backed (swash in production, a
/// deterministic synthetic rasterizer in tests). Returning `None` is a
/// fatal-for-this-glyph signal — the atlas falls back to a blank tile
/// and tracks the miss so callers can log it. Implementors must NOT
/// panic on unknown keys.
pub trait Rasterizer {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile>;
}

/// Shelf packer: simple left-to-right strip allocator that opens a new
/// strip when the current one overflows in width.
///
/// Trade-off vs a guillotine/skyline packer: shelf wastes more vertical
/// space when tile heights vary a lot, but monospace glyphs are nearly
/// uniform in height so the waste is small (<20% in practice). The
/// simplicity makes the code easier to audit; we can swap algorithms
/// later without touching the public API.
#[derive(Debug)]
#[doc(hidden)]
pub struct ShelfPacker {
    width: u32,
    height: u32,
    /// X cursor on the current shelf.
    cursor_x: u32,
    /// Y top of the current shelf.
    shelf_y: u32,
    /// Height of the current shelf — set by the first tile placed on it.
    shelf_h: u32,
}

impl ShelfPacker {
    #[doc(hidden)]
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, cursor_x: 0, shelf_y: 0, shelf_h: 0 }
    }

    /// Allocate a `(w, h)` rect on the atlas. Returns `(x, y)` of the
    /// top-left or `None` if the atlas is full. On failure the packer
    /// state is unchanged so subsequent smaller allocations that DO fit
    /// the current shelf still succeed.
    #[doc(hidden)]
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.width || h > self.height {
            return None; // tile bigger than entire atlas
        }
        // Compute a candidate (cursor_x, shelf_y, shelf_h) without
        // mutating self until the candidate is proven valid.
        let mut cand_cursor_x = self.cursor_x;
        let mut cand_shelf_y = self.shelf_y;
        let mut cand_shelf_h = self.shelf_h;

        // First-tile-ever — bootstrap the shelf height.
        if cand_shelf_h == 0 {
            cand_shelf_h = h;
        }
        // Doesn't fit horizontally → advance to a fresh shelf.
        if cand_cursor_x + w > self.width {
            cand_shelf_y = cand_shelf_y.saturating_add(cand_shelf_h);
            cand_cursor_x = 0;
            cand_shelf_h = h;
        }
        // Grow shelf height if this tile is taller than what's there.
        if h > cand_shelf_h {
            cand_shelf_h = h;
        }
        // Bounds check BEFORE committing — if vertical capacity is
        // exhausted, return None and leave packer untouched.
        if cand_shelf_y + cand_shelf_h > self.height {
            return None;
        }
        // Commit.
        let x = cand_cursor_x;
        let y = cand_shelf_y;
        self.cursor_x = cand_cursor_x + w;
        self.shelf_y = cand_shelf_y;
        self.shelf_h = cand_shelf_h;
        Some((x, y))
    }
}

/// CPU-side glyph atlas. Holds the alpha texture in a `Vec<u8>` and
/// the key→info map. A separate type, `AtlasUpload`, wraps this with a
/// wgpu `Texture` + `BindGroup` for the actual GPU path.
///
/// This split lets the bench harness, integration tests, and the
/// production renderer share the same packing + lookup logic without
/// pulling a GPU dependency into the bench.
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    map: HashMap<GlyphKey, GlyphInfo>,
    packer: ShelfPacker,
    /// Each `get_or_insert` records which rect was just uploaded so a
    /// wrapping `AtlasUpload` can replay only the diff to the GPU,
    /// rather than re-uploading the whole texture every frame. Drained
    /// by `take_dirty_rects`.
    dirty: Vec<DirtyRect>,
    /// Counters for diagnostics + bench validation.
    hits: u64,
    misses: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl GlyphAtlas {
    /// New empty atlas backed by a `width × height` R8 buffer.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height) as usize],
            map: HashMap::new(),
            packer: ShelfPacker::new(width, height),
            dirty: Vec::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Convenience: default-sized atlas (2048×2048).
    pub fn default_size() -> Self {
        Self::new(ATLAS_DIM, ATLAS_DIM)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of unique glyphs currently resident.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Cumulative lookup hit count since construction.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cumulative lookup miss count since construction.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Borrow the CPU-side alpha buffer. Used by `AtlasUpload` to push
    /// the initial empty texture; the upload path normally uses
    /// `take_dirty_rects` + subregion writes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Take and clear the list of rectangles modified since the last
    /// call. The caller (typically `AtlasUpload`) should `write_texture`
    /// these subregions to the GPU before the next frame.
    pub fn take_dirty_rects(&mut self) -> Vec<DirtyRect> {
        std::mem::take(&mut self.dirty)
    }

    /// Look up the glyph for `key`, rasterizing + packing on miss.
    ///
    /// Returns `None` only when the atlas is full and the new tile
    /// won't fit. In v0.7 the renderer treats that as "draw a blank"
    /// rather than crash; v0.8 will add LRU eviction.
    pub fn get_or_insert<R: Rasterizer>(
        &mut self,
        key: GlyphKey,
        rasterizer: &mut R,
    ) -> Option<GlyphInfo> {
        if let Some(info) = self.map.get(&key) {
            self.hits += 1;
            return Some(*info);
        }
        self.misses += 1;
        let tile = rasterizer.rasterize(key)?;
        // Empty tile (space, etc.) — stash a zero-area UV; no upload
        // needed. The renderer will skip the draw instance anyway.
        if tile.is_empty() {
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [tile.offset_x, tile.offset_y],
                advance: tile.advance,
            };
            self.map.insert(key, info);
            return Some(info);
        }
        let (x, y) = self.packer.alloc(tile.width, tile.height)?;
        // Blit coverage rows into the CPU buffer.
        for row in 0..tile.height {
            let src_off = (row * tile.width) as usize;
            let dst_off = ((y + row) * self.width + x) as usize;
            let len = tile.width as usize;
            self.pixels[dst_off..dst_off + len]
                .copy_from_slice(&tile.coverage[src_off..src_off + len]);
        }
        self.dirty.push(DirtyRect { x, y, w: tile.width, h: tile.height });
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
        };
        self.map.insert(key, info);
        Some(info)
    }

    /// Just-the-lookup variant — for cases where the caller already
    /// knows the glyph is resident (e.g. after a pre-pass). Returns
    /// `None` on a miss without rasterizing.
    pub fn get(&self, key: GlyphKey) -> Option<GlyphInfo> {
        self.map.get(&key).copied()
    }

    /// Hit rate as a percentage (0..=100). Returns 0 when no lookups
    /// have been made yet.
    pub fn hit_rate_pct(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        (self.hits as f64 / total as f64) * 100.0
    }

    /// Sample a single coverage byte from the CPU buffer. Used in
    /// tests to assert that rasterized pixels actually landed where
    /// the GlyphInfo's UV says they should.
    pub fn sample(&self, x: u32, y: u32) -> u8 {
        self.pixels[(y * self.width + x) as usize]
    }
}

/// Deterministic synthetic rasterizer used by tests and the bench. Each
/// glyph becomes an `NxN` ramp where `N` is `8 + (key.ch as u32 % 8)`,
/// so different chars produce different sizes (exercising the packer)
/// and bold/italic of the same char produce identical-sized but distinct
/// coverage patterns (exercising key separation).
#[derive(Default)]
pub struct SyntheticRasterizer {
    pub calls: u64,
}

impl Rasterizer for SyntheticRasterizer {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        self.calls += 1;
        if key.ch == ' ' {
            // Space → empty tile.
            return Some(RasterTile {
                width: 0,
                height: 0,
                offset_x: 0,
                offset_y: 0,
                advance: 8.0,
                coverage: Vec::new(),
            });
        }
        let side = 8 + (key.ch as u32 % 8);
        let bias: u8 = if key.weight_bold { 80 } else { 0 };
        let twist: u8 = if key.italic { 7 } else { 0 };
        let mut coverage = vec![0u8; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                let v = ((x + y) as u8).wrapping_mul(11).wrapping_add(bias).wrapping_add(twist);
                coverage[(y * side + x) as usize] = v;
            }
        }
        Some(RasterTile {
            width: side,
            height: side,
            offset_x: 0,
            offset_y: 0,
            advance: side as f32,
            coverage,
        })
    }
}
