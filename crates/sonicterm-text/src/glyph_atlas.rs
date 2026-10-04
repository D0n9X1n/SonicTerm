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
//!   premultiplied sRGB-encoded color tiles.
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
/// Smallest square glyph atlas a renderer starts with. Warm-pool renderers start here
/// and grow after adoption, so an idle pooled window holds 256 KiB rather than 16 MiB.
pub const MIN_ATLAS_DIM: u32 = 256;
/// Start dimension of a scale-1 renderer's glyph atlas. It equals the start rule over the
/// measured scale-1 inputs in [`crate::start_size_inputs::START_SIZE_INPUTS`], and is the
/// maximum while that table holds no scale-1 row.
pub const START_ATLAS_DIM_1X: u32 = 2048;
/// Start dimension of a scale-2 renderer's glyph atlas, chosen the same way at scale 2.
pub const START_ATLAS_DIM_2X: u32 = 2048;
/// Dirty-list capacity above which a drained list is shrunk, so one growth re-upload of
/// thousands of tiles does not pin its capacity for the atlas's lifetime.
pub const DIRTY_LIST_SHRINK_ABOVE: usize = 1024;
/// Capacity a drained dirty list is shrunk to; ordinary frames dirty fewer tiles than this.
pub const DIRTY_LIST_RETAINED: usize = 64;
/// Candidate start dimensions, smallest first, that [`GlyphAtlas::fit_outcome`] tries.
pub const FIT_DIMS: [u32; 4] = [256, 512, 1024, 2048];

/// Whether an atlas may enlarge itself when packing fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowthPolicy {
    /// The atlas keeps its constructed size and evicts when full.
    Fixed,
    /// The atlas doubles, square, up to `max` before it evicts.
    Growable {
        /// Largest square dimension the atlas may reach.
        max: u32,
    },
}

/// Smallest candidate start size the resident tiles would have fitted, replayed from empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitOutcome {
    /// Every tile packs at this dimension with the used height at most 75% of it.
    Fits(u32),
    /// Every tile packs at the maximum, but no candidate leaves 25% of its height free.
    FitsWithoutHeadroom,
    /// Some tile cannot be placed even at the maximum.
    DoesNotFit,
    /// The atlas has evicted since creation, so its resident set is not the working set.
    Evicted,
}

impl FitOutcome {
    /// Snapshot label: the dimension for a fit, else `no_headroom`, `does_not_fit` or `evicted`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Fits(dim) => dim.to_string(),
            Self::FitsWithoutHeadroom => "no_headroom".to_owned(),
            Self::DoesNotFit => "does_not_fit".to_owned(),
            Self::Evicted => "evicted".to_owned(),
        }
    }
}

/// Replay `tiles` (each a `[width, height]`, in placement order) through a fresh shelf
/// packer at each of [`FIT_DIMS`] and classify the smallest dimension that holds them.
///
/// A dimension qualifies only when every tile is placed and the used height
/// (`shelf_y + shelf_h`) is at most three quarters of it, leaving room for the session to
/// keep drawing new glyphs before the first growth.
#[must_use]
pub fn fit_outcome_of_tiles(tiles: &[[u32; 2]]) -> FitOutcome {
    let mut placed_at_max = false;
    for dim in FIT_DIMS {
        let mut packer = ShelfPacker::new(dim, dim);
        let all_placed =
            tiles.iter().all(|[width, height]| packer.alloc(*width, *height).is_some());
        if !all_placed {
            // When: all_placed is false at this dim a larger candidate may still hold the set.
            continue;
        }
        if packer.used_height().saturating_mul(4) <= dim.saturating_mul(3) {
            // When: used_height leaves a quarter of dim free this is the smallest qualifying fit.
            return FitOutcome::Fits(dim);
        }
        placed_at_max = dim == ATLAS_DIM;
    }
    if placed_at_max {
        FitOutcome::FitsWithoutHeadroom
    } else {
        // When: placed_at_max is false some tile could not be placed even at ATLAS_DIM.
        FitOutcome::DoesNotFit
    }
}

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
    /// Raster offsets `[x, y]` in pixels; see [`RasterTile::offset_x`] and [`RasterTile::offset_y`].
    pub px_offset: [i32; 2],
    /// Horizontal pen advance in pixels. The renderer uses cell-grid
    /// positioning so this is informational for proportional fallback,
    /// not for the main grid path.
    pub advance: f32,
    /// True when this tile holds premultiplied sRGB-encoded BGRA8 color
    /// pixels (Apple Color Emoji, Segoe UI Emoji, Noto Color Emoji). The
    /// shader treats color tiles as pre-shaded and skips the
    /// `cov * fg_color` modulation.
    pub is_color: bool,
    /// True when this tile holds BGRA subpixel text coverage. The renderer
    /// multiplies each coverage channel by the requested foreground color.
    pub is_subpixel: bool,
    /// True only for the sentinel cached when the rasterizer could not resolve the glyph:
    /// the renderer draws a tofu box for it, and [`GlyphAtlas::forget_missing`] drops it once a
    /// fallback face is published. An empty glyph, such as a space, is never missing.
    pub missing: bool,
}

