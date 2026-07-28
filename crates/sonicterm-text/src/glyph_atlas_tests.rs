use super::*;

struct SubpixelRasterizer;

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

struct OnePixelRasterizer;

impl Rasterizer for OnePixelRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![255],
            is_color: false,
            is_subpixel: false,
        })
    }
}

struct TileRasterizer(RasterTile);

impl Rasterizer for TileRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(self.0.clone())
    }
}

struct MissingRasterizer;

impl Rasterizer for MissingRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        None
    }
}

#[test]
fn subpixel_text_coverage_copies_bgra_channels() {
    let mut atlas = GlyphAtlas::new(4, 4);
    let mut rasterizer = SubpixelRasterizer;
    let info = atlas
        .get_or_insert(
            GlyphKey { ch: 'V', font_slot: 0, weight_bold: false, italic: false, glyph_id: 1 },
            &mut rasterizer,
        )
        .expect("subpixel tile inserts");

    assert!(info.is_subpixel);
    assert!(!info.is_color);
    assert_eq!(&atlas.pixels_bgra()[0..4], &[10, 20, 30, 40]);
}

#[test]
fn reset_in_place_retains_pixels_and_restarts_atlas_state() {
    let mut atlas = GlyphAtlas::new(2, 1);
    let old = GlyphKey::new('a', false, false);
    let old_info = atlas.get_or_insert(old, &mut OnePixelRasterizer).expect("old tile inserts");
    atlas.tick_frame();
    atlas.set_eviction_enabled(false);
    let pixels_ptr = atlas.pixels().as_ptr();
    let pixels_capacity = atlas.pixels.capacity();
    let old_sample = atlas.sample(0, 0);

    atlas.reset_in_place();

    assert_eq!(atlas.pixels().as_ptr(), pixels_ptr);
    assert_eq!(atlas.pixels.capacity(), pixels_capacity);
    assert_eq!(atlas.sample(0, 0), old_sample, "reset must not clear the retained pixel buffer");
    assert!(atlas.get(old).is_none());
    assert!(atlas.is_empty());
    assert_eq!(atlas.hits(), 0);
    assert_eq!(atlas.misses(), 0);
    assert_eq!(atlas.evictions(), 0);
    assert_eq!(atlas.current_frame(), 0);
    assert!(atlas.take_dirty_rects().is_empty());

    let new = GlyphKey::new('b', false, false);
    let replacement = atlas
        .get_or_insert(
            new,
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![23],
                is_color: false,
                is_subpixel: false,
            }),
        )
        .expect("replacement tile inserts");
    assert_eq!(replacement.uv, old_info.uv);
    assert_eq!(atlas.sample(0, 0), 23, "replacement must overwrite retained bytes before sampling");
}

#[test]
fn non_evicting_insert_preserves_resident_tiles_when_full() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut rasterizer = OnePixelRasterizer;
    let first = GlyphKey::new('a', false, false);
    atlas.get_or_insert(first, &mut rasterizer).expect("first tile fills atlas");
    let epoch = atlas.evictions();

    let second =
        atlas.get_or_insert_without_eviction(GlyphKey::new('b', false, false), &mut rasterizer);

    assert!(second.is_none(), "a non-evicting insert must report a full atlas");
    assert_eq!(atlas.evictions(), epoch, "the resident tile must not be recycled");
    assert!(atlas.get(first).is_some(), "the original tile remains addressable");
}

#[test]
fn lazy_non_evicting_insert_does_not_build_tile_when_full() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut rasterizer = OnePixelRasterizer;
    atlas
        .get_or_insert(GlyphKey::new('a', false, false), &mut rasterizer)
        .expect("first tile fills atlas");
    let mut build_calls = 0;

    let second =
        atlas.get_or_insert_lazy_without_eviction(GlyphKey::new('b', false, false), 1, 1, || {
            build_calls += 1;
            RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255],
                is_color: false,
                is_subpixel: false,
            }
        });

    assert!(second.is_none());
    assert_eq!(build_calls, 0, "a rejected insertion must not materialize pixel coverage");
}

