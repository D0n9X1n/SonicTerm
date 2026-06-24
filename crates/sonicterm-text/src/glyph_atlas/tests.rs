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

