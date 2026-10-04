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

/// Subpixel tiles keep their four linear coverage channels and never enter color conversion.
#[test]
fn subpixel_text_coverage_copies_bgra_channels() {
    let mut atlas = GlyphAtlas::new(4, 4);
    let mut rasterizer = SubpixelRasterizer;
    let info = atlas
        .get_or_insert(
            GlyphKey {
                ch: 'V',
                font_slot: 0,
                weight_bold: false,
                italic: false,
                glyph_id: 1,
                raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
            },
            &mut rasterizer,
        )
        .expect("subpixel tile inserts");

    assert!(info.is_subpixel);
    assert!(!info.is_color);
    assert_eq!(&atlas.pixels_bgra()[0..4], &[10, 20, 30, 40]);
    assert_eq!(
        atlas.take_dirty_rects(),
        [DirtyRect { x: 0, y: 0, w: 1, h: 1, kind: AtlasPixelKind::Coverage }]
    );
}

/// Color glyph insertion preserves its premultiplied sRGB bytes and identifies only that write for conversion.
#[test]
fn color_tile_preserves_cpu_bytes_and_marks_color_dirty() {
    let encoded_bgra = [17, 34, 68, 128];
    let mut atlas = GlyphAtlas::new(1, 1);

    atlas
        .get_or_insert(
            GlyphKey::new('😀', false, false),
            &mut TileRasterizer(RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: encoded_bgra.to_vec(),
                is_color: true,
                is_subpixel: false,
            }),
        )
        .expect("color tile inserts");

    assert_eq!(atlas.pixels_bgra(), encoded_bgra);
    assert_eq!(
        atlas.take_dirty_rects(),
        [DirtyRect { x: 0, y: 0, w: 1, h: 1, kind: AtlasPixelKind::Color }]
    );
}

/// Reusing an undrained atlas slot must discard the stale pixel interpretation in either direction.
#[test]
fn replacement_kind_supersedes_stale_overlapping_dirty_record() {
    fn assert_replacement(first_color: bool, replacement_color: bool) {
        let mut atlas = GlyphAtlas::new(2, 2);
        let tile = |side, is_color, value| RasterTile {
            width: side,
            height: side,
            offset_x: 0,
            offset_y: 0,
            advance: side as f32,
            coverage: if is_color {
                vec![value; (side * side * 4) as usize]
            } else {
                vec![value; (side * side) as usize]
            },
            is_color,
            is_subpixel: false,
        };
        atlas
            .get_or_insert(
                GlyphKey::new('a', false, false),
                &mut TileRasterizer(tile(2, first_color, 32)),
            )
            .expect("first tile fills atlas");
        atlas
            .get_or_insert(
                GlyphKey::new('b', false, false),
                &mut TileRasterizer(tile(1, replacement_color, 96)),
            )
            .expect("smaller replacement reuses evicted slot");

        let expected_kind =
            if replacement_color { AtlasPixelKind::Color } else { AtlasPixelKind::Coverage };
        assert_eq!(&atlas.pixels_bgra()[0..4], &[96; 4]);
        assert_eq!(
            atlas.take_dirty_rects(),
            [DirtyRect { x: 0, y: 0, w: 1, h: 1, kind: expected_kind }],
            "the smaller newest write must remove the larger stale interpretation"
        );
    }

    assert_replacement(true, false);
    assert_replacement(false, true);
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
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
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
    // A governor charges the atlas for what it actually holds: the pixel buffer plus the pending
    // dirty list's capacity. The pixels are allocated up front at full size and a fixed atlas's
    // pixels never grow with use; items are resident entries, which is what eviction acts on.
    let mut atlas = GlyphAtlas::new(64, 64);
    let empty = atlas.retained_amount();
    assert_eq!(atlas.retained_pixel_bytes(), 64 * 64 * 4, "the pixel buffer is allocated up front");
    assert_eq!(
        empty.bytes,
        atlas.retained_pixel_bytes() + atlas.dirty_capacity() * std::mem::size_of::<DirtyRect>()
    );
    assert_eq!(empty.items, 0, "a fresh atlas holds no entries");

    let mut raster = OnePixelRasterizer;
    let key = |n: u32| GlyphKey {
        ch: char::from_u32(n).unwrap_or('a'),
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: n,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    };
    for n in 33..43u32 {
        atlas.get_or_insert(key(n), &mut raster);
    }

    let filled = atlas.retained_amount();
    assert_eq!(
        atlas.retained_pixel_bytes(),
        64 * 64 * 4,
        "inserting glyphs does not grow the pixels"
    );
    assert_eq!(
        filled.bytes,
        atlas.retained_pixel_bytes() + atlas.dirty_capacity() * std::mem::size_of::<DirtyRect>(),
        "bytes are the pixels plus the dirty list's capacity"
    );
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
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
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

    let big_key = GlyphKey {
        ch: 'B',
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: 1,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    };
    atlas.get_or_insert(big_key, &mut rasterizer).expect("the big glyph fits an empty atlas");
    assert_eq!(atlas.sample(3, 3), 255, "the big glyph paints the far corner of the slot");

    // Force the big glyph out and put the small one in its place.
    let small_key = GlyphKey {
        ch: 'S',
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: 2,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    };
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
    let first = GlyphKey {
        ch: 'A',
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: 1,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    };
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
    let blank = GlyphKey {
        ch: ' ',
        font_slot: 0,
        weight_bold: false,
        italic: false,
        glyph_id: 2,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    };
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

/// Answers each key from a fixed table: `None` is an unresolved glyph, a zero-area tile is a
/// valid empty glyph such as a space. Counts its calls.
struct TableRasterizer {
    tiles: std::collections::HashMap<char, Option<RasterTile>>,
    calls: usize,
}

impl Rasterizer for TableRasterizer {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        self.calls += 1;
        self.tiles.get(&key.ch).cloned().flatten()
    }
}

