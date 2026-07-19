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