#[test]
fn failed_lazy_build_restores_full_reclaimed_slot() {
    let mut atlas = GlyphAtlas::new(2, 2);
    let mut rasterizer = TileRasterizer(RasterTile {
        width: 2,
        height: 2,
        offset_x: 0,
        offset_y: 0,
        advance: 2.0,
        coverage: vec![255; 4],
        is_color: false,
        is_subpixel: false,
    });
    atlas
        .get_or_insert(GlyphKey::new('a', false, false), &mut rasterizer)
        .expect("first tile fills atlas");
    atlas.evict_lru_quartile();

    let invalid =
        atlas.get_or_insert_lazy_without_eviction(GlyphKey::new('b', false, false), 1, 1, || {
            RasterTile {
                width: 2,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255; 2],
                is_color: false,
                is_subpixel: false,
            }
        });
    assert!(invalid.is_none(), "mismatched lazy tile must be rejected");

    let replacement =
        atlas.get_or_insert_lazy_without_eviction(GlyphKey::new('c', false, false), 2, 2, || {
            RasterTile {
                width: 2,
                height: 2,
                offset_x: 0,
                offset_y: 0,
                advance: 2.0,
                coverage: vec![255; 4],
                is_color: false,
                is_subpixel: false,
            }
        });
    assert!(replacement.is_some(), "rollback must restore the complete 2x2 reclaimed slot");
}

#[test]
fn disabled_eviction_bounds_regular_insertions_when_full() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut rasterizer = OnePixelRasterizer;
    let first = GlyphKey::new('a', false, false);
    atlas.get_or_insert(first, &mut rasterizer).expect("first tile fills atlas");
    let epoch = atlas.evictions();
    atlas.set_eviction_enabled(false);

    let second = atlas.get_or_insert(GlyphKey::new('b', false, false), &mut rasterizer);

    assert!(second.is_none(), "disabled eviction must bound a full-atlas retry");
    assert_eq!(atlas.evictions(), epoch);
    assert!(atlas.get(first).is_some());
}

#[test]
fn missing_glyph_metadata_stays_bounded() {
    let mut atlas = GlyphAtlas::new(4, 4);
    let mut rasterizer = MissingRasterizer;
    for codepoint in 0..(MAX_ATLAS_ENTRIES as u32 + 100) {
        let ch = char::from_u32(0xF0000 + codepoint).expect("private-use codepoint");
        atlas.get_or_insert(GlyphKey::new(ch, false, false), &mut rasterizer);
    }

    assert!(atlas.len() <= MAX_ATLAS_ENTRIES);
}

#[test]
fn lazy_insert_metadata_stays_bounded() {
    let mut atlas = GlyphAtlas::default_size();
    for codepoint in 0..(MAX_ATLAS_ENTRIES as u32 + 100) {
        let ch = char::from_u32(0xF0000 + codepoint).expect("private-use codepoint");
        atlas.get_or_insert_lazy_without_eviction(GlyphKey::new(ch, false, false), 1, 1, || {
            RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255],
                is_color: false,
                is_subpixel: false,
            }
        });
    }

    assert!(atlas.len() <= MAX_ATLAS_ENTRIES);
}

#[test]
fn v120_stale_atlas_identity_invalidates_all_dependents_888() {
    // A dependent cache stores atlas coordinates and must discard them
    // whenever those coordinates could have stopped meaning what they meant.
    // Two things do that: eviction, which recycles a rect to a different
    // glyph, and reset, which replaces the contents wholesale.
    //
    // The eviction counter cannot serve as that identity. `reset_in_place`
    // returns it to zero, so a cache holding a pre-reset value is invalidated
    // correctly at first and then matches again once the counter climbs back
    // past it — pointing into an atlas that was entirely replaced. Measured
    // before this fix: 8 -> 0 -> 11, and an entry holding 8 revived.
    let mut atlas = GlyphAtlas::new(32, 32);
    let mut raster = TileRasterizer(RasterTile {
        width: 16,
        height: 16,
        offset_x: 0,
        offset_y: 0,
        advance: 16.0,
        coverage: vec![255; 16 * 16],
        is_color: false,
        is_subpixel: false,
    });
    let key = |n: u32| GlyphKey {
        ch: char::from_u32(n).unwrap_or('a'),
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: n,
    };

    let fresh = atlas.identity();

    // Eviction changes identity, because a freed rect is handed to a new glyph.
    for n in 33..45u32 {
        atlas.get_or_insert(key(n), &mut raster);
    }
    assert!(atlas.evictions() > 0, "the run must evict for this to assert anything");
    let after_eviction = atlas.identity();
    assert_ne!(after_eviction, fresh, "eviction must change the atlas identity");

    // Reset changes it again rather than returning to a prior value.
    atlas.reset_in_place();
    let after_reset = atlas.identity();
    assert_ne!(after_reset, after_eviction, "reset must change the identity");
    assert_ne!(after_reset, fresh, "reset must not return to the identity of a fresh atlas");

    // Refilling past the earlier eviction count must never reproduce an
    // identity a dependent could still be holding.
    for n in 60..80u32 {
        atlas.get_or_insert(key(n), &mut raster);
    }
    let after_refill = atlas.identity();
    assert_ne!(after_refill, fresh);
    assert_ne!(after_refill, after_eviction, "a stale entry must not revive");
    assert!(
        after_refill > after_eviction,
        "identity advances monotonically: {after_eviction} -> {after_refill}"
    );

    // The eviction counter alone does revisit values, which is why it is not
    // the identity. Asserting this keeps the two from being conflated again.
    assert!(
        atlas.evictions() < after_refill,
        "the eviction counter resets and so cannot serve as a cache generation"
    );
}