fn table_tile(width: u32, height: u32) -> RasterTile {
    RasterTile {
        width,
        height,
        offset_x: 1,
        offset_y: -2,
        advance: width as f32,
        coverage: vec![255; (width * height) as usize],
        is_color: false,
        is_subpixel: false,
    }
}

#[test]
fn an_unresolved_glyph_is_cached_missing_and_an_empty_glyph_is_not() {
    // `None` from the rasterizer caches the miss sentinel marked missing, so the renderer draws
    // tofu; an empty tile, such as a space, keeps `missing` false and is skipped, never tofu.
    let mut rasterizer = TableRasterizer {
        tiles: [('x', None), (' ', Some(table_tile(0, 0)))].into_iter().collect(),
        calls: 0,
    };
    let mut atlas = GlyphAtlas::new(64, 64);
    let unresolved =
        atlas.get_or_insert(GlyphKey::new('x', false, false), &mut rasterizer).unwrap();
    assert!(unresolved.missing);
    assert_eq!(unresolved.px_size, [0, 0]);
    let space = atlas.get_or_insert(GlyphKey::new(' ', false, false), &mut rasterizer).unwrap();
    assert!(!space.missing);
    assert_eq!((space.px_size, space.px_offset), ([0, 0], [1, -2]));
}

#[test]
fn forget_missing_drops_only_missing_entries_so_they_rasterize_again() {
    // Once a fallback face is published the renderer forgets the missing sentinels: they rasterize
    // again on the next lookup, while resident tiles keep their UVs and are not rasterized again.
    let mut rasterizer = TableRasterizer {
        tiles: [('x', None), ('a', Some(table_tile(4, 6))), (' ', Some(table_tile(0, 0)))]
            .into_iter()
            .collect(),
        calls: 0,
    };
    let mut atlas = GlyphAtlas::new(64, 64);
    let keys = ['x', 'a', ' '].map(|character| GlyphKey::new(character, false, false));
    let before: Vec<_> =
        keys.iter().map(|key| atlas.get_or_insert(*key, &mut rasterizer).unwrap()).collect();
    assert_eq!(rasterizer.calls, 3);
    atlas.forget_missing();
    rasterizer.tiles.insert('x', Some(table_tile(5, 7)));
    let after: Vec<_> =
        keys.iter().map(|key| atlas.get_or_insert(*key, &mut rasterizer).unwrap()).collect();
    assert_eq!(rasterizer.calls, 4, "only the missing glyph is rasterized again");
    assert!(!after[0].missing && after[0].px_size == [5, 7], "the real glyph replaces tofu");
    assert_eq!(&after[1..], &before[1..], "other entries and their UVs are unchanged");
}

