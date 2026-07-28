//! CPU-side BGRA8 glyph atlas keyed by
//! [`sonicterm_types::glyph_key::GlyphKey`]. Monochrome coverage is
//! replicated across channels so the same storage also carries subpixel text,
//! color emoji, custom terminal glyphs, and inline images.
//!
//! The atlas is the centerpiece of the B3 GPU text path. Once warm, a
//! cell renders by:
//!   1. computing its `GlyphKey`            (≈1 ns)
//!   2. looking up the `GlyphInfo`          (≈30 ns, HashMap hit)
//!   3. emitting an instance into the text pipeline's vertex buffer
//!
//! Once warm, the per-cell hot path is a key calculation, a map lookup, and
//! glyph-instance emission. Repeated terminal characters therefore reuse the
//! same raster tile during heavy scrollback.
//!
//! ## Design choices
//!
//! - One 2048×2048 BGRA8 atlas (~16 MiB) stores monochrome, subpixel, and
//!   premultiplied color tiles.
//! - A shelf packer handles similarly-sized terminal glyphs; a free-rectangle
//!   list and deterministic LRU-quartile eviction reclaim space under pressure.
//! - The `Rasterizer` trait keeps the atlas independent of a concrete font
//!   backend and lets tests use synthetic tiles.
//! - Foreground color remains on the instance for monochrome/subpixel text;
//!   color tiles carry their own premultiplied pixels.

use std::collections::HashMap;

use sonicterm_types::{GlyphKey, ResourceAmount};

/// Default atlas dimensions. BGRA8Unorm, so 16 MiB on the GPU. The
/// BGRA channel layout lets one texture serve both monochrome glyphs
/// (coverage replicated into all four channels) and color emoji
/// (premultiplied BGRA from sbix/COLR strikes). The per-tile
/// [`GlyphInfo::is_color`] flag tells the shader which branch to take.
pub const ATLAS_DIM: u32 = 2048;
/// Maximum resident glyph metadata entries, including blank/missing sentinels.
pub const MAX_ATLAS_ENTRIES: usize = 16 * 1024;

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
    /// True when this tile holds premultiplied BGRA color pixels
    /// (Apple Color Emoji, Segoe UI Emoji, Noto Color Emoji). The
    /// shader treats color tiles as pre-shaded and skips the
    /// `cov * fg_color` modulation.
    pub is_color: bool,
    /// True when this tile holds BGRA subpixel text coverage. The renderer
    /// multiplies each coverage channel by the requested foreground color.
    pub is_subpixel: bool,
}

/// A single rasterized glyph: alpha coverage mask + the metrics needed
/// to build the `GlyphInfo`.
#[derive(Debug, Clone)]
pub struct RasterTile {
    /// Glyph tile width in pixels.
    pub width: u32,
    /// Glyph tile height in pixels.
    pub height: u32,
    /// Top-left offset of the visible pixels relative to the cell box.
    pub offset_x: i32,
    /// Top-left vertical offset of the visible pixels relative to the cell box.
    pub offset_y: i32,
    /// Horizontal advance after drawing this glyph, in pixels.
    pub advance: f32,
    /// When `is_color == false && is_subpixel == false`: `width * height`
    /// bytes of 8-bit coverage, row-major. When `is_color == true`:
    /// `width * height * 4` bytes of premultiplied BGRA pixels, row-major.
    /// When `is_subpixel == true`: `width * height * 4` bytes of BGRA
    /// subpixel coverage, row-major.
    pub coverage: Vec<u8>,
    /// True when `coverage` is BGRA color emoji; false for text coverage.
    pub is_color: bool,
    /// True when `coverage` is BGRA subpixel text coverage.
    pub is_subpixel: bool,
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
    /// Rasterize the glyph identified by `key`, or return `None` if the
    /// glyph cannot be produced.
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
/// This split lets integration tests and the production renderer share
/// the same packing + lookup logic without pulling in a GPU dependency.
/// Per-glyph entry: the public `GlyphInfo` plus the bookkeeping the
/// atlas needs for LRU eviction.
///
/// `rect` is `None` for zero-area entries (spaces, rasterizer-miss
/// sentinels) — those occupy no atlas pixels and so contribute nothing
/// to free-list reclamation when evicted. They still get evicted by
/// LRU like any other entry; the only difference is no rect is pushed
/// to `free_rects`.
#[derive(Debug, Clone, Copy)]
struct AtlasEntry {
    info: GlyphInfo,
    last_used_frame: u64,
    rect: Option<(u32, u32, u32, u32)>,
}

/// CPU-side BGRA8 glyph atlas with shelf-packed allocation and LRU eviction.
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    map: HashMap<GlyphKey, AtlasEntry>,
    packer: ShelfPacker,
    /// Rectangles freed by LRU eviction. The allocator scans this
    /// list (first-fit) before asking the shelf packer, so an atlas
    /// that has cycled through eviction can keep reusing the same
    /// region of pixels indefinitely without ever growing.
    free_rects: Vec<(u32, u32, u32, u32)>,
    /// Each `get_or_insert` records which rect was just uploaded so a
    /// wrapping `AtlasUpload` can replay only the diff to the GPU,
    /// rather than re-uploading the whole texture every frame. Drained
    /// by `take_dirty_rects`.
    dirty: Vec<DirtyRect>,
    /// Monotonic identity of the atlas's contents. Bumped wherever a cached
    /// coordinate could stop meaning what it meant, and never reset, so a
    /// stale value can never come back around and match again.
    identity: u64,
    /// Counters for diagnostics + bench validation.
    hits: u64,
    misses: u64,
    /// Monotonic frame counter. Bumped by `tick_frame()`; recorded on
    /// every lookup/insert so LRU eviction can find the coldest entries.
    current_frame: u64,
    /// Cumulative eviction count for diagnostics.
    evictions: u64,
    /// Runtime policy used by the renderer's bounded compaction retry.
    /// When false, a full atlas rejects new tiles instead of recycling
    /// rectangles referenced earlier in the frame.
    eviction_enabled: bool,
}