/// A single rasterized glyph: alpha coverage mask + the metrics needed
/// to build the `GlyphInfo`.
#[derive(Debug, Clone)]
pub struct RasterTile {
    /// Glyph tile width in pixels.
    pub width: u32,
    /// Glyph tile height in pixels.
    pub height: u32,
    /// Font tile x-offset from the pen origin in pixels, positive rightward; unused for block/image tiles.
    pub offset_x: i32,
    /// Font tile y-offset from the baseline in pixels, positive downward; unused for block/image tiles.
    pub offset_y: i32,
    /// Horizontal advance after drawing this glyph, in pixels.
    pub advance: f32,
    /// When `is_color == false && is_subpixel == false`: `width * height`
    /// bytes of 8-bit coverage, row-major. When `is_color == true`:
    /// `width * height * 4` bytes of premultiplied sRGB-encoded BGRA8 pixels, row-major.
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
/// Implementors are typically font-backed (`sonicterm_engine::FontStack`
/// in production, a deterministic synthetic rasterizer in tests).
/// Returning `None` is a fatal-for-this-glyph signal — the atlas falls
/// back to a blank tile and tracks the miss so callers can log it.
/// Implementors must NOT panic on unknown keys.
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
    /// New packer for a `width × height` atlas, starting with an empty shelf.
    ///
    /// The packer owns placement only: `cursor_x`, `shelf_y` and `shelf_h`
    /// describe the open shelf, and `shelf_h` is fixed by the first tile
    /// placed on each one.
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
            // When: w or h exceeds the atlas no placement exists, so alloc returns before
            // mutating cursor_x or shelf_h and a later smaller tile still fits this shelf.
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
            // When: cand_shelf_y plus cand_shelf_h passes the atlas height no shelf room
            // is left, and returning before the commit keeps the packer state reusable.
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

    /// Height consumed so far: the open shelf's top plus its height.
    #[doc(hidden)]
    pub fn used_height(&self) -> u32 {
        self.shelf_y.saturating_add(self.shelf_h)
    }