/// Answers every key with one `width × height` tile, coverage or color, so a test controls packing.
struct ShapedRasterizer {
    width: u32,
    height: u32,
    is_color: bool,
}

impl Rasterizer for ShapedRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        let bytes_per_pixel = if self.is_color { 4 } else { 1 };
        Some(RasterTile {
            width: self.width,
            height: self.height,
            offset_x: 0,
            offset_y: 0,
            advance: self.width as f32,
            coverage: vec![200; (self.width * self.height * bytes_per_pixel) as usize],
            is_color: self.is_color,
            is_subpixel: false,
        })
    }
}

/// A distinct key per `index`, drawn from the CJK block so thousands stay valid chars.
fn numbered_key(index: u32) -> GlyphKey {
    GlyphKey::new(char::from_u32(0x4E00 + index).expect("CJK char"), false, false)
}

/// Bytes the governor should see for `atlas`: its pixels plus its dirty list's capacity.
fn expected_retained_bytes(atlas: &GlyphAtlas) -> usize {
    atlas.retained_pixel_bytes() + atlas.dirty_capacity() * std::mem::size_of::<DirtyRect>()
}

/// A growable atlas starts square at its start size and charges only that size's pixels.
#[test]
fn a_growable_atlas_starts_at_its_start_size() {
    for start in [MIN_ATLAS_DIM, START_ATLAS_DIM_1X, START_ATLAS_DIM_2X] {
        let atlas = GlyphAtlas::growable(start, ATLAS_DIM);
        assert_eq!((atlas.width(), atlas.height()), (start, start));
        assert_eq!(atlas.retained_pixel_bytes(), (start * start * 4) as usize);
        assert_eq!(atlas.retained_amount().bytes, expected_retained_bytes(&atlas));
        assert_eq!(atlas.growth_policy(), GrowthPolicy::Growable { max: ATLAS_DIM });
        assert_eq!(atlas.growths(), 0);
    }
}

/// The fit rule takes the smallest candidate that places every tile and leaves a quarter free.
#[test]
fn fit_outcome_needs_every_tile_placed_and_a_quarter_of_the_height_free() {
    // Three 129-wide tiles need 387 px of shelf: 256 cannot hold the row, 512 holds it at 100 px.
    assert_eq!(fit_outcome_of_tiles(&[[129, 100]; 3]), FitOutcome::Fits(512));
    // Four 500×100 tiles place at 512 one per shelf, using 400 px, above 75% of 512 (384), so the
    // smallest qualifying start is 1024, where two shelves use 200 px.
    assert_eq!(fit_outcome_of_tiles(&[[500, 100]; 4]), FitOutcome::Fits(1024));
    // One 64×1600 tile places only at 2048 and there uses 78% of the height.
    assert_eq!(fit_outcome_of_tiles(&[[64, 1600]]), FitOutcome::FitsWithoutHeadroom);
    // Two 1100×1100 tiles need two shelves totalling 2200 px, more than 2048.
    assert_eq!(fit_outcome_of_tiles(&[[1100, 1100]; 2]), FitOutcome::DoesNotFit);
    assert_eq!(fit_outcome_of_tiles(&[]), FitOutcome::Fits(256), "an empty set fits the floor");
    for (outcome, label) in [
        (FitOutcome::Fits(512), "512"),
        (FitOutcome::FitsWithoutHeadroom, "no_headroom"),
        (FitOutcome::DoesNotFit, "does_not_fit"),
        (FitOutcome::Evicted, "evicted"),
    ] {
        assert_eq!(outcome.label(), label);
    }
}

