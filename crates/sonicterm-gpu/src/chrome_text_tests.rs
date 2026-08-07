use sonicterm_text::glyph_atlas::GlyphAtlas;

use super::*;

#[test]
fn native_raster_roles_use_distinct_tiles_without_projection_scaling() {
    // Contract: each chrome role has a distinct atlas tile drawn at its native raster size.
    let _font_lock = TRACKED_FONT_STACK_LOCK.lock().expect("font fixture lock");
    let mut atlas = GlyphAtlas::new(512, 512);
    let screen = (800.0, 100.0);
    let cases = [
        (12.0, GlyphRasterVariant::PaletteFooter),
        (13.0, GlyphRasterVariant::Normal),
        (14.0, GlyphRasterVariant::TabTitle),
    ];
    let mut tile_sizes = Vec::new();

    for (size, variant) in cases {
        let stack = tracked_font_stack(size);
        let shaped = stack.shape_text("M").expect("tracked font shapes M");
        let glyph = shaped.iter().find(|glyph| glyph.glyph_pos != 0).expect("tracked font has M");
        let key = GlyphKey::shaped(
            'M',
            u8::try_from(glyph.font_idx).expect("fixture font index fits"),
            glyph.glyph_pos,
            false,
            false,
        )
        .with_raster_variant(variant);
        let mut rasterizer = stack.clone();
        let layout = layout_with_raster_variant(
            &stack,
            &mut rasterizer,
            &mut atlas,
            "M",
            ChromeColor::WHITE,
            ChromeAttrs::default(),
            size as f32,
            size as f32,
            (0.0, 30.0),
            screen,
            None,
            variant,
        );
        let info = atlas.get(key).expect("role-specific tile was inserted");
        let instance = layout.glyphs.first().expect("M emits one visible glyph");
        let draw_size =
            [(instance.rect[2] * screen.0 * 0.5).abs(), (instance.rect[3] * screen.1 * 0.5).abs()];

        assert_eq!(draw_size[0].to_bits(), (info.px_size[0] as f32).to_bits());
        assert_eq!(draw_size[1].to_bits(), (info.px_size[1] as f32).to_bits());
        tile_sizes.push(info.px_size);
    }

    assert_eq!(atlas.len(), 3);
    assert_ne!(tile_sizes[0], tile_sizes[1]);
    assert_ne!(tile_sizes[1], tile_sizes[2]);
}