#[test]
fn retained_amount_reports_pixels_and_resident_entries() {
    // A governor charges the atlas for what it actually holds. Bytes are the
    // pixel buffer, which is allocated up front at full size rather than
    // growing with use; items are resident entries, which is what eviction
    // acts on.
    let mut atlas = GlyphAtlas::new(64, 64);
    let empty = atlas.retained_amount();
    assert_eq!(empty.bytes, 64 * 64 * 4, "the pixel buffer is allocated up front");
    assert_eq!(empty.items, 0, "a fresh atlas holds no entries");

    let mut raster = OnePixelRasterizer;
    let key = |n: u32| GlyphKey {
        ch: char::from_u32(n).unwrap_or('a'),
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: n,
    };
    for n in 33..43u32 {
        atlas.get_or_insert(key(n), &mut raster);
    }

    let filled = atlas.retained_amount();
    assert_eq!(filled.bytes, empty.bytes, "inserting glyphs does not grow the buffer");
    assert_eq!(filled.items, 10, "resident entries are counted");
    assert_eq!(filled.items, atlas.len(), "the item count matches the entry count");
}

#[test]
fn retained_amount_falls_when_eviction_reclaims_entries() {
    // Eviction has to be visible in the reported figure, or a governor would
    // hold a charge for entries the atlas has already dropped.
    let mut atlas = GlyphAtlas::new(32, 32);
    let mut raster = TileRasterizer(RasterTile {
        width: 16,
        height: 16,
        offset_x: 0,
        offset_y: 0,
        advance: 16.0,
        coverage: vec![255; 16 * 16],
        is_color: false,
        is_subpixel: false,
    });
    let key = |n: u32| GlyphKey {
        ch: char::from_u32(n).unwrap_or('a'),
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: n,
    };

    for n in 33..40u32 {
        atlas.get_or_insert(key(n), &mut raster);
    }
    let peak = atlas.retained_amount();
    assert!(atlas.evictions() > 0, "the run must actually evict for this to mean anything");
    assert!(peak.items <= 4, "a 32x32 atlas holds at most four 16x16 tiles");
    assert_eq!(peak.items, atlas.len());
}

/// A rasterizer whose tile size and coverage are chosen per character, so a
/// test can evict a large glyph and insert a smaller one into its slot.
struct SizedRasterizer {
    big: RasterTile,
    small: RasterTile,
}

impl Rasterizer for SizedRasterizer {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        Some(if key.ch == 'B' { self.big.clone() } else { self.small.clone() })
    }
}