/// Growth copies every tile to the same pixel position, recomputes UVs from the tile size,
/// advances identity and the growth count, and replaces the dirty list with one rect per resident
/// tile typed by its own pixel kind. Nothing is rasterized again.
#[test]
fn growth_keeps_positions_and_pixels_and_queues_a_typed_reupload() {
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    let mono = atlas
        .get_or_insert(
            numbered_key(0),
            &mut ShapedRasterizer { width: 10, height: 12, is_color: false },
        )
        .unwrap();
    let color = atlas
        .get_or_insert(
            numbered_key(1),
            &mut ShapedRasterizer { width: 8, height: 8, is_color: true },
        )
        .unwrap();
    let mut subpixel = SubpixelRasterizer;
    let lcd = atlas.get_or_insert(numbered_key(2), &mut subpixel).unwrap();
    let mut drained = Vec::new();
    atlas.drain_dirty_rects_into(&mut drained);
    let before_pixels = atlas.pixels().to_vec();
    let identity_before = atlas.identity();
    let mut counting = SyntheticRasterizer::default();

    assert!(atlas.grow_to(512), "the next doubling is allowed");

    assert_eq!((atlas.width(), atlas.height(), atlas.growths()), (512, 512, 1));
    assert!(atlas.identity() > identity_before, "every UV-bearing cache must rebuild");
    for (key, before) in [(numbered_key(0), mono), (numbered_key(1), color), (numbered_key(2), lcd)]
    {
        let after = atlas.get(key).unwrap();
        let x_px = (before.uv[0] * 256.0).round() as u32;
        let y_px = (before.uv[1] * 256.0).round() as u32;
        let expected = [
            x_px as f32 / 512.0,
            y_px as f32 / 512.0,
            (x_px + before.px_size[0]) as f32 / 512.0,
            (y_px + before.px_size[1]) as f32 / 512.0,
        ];
        assert_eq!(after.uv, expected, "{key:?} keeps its pixel position");
        assert_eq!(after.px_size, before.px_size);
        for row in 0..before.px_size[1] {
            let old_start = (((y_px + row) * 256 + x_px) * 4) as usize;
            let new_start = (((y_px + row) * 512 + x_px) * 4) as usize;
            let len = (before.px_size[0] * 4) as usize;
            assert_eq!(
                atlas.pixels()[new_start..new_start + len],
                before_pixels[old_start..old_start + len],
                "{key:?} row {row} copied byte for byte"
            );
        }
    }
    let mut reupload = Vec::new();
    atlas.drain_dirty_rects_into(&mut reupload);
    let kinds: Vec<(u32, u32, AtlasPixelKind)> =
        reupload.iter().map(|rect| (rect.w, rect.h, rect.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            (10, 12, AtlasPixelKind::Coverage),
            (8, 8, AtlasPixelKind::Color),
            (1, 1, AtlasPixelKind::Coverage)
        ],
        "one rect per tile at its tile size; subpixel coverage stays Coverage"
    );
    assert_eq!(counting.calls, 0);
    let _ = atlas.get_or_insert(numbered_key(0), &mut counting);
    assert_eq!(counting.calls, 0, "a grown atlas still hits without rasterizing");
}

