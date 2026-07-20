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
fn non_evicting_insert_preserves_resident_tiles_when_full() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut rasterizer = OnePixelRasterizer;
    let first = GlyphKey::new('a', false, false);
    atlas.get_or_insert(first, &mut rasterizer).expect("first tile fills atlas");
    let epoch = atlas.evictions();

    let second = atlas.get_or_insert_without_eviction(
        GlyphKey::new('b', false, false),
        &mut rasterizer,
    );

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

    let second = atlas.get_or_insert_lazy_without_eviction(
        GlyphKey::new('b', false, false),
        1,
        1,
        || {
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
        },
    );

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

    let invalid = atlas.get_or_insert_lazy_without_eviction(
        GlyphKey::new('b', false, false),
        1,
        1,
        || RasterTile {
            width: 2,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![255; 2],
            is_color: false,
            is_subpixel: false,
        },
    );
    assert!(invalid.is_none(), "mismatched lazy tile must be rejected");

    let replacement = atlas.get_or_insert_lazy_without_eviction(
        GlyphKey::new('c', false, false),
        2,
        2,
        || RasterTile {
            width: 2,
            height: 2,
            offset_x: 0,
            offset_y: 0,
            advance: 2.0,
            coverage: vec![255; 4],
            is_color: false,
            is_subpixel: false,
        },
    );
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
        atlas.get_or_insert_lazy_without_eviction(
            GlyphKey::new(ch, false, false),
            1,
            1,
            || RasterTile {
                width: 1,
                height: 1,
                offset_x: 0,
                offset_y: 0,
                advance: 1.0,
                coverage: vec![255],
                is_color: false,
                is_subpixel: false,
            },
        );
    }

    assert!(atlas.len() <= MAX_ATLAS_ENTRIES);
}