#[test]
fn reusing_a_freed_slot_leaves_the_evicted_glyphs_pixels_in_the_margin() {
    // Eviction returns a rect to the free list without clearing the pixels
    // under it, and `alloc_rect` reuses the whole slot rather than splitting
    // it. A smaller glyph landing in a larger freed slot therefore writes
    // only its own extent, and the evicted glyph's ink survives in the
    // margin between the new tile's edge and the slot's.
    //
    // The atlas is sized to hold exactly one 4x4 tile so the second insert
    // is forced to evict and reuse.
    let mut atlas = GlyphAtlas::new(4, 4);
    let mut rasterizer = SizedRasterizer {
        big: RasterTile {
            width: 4,
            height: 4,
            offset_x: 0,
            offset_y: 0,
            advance: 4.0,
            coverage: vec![255; 16],
            is_color: false,
            is_subpixel: false,
        },
        small: RasterTile {
            width: 2,
            height: 2,
            offset_x: 0,
            offset_y: 0,
            advance: 2.0,
            coverage: vec![64; 4],
            is_color: false,
            is_subpixel: false,
        },
    };

    let big_key =
        GlyphKey { ch: 'B', font_slot: 0, weight_bold: false, italic: false, glyph_id: 1 };
    atlas.get_or_insert(big_key, &mut rasterizer).expect("the big glyph fits an empty atlas");
    assert_eq!(atlas.sample(3, 3), 255, "the big glyph paints the far corner of the slot");

    // Force the big glyph out and put the small one in its place.
    let small_key =
        GlyphKey { ch: 'S', font_slot: 0, weight_bold: false, italic: false, glyph_id: 2 };
    atlas.tick_frame();
    let small = atlas
        .get_or_insert(small_key, &mut rasterizer)
        .expect("the small glyph fits once the big one is evicted");
    assert!(atlas.evictions() > 0, "the run must actually evict, or it proves nothing");

    // The small glyph's own pixels are correct.
    assert_eq!(atlas.sample(0, 0), 64, "the new tile is written");

    // The margin still holds the evicted glyph. This is the defect: those
    // pixels are live texture content that nothing will overwrite until
    // another tile happens to cover them.
    assert_eq!(
        atlas.sample(3, 3),
        255,
        "the evicted glyph's ink survives in the reused slot's margin"
    );

    // The saving grace today is that the UV rect is derived from the tile,
    // not the slot, so a correct sample never reaches the margin. That is
    // what keeps this latent rather than visible, and it is worth pinning:
    // if UVs ever widen to the slot, the stale ink becomes visible ink.
    let u1 = small.uv[2];
    let v1 = small.uv[3];
    assert!(
        u1 <= 2.0 / 4.0 + f32::EPSILON,
        "UV right edge must stay within the tile, not the slot: {u1}"
    );
    assert!(
        v1 <= 2.0 / 4.0 + f32::EPSILON,
        "UV bottom edge must stay within the tile, not the slot: {v1}"
    );
}

/// An empty tile is cached with a zero-area UV whose corners are all
/// `(0.0, 0.0)`. The atlas comment says the renderer skips such a draw, and
/// this pins the property that makes that skip load-bearing: `(0,0)` is not
/// "nowhere", it is the atlas's top-left texel, which the shelf packer hands
/// to the first glyph of the session. Sampling it draws that glyph's corner.
///
/// For a block glyph the consequence is worse than a wrong shade. Block tiles
/// carry `is_color: true`, so the renderer skips the per-cell foreground and
/// paints the texture's own colour — a fully-opaque corner texel arrives as
/// pure white regardless of theme.
#[test]
fn a_zero_area_uv_points_at_the_first_packed_glyph_not_at_nothing() {
    let mut atlas = GlyphAtlas::new(64, 64);

    // The first glyph packed lands at the atlas origin.
    let mut opaque = TileRasterizer(RasterTile {
        width: 4,
        height: 4,
        offset_x: 0,
        offset_y: 0,
        advance: 4.0,
        coverage: vec![255; 16],
        is_color: false,
        is_subpixel: false,
    });
    let first = GlyphKey { ch: 'A', font_slot: 0, weight_bold: false, italic: false, glyph_id: 1 };
    let info = atlas.get_or_insert(first, &mut opaque).expect("first glyph packs");
    assert_eq!(info.uv[0], 0.0, "the first glyph is packed at the atlas origin");
    assert_eq!(info.uv[1], 0.0, "the first glyph is packed at the atlas origin");
    assert_eq!(
        atlas.sample(0, 0),
        255,
        "so texel (0,0) now holds fully-opaque ink, not transparency"
    );

    // An empty tile caches the zero-area sentinel.
    let mut empty = TileRasterizer(RasterTile {
        width: 0,
        height: 0,
        offset_x: 0,
        offset_y: 0,
        advance: 0.0,
        coverage: Vec::new(),
        is_color: true,
        is_subpixel: false,
    });
    let blank = GlyphKey { ch: ' ', font_slot: 0, weight_bold: false, italic: false, glyph_id: 2 };
    let sentinel = atlas.get_or_insert(blank, &mut empty).expect("empty tile caches a sentinel");

    assert_eq!(sentinel.uv, [0.0, 0.0, 0.0, 0.0], "empty tiles cache a zero-area UV");
    assert_eq!(sentinel.px_size, [0, 0], "and zero pixel size");
    assert!(
        sentinel.is_color,
        "a block glyph's empty tile keeps is_color, which makes the renderer paint the \
         texture's own colour rather than the cell foreground"
    );

    // The sentinel's UV corner is exactly the texel the first glyph occupies.
    // A renderer that draws this instance samples opaque ink, and for a
    // colour tile paints it as-is. The skip is what prevents that, so it has
    // to exist rather than be assumed.
    assert_eq!(
        atlas.sample((sentinel.uv[0] * 64.0) as u32, (sentinel.uv[1] * 64.0) as u32),
        255,
        "the zero-area UV addresses opaque ink, so emitting the draw is not harmless"
    );
}