/// A tile larger than the current size but within the maximum grows the atlas until it fits;
/// only a tile beyond the maximum becomes a sentinel, and it causes no growth.
#[test]
fn a_tile_up_to_the_maximum_grows_the_atlas_and_a_larger_one_is_a_sentinel() {
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    let large = atlas
        .get_or_insert(
            numbered_key(0),
            &mut ShapedRasterizer { width: 600, height: 300, is_color: false },
        )
        .unwrap();
    assert_eq!(large.px_size, [600, 300]);
    assert_eq!((atlas.width(), atlas.growths()), (1024, 2), "256 -> 512 -> 1024 holds 600 px");
    let too_large = atlas
        .get_or_insert(
            numbered_key(1),
            &mut ShapedRasterizer { width: 2049, height: 8, is_color: false },
        )
        .unwrap();
    assert_eq!(too_large.px_size, [0, 0], "beyond the maximum is a sentinel");
    assert!(!too_large.missing);
    assert_eq!((atlas.width(), atlas.growths()), (1024, 2), "an impossible tile grows nothing");
}

/// Below the maximum a packing failure grows and evicts nothing; at the maximum it evicts the
/// coldest quarter; the entry cap evicts at any size; growth works with eviction disabled.
#[test]
fn eviction_happens_only_at_the_maximum_or_the_entry_cap() {
    let mut quarter = ShapedRasterizer { width: 1024, height: 1024, is_color: false };
    let mut growing = GlyphAtlas::growable(1024, ATLAS_DIM);
    for index in 0..4 {
        growing.get_or_insert(numbered_key(index), &mut quarter).unwrap();
    }
    assert_eq!((growing.width(), growing.growths(), growing.evictions()), (2048, 1, 0));
    growing.tick_frame();
    growing.get_or_insert(numbered_key(4), &mut quarter).unwrap();
    assert_eq!((growing.width(), growing.evictions()), (2048, 1), "the maximum evicts a quarter");

    let mut capped = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    let mut missing = MissingRasterizer;
    for index in 0..=MAX_ATLAS_ENTRIES as u32 {
        capped.get_or_insert(numbered_key(index), &mut missing);
    }
    assert!(capped.evictions() > 0, "the entry cap evicts below the maximum");
    assert_eq!((capped.width(), capped.growths()), (MIN_ATLAS_DIM, 0));

    let mut barred = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    barred.set_eviction_enabled(false);
    let placed = barred
        .get_or_insert(
            numbered_key(0),
            &mut ShapedRasterizer { width: 300, height: 300, is_color: false },
        )
        .unwrap();
    assert_eq!(placed.px_size, [300, 300], "growth moves no tile, so it needs no eviction");
    assert_eq!((barred.width(), barred.growths(), barred.evictions()), (512, 1, 0));
}

/// Fixed atlases, square or not and down to one pixel wide, evict instead of growing: their
/// size, pixel bytes and growth count stay put, and a drained dirty list returns to 64 rects.
#[test]
fn fixed_atlases_never_grow() {
    let fixed = [
        GlyphAtlas::new(256, 256),
        GlyphAtlas::new(64, 64),
        GlyphAtlas::new(1, 30),
        GlyphAtlas::new(30, 1),
        GlyphAtlas::new(64, 512),
        GlyphAtlas::new(512, 64),
        GlyphAtlas::default_size(),
    ];
    for mut atlas in fixed {
        let (width, height) = (atlas.width(), atlas.height());
        let pixel_bytes = atlas.retained_pixel_bytes();
        assert_eq!(atlas.growth_policy(), GrowthPolicy::Fixed);
        // The 2048 atlas uses 32 px tiles so its 4,100 inserts take the dirty list past the
        // 1,024-rect shrink threshold; the smaller atlases never reach 64 pending rects.
        let side = if width >= ATLAS_DIM { 32 } else { width.min(height).min(64) };
        let capacity = (width / side) * (height / side);
        let mut rasterizer = ShapedRasterizer { width: side, height: side, is_color: false };
        for index in 0..capacity + 4 {
            atlas.tick_frame();
            atlas.get_or_insert(numbered_key(index), &mut rasterizer);
        }
        assert!(atlas.evictions() > 0, "{width}x{height} must evict once full");
        assert_eq!((atlas.width(), atlas.height(), atlas.growths()), (width, height, 0));
        assert_eq!(atlas.retained_pixel_bytes(), pixel_bytes, "{width}x{height} pixels never grow");
        let mut drained = Vec::new();
        atlas.drain_dirty_rects_into(&mut drained);
        assert!(atlas.dirty_capacity() <= DIRTY_LIST_RETAINED, "{width}x{height} dirty list");
    }
}