    /// Enlarge the packing area to `width × height` without moving any placement.
    ///
    /// The open shelf keeps its cursor and height and gains the new width; everything below
    /// the old height becomes free for new shelves. Closed shelves do not reclaim their new
    /// right-hand space, which is what keeps every placed tile at its pixel position.
    #[doc(hidden)]
    pub fn grow(&mut self, width: u32, height: u32) {
        debug_assert!(width >= self.width && height >= self.height, "a packer never shrinks");
        self.width = width;
        self.height = height;
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

#[derive(Debug, Clone, Copy)]
struct AtlasAllocation {
    x: u32,
    y: u32,
    slot_w: u32,
    slot_h: u32,
    reused: bool,
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
    /// Whether packing failure enlarges the atlas before it evicts.
    growth: GrowthPolicy,
    /// Monotonic count of size doublings; survives `reset_in_place`.
    growths: u64,
    /// Whether any eviction has happened since construction; survives `reset_in_place`.
    ever_evicted: bool,
}

/// Pixel interpretation required when uploading one atlas write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AtlasPixelKind {
    /// Linear monochrome or subpixel coverage copied without color conversion.
    Coverage,
    /// Premultiplied sRGB-encoded color converted for sRGB texture sampling.
    Color,
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
    /// Interpretation of the bytes in this rectangle.
    pub kind: AtlasPixelKind,
}

/// Bytes per atlas pixel — BGRA8 = 4. The CPU buffer is `width *
/// height * BYTES_PER_PIXEL` bytes; monochrome tiles replicate their
/// coverage into all four channels at upload time so a single shader
/// path can sample either flavor.
pub const BYTES_PER_PIXEL: u32 = 4;

fn dirty_rects_overlap(left: DirtyRect, right: DirtyRect) -> bool {
    left.x < right.x.saturating_add(right.w)
        && right.x < left.x.saturating_add(left.w)
        && left.y < right.y.saturating_add(right.h)
        && right.y < left.y.saturating_add(left.h)
}

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
            growth: GrowthPolicy::Fixed,
            growths: 0,
            ever_evicted: false,
        }
    }

    /// Convenience: default-sized atlas (2048×2048). Fixed: it never grows.
    pub fn default_size() -> Self {
        Self::new(ATLAS_DIM, ATLAS_DIM)
    }

    /// New square atlas of `start` pixels that doubles up to `max` before it evicts.
    ///
    /// This is the only growing constructor; the renderer's glyph atlas is its only caller.
    ///
    /// # Panics
    ///
    /// When `start` or `max` is not a power of two, or `start > max`.
    #[must_use]
    pub fn growable(start: u32, max: u32) -> Self {
        assert!(
            start.is_power_of_two() && max.is_power_of_two(),
            "growable sizes are powers of two"
        );
        assert!(start <= max, "a growable atlas starts at or below its maximum");
        let mut atlas = Self::new(start, start);
        atlas.growth = GrowthPolicy::Growable { max };
        atlas
    }

    /// Whether this atlas grows on packing failure, and to what maximum.
    #[must_use]
    pub fn growth_policy(&self) -> GrowthPolicy {
        self.growth
    }

    /// Monotonic count of size doublings since construction, including across resets.
    #[must_use]
    pub fn growths(&self) -> u64 {
        self.growths
    }

    /// Largest tile width and height a lookup may place: the growth maximum when growable,
    /// else the current size. A larger tile is cached as a sentinel instead.
    fn placement_limit(&self) -> (u32, u32) {
        match self.growth {
            GrowthPolicy::Growable { max } => (max, max),
            GrowthPolicy::Fixed => (self.width, self.height),
        }
    }

    /// The next doubled dimension when this atlas may still grow, else `None`.
    fn next_growth_dim(&self) -> Option<u32> {
        match self.growth {
            GrowthPolicy::Growable { max } if self.width < max => {
                Some(self.width.saturating_mul(2).min(max))
            }
            // When: growth is Fixed or width already reached max, packing failure must evict.
            _ => None,
        }
    }

    /// Enlarge the atlas to `dim × dim` without re-rasterizing anything.
    ///
    /// Every resident tile keeps its pixel position, so its pixels are copied row by row and
    /// its normalized UVs are recomputed from its own tile size. The identity advances so
    /// every UV-bearing cache rebuilds, and the dirty list is replaced by one rect per
    /// resident tile, typed by that tile's pixel kind, so the recreated GPU texture receives
    /// each tile exactly once.
    ///
    /// # Panics
    ///
    /// When the atlas is not square or `dim` does not exceed its current size.
    pub fn grow_to(&mut self, dim: u32) {
        assert!(self.width == self.height && dim > self.width, "growth enlarges a square atlas");
        let bpp = BYTES_PER_PIXEL as usize;
        let old_row_bytes = self.width as usize * bpp;
        let new_row_bytes = dim as usize * bpp;
        let mut grown = vec![0u8; new_row_bytes * dim as usize];
        for (row_index, old_row) in self.pixels.chunks_exact(old_row_bytes).enumerate() {
            let start = row_index * new_row_bytes;
            grown[start..start + old_row_bytes].copy_from_slice(old_row);
        }
        // The old buffer drops here, so peak memory is both buffers for this call only.
        self.pixels = grown;
        self.packer.grow(dim, dim);
        self.width = dim;
        self.height = dim;
        let dim_f = dim as f32;
        let mut uploads: Vec<DirtyRect> = Vec::new();
        for entry in self.map.values_mut() {
            let Some((x, y, _, _)) = entry.rect else {
                // When: rect is None the entry owns no pixels, so its zero UV stays as is.
                continue;
            };
            let [tile_w, tile_h] = entry.info.px_size;
            // UVs use the tile size, never the slot size: a reused slot may be larger than
            // its tile, and the slot margin can hold an evicted tile's stale pixels.
            entry.info.uv = [
                x as f32 / dim_f,
                y as f32 / dim_f,
                (x + tile_w) as f32 / dim_f,
                (y + tile_h) as f32 / dim_f,
            ];
            let kind =
                if entry.info.is_color { AtlasPixelKind::Color } else { AtlasPixelKind::Coverage };
            uploads.push(DirtyRect { x, y, w: tile_w, h: tile_h, kind });
        }
        // HashMap order is arbitrary; sorting keeps the re-upload order deterministic.
        uploads.sort_by_key(|rect| (rect.y, rect.x));
        self.dirty.clear();
        self.dirty.extend(uploads);
        self.identity = self.identity.wrapping_add(1);
        self.growths += 1;
        tracing::debug!(
            target: "sonic::glyph_atlas",
            dim,
            growths = self.growths,
            resident = self.map.len(),
            "glyph atlas grew"
        );
    }

    /// Allocate a `width × height` slot, growing the atlas before giving up. Never evicts.
    fn alloc_rect_growing(&mut self, width: u32, height: u32) -> Option<AtlasAllocation> {
        loop {
            if let Some(allocation) = self.alloc_rect(width, height) {
                return Some(allocation);
            }
            // Each pass doubles toward the maximum, so the loop ends after at most three
            // growths from the 256 floor.
            let next_dim = self.next_growth_dim()?;
            self.grow_to(next_dim);
        }
    }

    /// Area in pixels of every resident tile at its tile size.
    #[must_use]
    pub fn packed_pixels(&self) -> u64 {
        self.map
            .values()
            .filter(|entry| entry.rect.is_some())
            .map(|entry| u64::from(entry.info.px_size[0]) * u64::from(entry.info.px_size[1]))
            .sum()
    }

    /// Largest resident tile width and largest resident tile height, independently.
    #[must_use]
    pub fn max_tile_dims(&self) -> [u32; 2] {
        self.map.values().filter(|entry| entry.rect.is_some()).fold(
            [0, 0],
            |[max_w, max_h], entry| {
                [max_w.max(entry.info.px_size[0]), max_h.max(entry.info.px_size[1])]
            },
        )
    }

    /// Keys of every resident tile that owns atlas pixels; sentinels are excluded.
    #[must_use]
    pub fn resident_tile_keys(&self) -> std::collections::HashSet<GlyphKey> {
        self.map.iter().filter(|(_, entry)| entry.rect.is_some()).map(|(key, _)| *key).collect()
    }

    /// Reserved capacity of the CPU pixel buffer alone.
    #[must_use]
    pub fn retained_pixel_bytes(&self) -> usize {
        self.pixels.capacity()
    }

    /// Reserved capacity of the pending dirty list, in rects.
    #[must_use]
    pub fn dirty_capacity(&self) -> usize {
        self.dirty.capacity()
    }

    /// Smallest candidate start size the resident tiles would have fitted.
    ///
    /// Replays the resident tiles in `(y, x)` order at their tile size through a fresh
    /// packer, per [`fit_outcome_of_tiles`]. An atlas that has ever evicted reports
    /// [`FitOutcome::Evicted`], since its resident set is no longer its working set.
    #[must_use]
    pub fn fit_outcome(&self) -> FitOutcome {
        if self.ever_evicted {
            // When: ever_evicted is set the resident set understates the working set.
            return FitOutcome::Evicted;
        }
        let mut placed: Vec<(u32, u32, [u32; 2])> = self
            .map
            .values()
            .filter_map(|entry| entry.rect.map(|(x, y, _, _)| (y, x, entry.info.px_size)))
            .collect();
        placed.sort_unstable();
        let tiles: Vec<[u32; 2]> = placed.into_iter().map(|(_, _, size)| size).collect();
        fit_outcome_of_tiles(&tiles)
    }

    /// Shrink the dirty list after a drain when one large batch left it oversized.
    fn shrink_dirty_list(&mut self) {
        if self.dirty.capacity() > DIRTY_LIST_SHRINK_ABOVE {
            self.dirty.shrink_to(DIRTY_LIST_RETAINED);
        }
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
        self.shrink_dirty_list();
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
    /// Bytes are the pixel buffer's reserved capacity plus the pending dirty
    /// list's reserved capacity. Items count resident
    /// glyph entries rather than pages: a page is a fixed 16 MiB allocation
    /// that says nothing about occupancy, while the entry count is what
    /// eviction actually acts on.
    ///
    /// Note this reports live storage, not live *glyphs* — a recycled slot
    /// over-reserves when a shorter tile replaces a taller one, so the pixel
    /// buffer can hold bytes no entry references.
    #[must_use]
    pub fn retained_amount(&self) -> ResourceAmount {
        let dirty_bytes = self.dirty.capacity() * std::mem::size_of::<DirtyRect>();
        ResourceAmount { bytes: self.pixels.capacity() + dirty_bytes, items: self.map.len() }
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
    ///
    /// A list left over 1,024 rects of capacity, as after a growth re-upload, shrinks to 64.
    pub fn drain_dirty_rects_into(&mut self, out: &mut Vec<DirtyRect>) {
        out.clear();
        out.append(&mut self.dirty);
        self.shrink_dirty_list();
    }

    /// Discard pending dirty rectangles, shrinking an oversized list like a drain does.
    pub fn clear_dirty_rects(&mut self) {
        self.dirty.clear();
        self.shrink_dirty_list();
    }

    /// Look up the glyph for `key`, rasterizing + packing on miss.
    ///
    /// On a hit, bumps the entry's `last_used_frame` to the current
    /// frame so LRU eviction will spare it.
    ///
    /// On a miss, admission may evict the coldest 25% of entries when eviction
    /// is enabled. Returns `None` when the entry ceiling or pixel allocator
    /// cannot admit the tile under the current eviction policy; the renderer
    /// treats that as "draw a blank" rather than crashing.
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

    /// Drop every cached missing-glyph sentinel, so those keys rasterize again on their next
    /// lookup. Missing entries own no rectangle, so every resident tile and UV is unchanged.
    pub fn forget_missing(&mut self) {
        self.map.retain(|_, entry| !entry.info.missing);
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
            // be placed, so it is refused before build_tile materializes any pixels.
            return None;
        }
        let allocation = self.alloc_rect_growing(width, height)?;
        let tile = build_tile();
        if tile.is_empty() || tile.width != width || tile.height != height {
            // When: tile does not match the reserved width and height the slot would be
            // written at the wrong extent, so the reservation returns to free_rects.
            self.free_rects.push((
                allocation.x,
                allocation.y,
                allocation.slot_w,
                allocation.slot_h,
            ));
            return None;
        }
        Some(self.insert_tile(key, tile, allocation))
    }

    fn get_or_insert_impl<R: Rasterizer>(
        &mut self,
        key: GlyphKey,
        rasterizer: &mut R,
        allow_eviction: bool,
    ) -> Option<GlyphInfo> {
        if let Some(entry) = self.map.get_mut(&key) {
            // When: key is already in map the cached info is returned without rasterizing,
            // and last_used_frame is refreshed so a hot glyph survives the LRU sweep.
            entry.last_used_frame = self.current_frame;
            self.hits += 1;
            return Some(entry.info);
        }
        self.misses += 1;
        if !self.make_entry_room(allow_eviction) {
            // When: make_entry_room cannot free a slot the index is full, so the glyph is
            // refused and goes missing rather than displacing a resident tile.
            return None;
        }
        // Rasterizer miss: cache a sentinel "blank" GlyphInfo so we don't
        // retry the same failing key every frame. Renderer treats
        // zero-area UV as "draw the tofu fallback box" (see Renderer's
        // missing-glyph path).
        let Some(tile) = rasterizer.rasterize(key) else {
            // When: rasterize fails for key a zero-area sentinel is cached, so the same
            // failing glyph is not retried every frame and draws as the tofu box.
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [0, 0],
                advance: 0.0,
                is_color: false,
                is_subpixel: false,
                missing: true,
            };
            self.map
                .insert(key, AtlasEntry { info, last_used_frame: self.current_frame, rect: None });
            return Some(info);
        };
        // Empty tile (space, etc.) — stash a zero-area UV; no upload
        // needed. The renderer will skip the draw instance anyway.
        if tile.is_empty() {
            // When: tile is_empty the glyph has no pixels, so a zero-area UV is cached
            // with the real advance and offsets and the draw instance is skipped.
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [tile.offset_x, tile.offset_y],
                advance: tile.advance,
                is_color: tile.is_color,
                is_subpixel: tile.is_subpixel,
                missing: false,
            };
            self.map
                .insert(key, AtlasEntry { info, last_used_frame: self.current_frame, rect: None });
            return Some(info);
        }
        // A tile larger than the atlas can never fit. Do not evict valid
        // entries before discovering that the retry is equally impossible;
        // callers can skip or resize the oversized asset without invalidating
        // every cached UV in the process.
        let (limit_w, limit_h) = self.placement_limit();
        if tile.width > limit_w || tile.height > limit_h {
            // When: tile exceeds placement_limit it can never be placed, so a sentinel is
            // cached instead of growing or evicting live tiles for an impossible retry.
            let info = GlyphInfo {
                uv: [0.0, 0.0, 0.0, 0.0],
                px_size: [0, 0],
                px_offset: [0, 0],
                advance: 0.0,
                is_color: false,
                is_subpixel: false,
                missing: false,
            };
            self.map
                .insert(key, AtlasEntry { info, last_used_frame: self.current_frame, rect: None });
            return Some(info);
        }
        // Allocate: try free-list first (slots reclaimed by prior eviction), then the shelf
        // packer, then growth toward the maximum, and only then evict-and-retry. Growth moves
        // no tile, so it is allowed even while eviction is disabled.
        let allocation = match self.alloc_rect_growing(tile.width, tile.height) {
            Some(allocation) => allocation,
            None if allow_eviction && self.eviction_enabled => {
                self.evict_lru_quartile();
                self.alloc_rect(tile.width, tile.height)?
            }
            None => {
                // When: alloc_rect fails with eviction barred the tile is refused, so the
                // glyph goes missing this frame rather than displacing a resident tile.
                return None;
            }
        };
        Some(self.insert_tile(key, tile, allocation))
    }

    fn make_entry_room(&mut self, allow_eviction: bool) -> bool {
        if self.map.len() < MAX_ATLAS_ENTRIES {
            // When: map is below MAX_ATLAS_ENTRIES a slot is already free, so admission
            // proceeds without disturbing any resident entry.
            return true;
        }
        if !allow_eviction || !self.eviction_enabled {
            // When: allow_eviction or eviction_enabled is false the index stops admitting
            // instead of growing, so memory looks flat while later glyphs go missing.
            return false;
        }
        self.evict_lru_quartile();
        self.map.len() < MAX_ATLAS_ENTRIES
    }

    fn insert_tile(
        &mut self,
        key: GlyphKey,
        tile: RasterTile,
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
    /// packer. Returns placement, the complete reserved slot size, and whether
    /// the slot was reclaimed, so insertion can supersede stale dirty metadata
    /// only when overlap is possible. Caller handles the eviction retry on `None`.
    fn alloc_rect(&mut self, w: u32, h: u32) -> Option<AtlasAllocation> {
        // First-fit on the free-list: any reclaimed rect at least as
        // large as the request. Reuses the full slot (no splitting),
        // which over-reserves vertically when the new tile is shorter
        // than the old, but keeps the data structure simple and avoids
        // fragmentation thrash. Monospace tiles are nearly uniform in
        // size so the waste is bounded.
        for i in 0..self.free_rects.len() {
            let (_, _, fw, fh) = self.free_rects[i];
            if fw >= w && fh >= h {
                // When: fw and fh cover the request the reclaimed slot is reused whole, so
                // an atlas that has cycled through eviction never grows again.
                let (x, y, slot_w, slot_h) = self.free_rects.swap_remove(i);
                return Some(AtlasAllocation { x, y, slot_w, slot_h, reused: true });
            }
        }
        self.packer.alloc(w, h).map(|(x, y)| AtlasAllocation {
            x,
            y,
            slot_w: w,
            slot_h: h,
            reused: false,
        })
    }

    /// Drop the bottom 25% of entries by `last_used_frame`, returning
    /// their atlas rects to the free-list. Entries with no rect
    /// (zero-area sentinels) are evicted but contribute nothing to the
    /// free-list. Called on pack failure; cheap relative to the cost
    /// of growing the atlas (4 MiB+ reallocation).
    fn evict_lru_quartile(&mut self) {
        let total = self.map.len();
        if total == 0 {
            // When: total is zero there is nothing resident to reclaim, and the quartile
            // below rounds up to one entry, which would underflow an empty map.
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
                self.ever_evicted = true;
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
            // When: total is zero no lookup has happened yet, so the ratio below would
            // divide by zero and report a rate no measurement supports.
            return 0.0;
        }
        (self.hits as f64 / total as f64) * 100.0
    }

    /// Sample the alpha (`BGRA[3]`) channel of a single atlas pixel.
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
            // When: key.ch is a space the glyph has no coverage, so an empty tile carries
            // only the advance and the atlas caches it without reserving pixels.

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
