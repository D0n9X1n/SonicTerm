//! Growth preserves resident pixels, tile identity, and bounded retained storage.

use super::super::test_support::*;
use super::*;

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
    // Only the impossible tile is marked oversize, so callers report it instead of skipping it.
    assert!(too_large.oversize, "the sentinel is marked oversize");
    assert!(!large.oversize, "a placed tile is not oversize");
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

/// The eviction getter reports the configured permission: on by default, and whatever
/// `set_eviction_enabled` last set.
#[test]
fn eviction_enabled_reports_the_configured_permission() {
    let mut atlas = GlyphAtlas::new(64, 64);
    assert!(atlas.eviction_enabled(), "a new atlas may evict");
    atlas.set_eviction_enabled(false);
    assert!(!atlas.eviction_enabled());
    atlas.set_eviction_enabled(true);
    assert!(atlas.eviction_enabled());
}