/// A slot reused by a smaller tile, then grown: the UV and the re-upload cover only the tile, so
/// the evicted tile's pixels left in the slot margin are never sampled or uploaded.
#[test]
fn growth_after_slot_reuse_covers_only_the_new_tile() {
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    atlas.get_or_insert(
        numbered_key(0),
        &mut ShapedRasterizer { width: 40, height: 40, is_color: false },
    );
    atlas.evict_lru_quartile();
    let reused = atlas
        .get_or_insert(
            numbered_key(1),
            &mut ShapedRasterizer { width: 10, height: 12, is_color: false },
        )
        .unwrap();
    assert_eq!(reused.uv[0..2], [0.0, 0.0], "the 10×12 tile reuses the freed 40×40 slot");
    assert_eq!(atlas.sample(20, 20), 200, "precondition: the evicted tile's pixel is still there");

    assert!(atlas.grow_to(512), "the next doubling is allowed");

    let grown = atlas.get(numbered_key(1)).unwrap();
    assert_eq!(grown.uv, [0.0, 0.0, 10.0 / 512.0, 12.0 / 512.0]);
    let mut reupload = Vec::new();
    atlas.drain_dirty_rects_into(&mut reupload);
    assert_eq!(
        reupload,
        vec![DirtyRect { x: 0, y: 0, w: 10, h: 12, kind: AtlasPixelKind::Coverage }]
    );
    let uploaded_bytes: u32 = reupload.iter().map(|rect| rect.w * rect.h * 4).sum();
    assert_eq!(uploaded_bytes, 10 * 12 * 4);
    let inside = |rect: &DirtyRect| {
        (rect.x..rect.x + rect.w).contains(&20) && (rect.y..rect.y + rect.h).contains(&20)
    };
    assert!(!reupload.iter().any(inside), "the stale pixel is never uploaded");
    assert!(grown.uv[2] * 512.0 <= 20.0, "the stale pixel lies outside the UV");
    assert_eq!(atlas.fit_outcome(), FitOutcome::Evicted, "an evicted atlas reports evicted");
}

/// The resident replay classifies the atlas's own tiles, and only tiles with pixels count.
#[test]
fn fit_outcome_replays_resident_tiles() {
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    let mut rasterizer = ShapedRasterizer { width: 129, height: 100, is_color: false };
    for index in 0..3 {
        atlas.get_or_insert(numbered_key(index), &mut rasterizer);
    }
    atlas.get_or_insert(numbered_key(3), &mut MissingRasterizer);
    assert_eq!(atlas.fit_outcome(), FitOutcome::Fits(512));
    assert_eq!(atlas.packed_pixels(), 3 * 129 * 100);
    assert_eq!(atlas.max_tile_dims(), [129, 100]);
    assert_eq!(atlas.resident_tile_keys().len(), 3, "sentinels own no pixels");
}

/// After growth to 5,000 entries, the governor sees the dirty list's capacity, and one drain
/// returns that list to 64 rects so the figure falls back to the pixels alone.
#[test]
fn a_large_reupload_is_counted_and_released_by_the_drain() {
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    let mut rasterizer = ShapedRasterizer { width: 8, height: 8, is_color: false };
    for index in 0..5000 {
        atlas.get_or_insert(numbered_key(index), &mut rasterizer);
    }
    assert!(atlas.growths() > 0, "5,000 tiles of 64 px outgrow 256");
    let dim = atlas.width() as usize;
    let before = atlas.retained_amount().bytes;
    assert!(atlas.dirty_capacity() >= 5000);
    assert_eq!(before, dim * dim * 4 + atlas.dirty_capacity() * std::mem::size_of::<DirtyRect>());
    let mut drained = Vec::new();
    atlas.drain_dirty_rects_into(&mut drained);
    assert_eq!(drained.len(), 5000);
    assert!(atlas.dirty_capacity() <= DIRTY_LIST_RETAINED);
    assert!(atlas.retained_amount().bytes < before, "the reported figure falls after the drain");
}