/// A rectangle of the atlas that has been written since the last drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    /// X position in pixels.
    pub x: u32,
    /// Y position in pixels.
    pub y: u32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// Bytes per atlas pixel — BGRA8 = 4. The CPU buffer is `width *
/// height * BYTES_PER_PIXEL` bytes; monochrome tiles replicate their
/// coverage into all four channels at upload time so a single shader
/// path can sample either flavor.
pub const BYTES_PER_PIXEL: u32 = 4;

impl GlyphAtlas {
    /// New empty atlas backed by a `width × height` BGRA8 buffer.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height * BYTES_PER_PIXEL) as usize],
            map: HashMap::new(),
            packer: ShelfPacker::new(width, height),
            free_rects: Vec::new(),
            identity: 0,
            dirty: Vec::new(),
            hits: 0,
            misses: 0,
            current_frame: 0,
            evictions: 0,
            eviction_enabled: true,
        }
    }

    /// Convenience: default-sized atlas (2048×2048).
    pub fn default_size() -> Self {
        Self::new(ATLAS_DIM, ATLAS_DIM)
    }

    /// Atlas width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Atlas height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of unique glyphs currently resident.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when the atlas has no resident glyphs.
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

    /// Cumulative number of glyph entries evicted by LRU since
    /// construction. Diagnostic only — useful for verifying that long
    /// sessions with diverse glyph sets are actually cycling memory
    /// rather than monotonically growing.
    /// Identity of the atlas's current contents.
    ///
    /// Changes whenever a cached coordinate could have stopped meaning what it
    /// meant: on eviction, which recycles a rect to a different glyph, and on
    /// reset, which replaces everything at once. A dependent cache stores this
    /// alongside its entries and discards those whose identity no longer
    /// matches.
    ///
    /// Never repeats a value. The eviction counter alone cannot serve here
    /// because `reset_in_place` returns it to zero, so a cache holding a
    /// pre-reset value would be invalidated correctly at first and then match
    /// again once the counter climbed back past it — pointing into an atlas
    /// whose contents had been entirely replaced.
    #[must_use]
    pub fn identity(&self) -> u64 {
        self.identity
    }

    /// Number of LRU evictions performed, for diagnostics and bench validation.
    ///
    /// Not an identity: this resets with the atlas. Use [`Self::identity`] for
    /// cache invalidation.
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Current frame counter. Bumped by `tick_frame()`.
    pub fn current_frame(&self) -> u64 {
        self.current_frame
    }

    /// Advance the frame counter. Call once per render frame so LRU
    /// eviction can distinguish recently-used glyphs from cold ones.
    /// Cheap (one integer increment); does not touch the atlas.
    pub fn tick_frame(&mut self) {
        self.current_frame = self.current_frame.wrapping_add(1);
    }

    /// Enable or disable LRU eviction for subsequent regular insertions.
    ///
    /// The renderer disables eviction for one clean retry after compaction.
    /// If that frame's working set is itself too large, excess glyphs are
    /// skipped rather than entering an endless evict-reset-redraw loop.
    pub fn set_eviction_enabled(&mut self, enabled: bool) {
        self.eviction_enabled = enabled;
    }

    /// Reset atlas contents and packing state while retaining the pixel allocation.
    pub fn reset_in_place(&mut self) {
        self.map.clear();
        self.packer = ShelfPacker::new(self.width, self.height);
        self.free_rects.clear();
        self.dirty.clear();
        self.hits = 0;
        self.misses = 0;
        self.current_frame = 0;
        self.evictions = 0;
        // Every coordinate handed out before now is meaningless.
        self.identity = self.identity.wrapping_add(1);
        self.eviction_enabled = true;
    }

    /// Borrow the CPU-side alpha buffer. Used by `AtlasUpload` to push
    /// the initial empty texture; the upload path normally uses
    /// `take_dirty_rects` + subregion writes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Storage this atlas is holding, for governor accounting and telemetry.
    ///
    /// Bytes are the pixel buffer's reserved capacity. Items count resident
    /// glyph entries rather than pages: a page is a fixed 16 MiB allocation
    /// that says nothing about occupancy, while the entry count is what
    /// eviction actually acts on.
    ///
    /// Note this reports live storage, not live *glyphs* — a recycled slot
    /// over-reserves when a shorter tile replaces a taller one, so the pixel
    /// buffer can hold bytes no entry references.
    #[must_use]
    pub fn retained_amount(&self) -> ResourceAmount {
        ResourceAmount { bytes: self.pixels.capacity(), items: self.map.len() }
    }

    /// Borrow the CPU-side atlas pixels without draining dirty rects.
    /// Windows software fallback rendering samples this buffer directly.
    pub fn pixels_bgra(&self) -> &[u8] {
        &self.pixels
    }

    /// Take and clear the list of rectangles modified since the last
    /// call. The caller (typically `AtlasUpload`) should `write_texture`
    /// these subregions to the GPU before the next frame.
    pub fn take_dirty_rects(&mut self) -> Vec<DirtyRect> {
        std::mem::take(&mut self.dirty)
    }

    /// Move pending dirty rectangles into reusable caller-owned storage.
    pub fn drain_dirty_rects_into(&mut self, out: &mut Vec<DirtyRect>) {
        out.clear();
        out.append(&mut self.dirty);
    }

    /// Discard pending dirty rectangles while retaining their allocation.
    pub fn clear_dirty_rects(&mut self) {
        self.dirty.clear();
    }

    /// Look up the glyph for `key`, rasterizing + packing on miss.
    ///
    /// On a hit, bumps the entry's `last_used_frame` to the current
    /// frame so LRU eviction will spare it.
    ///
    /// On a miss where the new tile won't fit, attempts LRU eviction
    /// (drops the coldest 25% of entries, returning their atlas rects
    /// to a free-list) and retries once. Returns `None` only when even
    /// after eviction the tile still won't fit — in v0.7 the renderer
    /// treats that as "draw a blank" rather than crash.
    pub fn get_or_insert<R: Rasterizer>(
        &mut self,
        key: GlyphKey,
        rasterizer: &mut R,
    ) -> Option<GlyphInfo> {
        self.get_or_insert_impl(key, rasterizer, true)
    }

    /// Look up or insert a glyph without evicting any resident entry.
    ///
    /// This is used by bounded secondary atlases such as inline media:
    /// once full, the caller skips excess assets rather than recycling
    /// rectangles that instances assembled earlier in the same frame
    /// still reference.
    pub fn get_or_insert_without_eviction<R: Rasterizer>(
        &mut self,
        key: GlyphKey,
        rasterizer: &mut R,
    ) -> Option<GlyphInfo> {
        self.get_or_insert_impl(key, rasterizer, false)
    }

    /// Look up or insert a known-size tile without eviction, building its
    /// pixel payload only after an atlas rectangle has been reserved.
    ///
    /// Bounded image atlases use this to avoid cloning large pixel buffers
    /// for entries that cannot fit. `build_tile` is not invoked on a cache
    /// hit, an oversized request, or allocation failure.
    pub fn get_or_insert_lazy_without_eviction<F>(
        &mut self,
        key: GlyphKey,
        width: u32,
        height: u32,
        build_tile: F,
    ) -> Option<GlyphInfo>
    where
        F: FnOnce() -> RasterTile,
    {
        if let Some(entry) = self.map.get_mut(&key) {
            entry.last_used_frame = self.current_frame;
            self.hits += 1;
            return Some(entry.info);
        }
        self.misses += 1;
        if !self.make_entry_room(false) {
            return None;
        }
        if width == 0 || height == 0 || width > self.width || height > self.height {
            return None;
        }
        let (x, y, slot_w, slot_h) = self.alloc_rect(width, height)?;
        let tile = build_tile();
        if tile.is_empty() || tile.width != width || tile.height != height {
            self.free_rects.push((x, y, slot_w, slot_h));
            return None;
        }
        Some(self.insert_tile(key, tile, x, y, slot_w, slot_h))
    }

    fn get_or_insert_impl<R: Rasterizer>(
        &mut self,
        key: GlyphKey,
        rasterizer: &mut R,
        allow_eviction: bool,
    ) -> Option<GlyphInfo> {
        if let Some(entry) = self.map.get_mut(&key) {
            entry.last_used_frame = self.current_frame;
            self.hits += 1;
            return Some(entry.info);
        }
        self.misses += 1;
        if !self.make_entry_room(allow_eviction) {
            return None;
        }
        // Rasterizer miss: cache a sentinel "blank" GlyphInfo so we don't
        // retry the same failing key every frame. Renderer treats
        // zero-area UV as "draw the tofu fallback box" (see Renderer's
        // missing-glyph path).
        let Some(tile) = rasterizer.rasterize(key) else {
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [0, 0],
                advance: 0.0,
                is_color: false,
                is_subpixel: false,
            };
            self.map
                .insert(key, AtlasEntry { info, last_used_frame: self.current_frame, rect: None });
            return Some(info);
        };
        // Empty tile (space, etc.) — stash a zero-area UV; no upload
        // needed. The renderer will skip the draw instance anyway.
        if tile.is_empty() {
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [tile.offset_x, tile.offset_y],
                advance: tile.advance,
                is_color: tile.is_color,
                is_subpixel: tile.is_subpixel,
            };
            self.map
                .insert(key, AtlasEntry { info, last_used_frame: self.current_frame, rect: None });
            return Some(info);
        }
        // A tile larger than the atlas can never fit. Do not evict valid
        // entries before discovering that the retry is equally impossible;
        // callers can skip or resize the oversized asset without invalidating
        // every cached UV in the process.
        if tile.width > self.width || tile.height > self.height {
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [0, 0],
                advance: 0.0,
                is_color: false,
                is_subpixel: false,
            };
            self.map
                .insert(key, AtlasEntry { info, last_used_frame: self.current_frame, rect: None });
            return Some(info);
        }
        // Allocate: try free-list first (slots reclaimed by prior
        // eviction), then the shelf packer, then evict-and-retry.
        let (x, y, slot_w, slot_h) = match self.alloc_rect(tile.width, tile.height) {
            Some(allocation) => allocation,
            None if allow_eviction && self.eviction_enabled => {
                self.evict_lru_quartile();
                self.alloc_rect(tile.width, tile.height)?
            }
            None => return None,
        };
        Some(self.insert_tile(key, tile, x, y, slot_w, slot_h))
    }

    fn make_entry_room(&mut self, allow_eviction: bool) -> bool {
        if self.map.len() < MAX_ATLAS_ENTRIES {
            return true;
        }
        if !allow_eviction || !self.eviction_enabled {
            return false;
        }
        self.evict_lru_quartile();
        self.map.len() < MAX_ATLAS_ENTRIES
    }

    fn insert_tile(
        &mut self,
        key: GlyphKey,
        tile: RasterTile,
        x: u32,
        y: u32,
        slot_w: u32,
        slot_h: u32,
    ) -> GlyphInfo {
        // Blit rows into the CPU BGRA buffer. Monochrome tiles arrive
        // as `width*height` alpha bytes — replicate each into the four
        // BGRA channels so the shader can sample a single uniform
        // texture format. Color tiles arrive as `width*height*4` BGRA
        // bytes already premultiplied; subpixel text tiles arrive as
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
            is_color: tile.is_color,
            is_subpixel: tile.is_subpixel,
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

    /// Try to allocate `(w, h)` from the free-list first, then the shelf
    /// packer. Returns the placement plus the complete reserved slot size so
    /// eviction or rollback can restore a larger reclaimed rectangle intact.
    /// Caller handles the eviction retry on `None`.
    fn alloc_rect(&mut self, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
        // First-fit on the free-list: any reclaimed rect at least as
        // large as the request. Reuses the full slot (no splitting),
        // which over-reserves vertically when the new tile is shorter
        // than the old, but keeps the data structure simple and avoids
        // fragmentation thrash. Monospace tiles are nearly uniform in
        // size so the waste is bounded.
        for i in 0..self.free_rects.len() {
            let (_, _, fw, fh) = self.free_rects[i];
            if fw >= w && fh >= h {
                return Some(self.free_rects.swap_remove(i));
            }
        }
        self.packer.alloc(w, h).map(|(x, y)| (x, y, w, h))
    }

    /// Drop the bottom 25% of entries by `last_used_frame`, returning
    /// their atlas rects to the free-list. Entries with no rect
    /// (zero-area sentinels) are evicted but contribute nothing to the
    /// free-list. Called on pack failure; cheap relative to the cost
    /// of growing the atlas (4 MiB+ reallocation).
    fn evict_lru_quartile(&mut self) {
        let total = self.map.len();
        if total == 0 {
            return;
        }
        // Evict at least 1 entry even when 25% rounds to 0 — otherwise
        // a tiny atlas could deadlock here on a single hot-key miss.
        let evict_n = (total / 4).max(1);
        // Collect (last_used_frame, key) pairs and use a deterministic
        // total ordering before taking the oldest quartile. The secondary
        // key prevents equal timestamps from depending on HashMap iteration
        // order or an unstable selection algorithm.
        let mut ages: Vec<(u64, GlyphKey)> =
            self.map.iter().map(|(k, e)| (e.last_used_frame, *k)).collect();
        ages.sort_by_key(|(frame, key)| {
            (*frame, u32::from(key.ch), key.font_slot, key.weight_bold, key.italic, key.glyph_id)
        });
        ages.truncate(evict_n);
        let evictions_before = self.evictions;
        for (_, k) in ages {
            if let Some(entry) = self.map.remove(&k) {
                if let Some(rect) = entry.rect {
                    self.free_rects.push(rect);
                }
                self.evictions += 1;
                self.identity = self.identity.wrapping_add(1);
            }
        }
        tracing::debug!(
            target: "sonic::glyph_atlas",
            resident_before = total,
            resident_after = self.map.len(),
            evicted = self.evictions.saturating_sub(evictions_before),
            eviction_epoch = self.evictions,
            free_rects = self.free_rects.len(),
            "glyph atlas reclaimed LRU entries"
        );
    }

    /// Just-the-lookup variant — for cases where the caller already
    /// knows the glyph is resident (e.g. after a pre-pass). Returns
    /// `None` on a miss without rasterizing. Does NOT bump
    /// `last_used_frame`; callers that want LRU credit should go
    /// through `get_or_insert`.
    pub fn get(&self, key: GlyphKey) -> Option<GlyphInfo> {
        self.map.get(&key).map(|e| e.info)
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

    /// Sample the alpha (BGRA[3]) channel of a single atlas pixel.
    /// Used in tests to assert that rasterized pixels actually landed
    /// where the GlyphInfo's UV says they should. We return the alpha
    /// channel specifically because (a) for monochrome glyphs all four
    /// channels equal the original coverage, and (b) for color emoji
    /// the alpha channel is the meaningful "is this pixel painted"
    /// signal even when an RGB component happens to be zero.
    pub fn sample(&self, x: u32, y: u32) -> u8 {
        let bpp = BYTES_PER_PIXEL as usize;
        let off = (y * self.width + x) as usize * bpp;
        self.pixels[off + 3]
    }
}

/// Deterministic synthetic rasterizer used by tests and the bench. Each
/// glyph becomes an `NxN` ramp where `N` is `8 + (key.ch as u32 % 8)`,
/// so different chars produce different sizes (exercising the packer)
/// and bold/italic of the same char produce identical-sized but distinct
/// coverage patterns (exercising key separation).
#[derive(Default)]
pub struct SyntheticRasterizer {
    /// Cumulative number of `rasterize` calls; useful for assertions.
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
                is_color: false,
                is_subpixel: false,
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
            is_color: false,
            is_subpixel: false,
        })
    }
}

#[cfg(test)]
#[path = "glyph_atlas_tests.rs"]
mod glyph_atlas_tests;
