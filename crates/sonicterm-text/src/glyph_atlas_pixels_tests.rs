//! The borrowed-pixel color insert writes, reports and refuses exactly as the owned-tile path.

use super::*;

/// `width` x `height` BGRA pixels whose bytes differ by position, so a misplaced row shows.
fn bgra(width: u32, height: u32) -> Vec<u8> {
    (0..width * height * 4).map(|byte| (byte * 7 % 251) as u8).collect()
}

/// The color tile the lazy path builds for an image of `pixels`.
fn color_tile(width: u32, height: u32, pixels: &[u8]) -> RasterTile {
    RasterTile {
        width,
        height,
        offset_x: 0,
        offset_y: 0,
        advance: width as f32,
        coverage: pixels.to_vec(),
        is_color: true,
        is_subpixel: false,
    }
}

/// The image key the renderer uses, by `id`.
fn image_key(id: u32) -> GlyphKey {
    GlyphKey {
        ch: '\u{fffc}',
        font_slot: 0xFE,
        weight_bold: false,
        italic: false,
        glyph_id: id,
        raster_variant: sonicterm_types::glyph_key::GlyphRasterVariant::Normal,
    }
}

#[test]
fn borrowed_pixels_insert_as_the_owned_tile_does() {
    // Contract: for a one-row, an odd-width and a 1024x1024 image, the borrowed insert leaves the
    // same atlas pixels, the same GlyphInfo and the same dirty rects as the RasterTile path, and a
    // second lookup is a cache hit that touches no pixels.
    for (width, height) in [(5, 1), (7, 3), (1024, 1024)] {
        let pixels = bgra(width, height);
        let mut owned = GlyphAtlas::new(2048, 2048);
        let mut borrowed = GlyphAtlas::new(2048, 2048);
        // A text tile first, so the image does not start at the origin.
        for atlas in [&mut owned, &mut borrowed] {
            atlas.get_or_insert_lazy_without_eviction(
                GlyphKey::new('a', false, false),
                3,
                2,
                || RasterTile { coverage: vec![9; 6], is_color: false, ..color_tile(3, 2, &[]) },
            );
        }
        let expected = owned
            .get_or_insert_lazy_without_eviction(image_key(1), width, height, || {
                color_tile(width, height, &pixels)
            })
            .expect("owned tile fits");
        let actual = borrowed
            .get_or_insert_color_pixels_without_eviction(image_key(1), width, height, &pixels)
            .expect("borrowed pixels fit");
        assert_eq!(actual, expected, "{width}x{height}: same GlyphInfo");
        assert!(borrowed.pixels() == owned.pixels(), "{width}x{height}: same atlas pixels");
        assert_eq!(borrowed.take_dirty_rects(), owned.take_dirty_rects(), "{width}x{height}");
        let misses = borrowed.misses();
        let hit =
            borrowed.get_or_insert_color_pixels_without_eviction(image_key(1), width, height, &[]);
        assert_eq!(hit, Some(expected), "a resident key answers from the cache");
        assert_eq!(borrowed.misses(), misses, "a hit is not a miss");
        assert!(borrowed.take_dirty_rects().is_empty(), "a hit writes nothing");
    }
}

#[test]
fn a_malformed_length_is_refused_before_a_slot_is_reserved() {
    // Contract: a pixel slice one byte short or long is refused without reading it or reserving
    // the slot, so the whole atlas is still free for a well-formed image of the full size.
    for wrong_len in [4 * 4 * 4 - 1, 4 * 4 * 4 + 1, 0] {
        let mut atlas = GlyphAtlas::new(4, 4);
        let refused = atlas.get_or_insert_color_pixels_without_eviction(
            image_key(1),
            4,
            4,
            &vec![1; wrong_len],
        );
        assert_eq!(refused, None, "{wrong_len} bytes for a 4x4 image");
        assert!(atlas.take_dirty_rects().is_empty(), "nothing was written");
        let fits =
            atlas.get_or_insert_color_pixels_without_eviction(image_key(2), 4, 4, &bgra(4, 4));
        assert!(fits.is_some(), "the refused request reserved nothing");
    }
}

#[test]
fn a_full_atlas_refuses_without_evicting() {
    // Contract: like the lazy path, the borrowed insert never evicts: a full atlas refuses the
    // new image and keeps the resident one.
    let mut atlas = GlyphAtlas::new(4, 4);
    let first = atlas
        .get_or_insert_color_pixels_without_eviction(image_key(1), 4, 4, &bgra(4, 4))
        .expect("the first image fills the atlas");
    let refused =
        atlas.get_or_insert_color_pixels_without_eviction(image_key(2), 2, 2, &bgra(2, 2));
    assert_eq!(refused, None);
    assert_eq!(atlas.evictions(), 0);
    assert_eq!(
        atlas.get_or_insert_color_pixels_without_eviction(image_key(1), 4, 4, &[]),
        Some(first),
        "the resident image is kept"
    );
}

#[test]
fn a_reused_slot_supersedes_its_previous_dirty_rect() {
    // Contract: an image written into a reclaimed slot drops the slot's pending coverage rect, so
    // the upload reads the new bytes as color, exactly as the owned-tile path does.
    let mut atlas = GlyphAtlas::new(2, 2);
    atlas.get_or_insert_lazy_without_eviction(GlyphKey::new('a', false, false), 2, 2, || {
        RasterTile { coverage: vec![255; 4], is_color: false, ..color_tile(2, 2, &[]) }
    });
    atlas.evict_lru_quartile();
    atlas
        .get_or_insert_color_pixels_without_eviction(image_key(1), 2, 2, &bgra(2, 2))
        .expect("the reclaimed slot holds the image");
    let dirty = atlas.take_dirty_rects();
    assert_eq!(dirty.len(), 1, "only the image's rect is pending: {dirty:?}");
    assert_eq!(dirty[0].kind, AtlasPixelKind::Color);
    assert_eq!(&atlas.pixels()[..8], &bgra(2, 2)[..8], "the image's first row is in place");
}