/// Everything a growth would change, so a rejected call can be shown to change none of it.
/// The pixels enter as a length and a byte sum, so a failure prints a short tuple.
fn growth_snapshot(atlas: &GlyphAtlas) -> (u32, u32, u64, u64, usize, u64, usize) {
    let pixel_sum = atlas.pixels().iter().map(|byte| u64::from(*byte)).sum();
    (
        atlas.width(),
        atlas.height(),
        atlas.identity(),
        atlas.growths(),
        atlas.pixels().len(),
        pixel_sum,
        atlas.dirty_capacity(),
    )
}

/// `grow_to` honors the growth policy: a fixed atlas never grows, and a growable one moves only
/// by one doubling at or below its maximum. Every rejected size leaves the atlas untouched.
#[test]
fn grow_to_rejects_sizes_the_growth_policy_forbids_without_mutating() {
    let mut fixed = GlyphAtlas::new(MIN_ATLAS_DIM, MIN_ATLAS_DIM);
    let before = growth_snapshot(&fixed);
    assert!(!fixed.grow_to(MIN_ATLAS_DIM * 2), "a fixed atlas refuses growth");
    assert_eq!(growth_snapshot(&fixed), before, "a fixed atlas never grows");

    let mut growable = GlyphAtlas::growable(MIN_ATLAS_DIM, 512);
    growable.get_or_insert(
        numbered_key(0),
        &mut ShapedRasterizer { width: 10, height: 12, is_color: false },
    );
    // Skipping a doubling, a size that is not a power of two, the current size and a smaller one.
    for rejected in [MIN_ATLAS_DIM * 4, MIN_ATLAS_DIM + 44, MIN_ATLAS_DIM, MIN_ATLAS_DIM / 2] {
        let before = growth_snapshot(&growable);
        assert!(!growable.grow_to(rejected), "{rejected} is refused");
        assert_eq!(growth_snapshot(&growable), before, "{rejected} is not one doubling");
    }
    // The one allowed size is the next doubling; at the maximum nothing further is allowed.
    assert!(growable.grow_to(MIN_ATLAS_DIM * 2), "the next doubling is allowed");
    assert_eq!((growable.width(), growable.growths()), (512, 1));
    let at_max = growth_snapshot(&growable);
    assert!(!growable.grow_to(1024), "the maximum is never exceeded");
    assert_eq!(growth_snapshot(&growable), at_max, "a refusal at the maximum changes nothing");
}

/// The test-only entry cap moves the eviction threshold and nothing else: a growable atlas with a
/// cap of 4 evicts its coldest quarter at the fifth key while it still has room to pack, and the
/// cap survives a reset in place so a retry runs under the same threshold.
#[test]
fn a_lowered_entry_cap_evicts_before_packing_runs_out() {
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    atlas.__set_entry_cap_for_test(4);
    let mut rasterizer = ShapedRasterizer { width: 8, height: 8, is_color: false };
    for index in 0..4 {
        atlas.tick_frame();
        atlas.get_or_insert(numbered_key(index), &mut rasterizer);
    }
    assert_eq!((atlas.len(), atlas.evictions()), (4, 0), "four keys fit under the cap");
    atlas.tick_frame();
    atlas.get_or_insert(numbered_key(4), &mut rasterizer);
    assert!(atlas.evictions() > 0, "the fifth key evicts at the cap");
    assert!(atlas.len() <= 4, "the index stays at or below the cap");
    assert_eq!(atlas.growths(), 0, "the eviction is not a packing failure");
    atlas.reset_in_place();
    for index in 0..5 {
        atlas.tick_frame();
        atlas.get_or_insert(numbered_key(index), &mut rasterizer);
    }
    assert!(atlas.evictions() > 0, "the cap survives a reset in place");
}
