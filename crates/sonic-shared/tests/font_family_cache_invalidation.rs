use sonic_core::grid::{Cell, Color};
use sonic_shared::{
    row_glyph_cache::{CachedRow, RowGlyphCache},
    shape::{RunStyle, ShapeCache},
    swash_rasterizer::{load_bundled_fonts, SwashRasterizer, DEFAULT_RASTER_PX},
};

#[test]
fn font_family_change_uses_distinct_shape_key_and_clears_row_cache() {
    let mut fs = cosmic_text::FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut rasterizer = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let style = RunStyle { bold: false, italic: false };
    let cells = [(
        0u16,
        Cell {
            ch: 'W',
            fg: Color::Default,
            bg: Color::Default,
            flags: Default::default(),
            hyperlink: None,
            extras: None,
        },
    )];

    let mut shape_cache = ShapeCache::new();
    let _ = shape_cache.get_or_shape(
        &mut rasterizer,
        "Rec Mono St.Helens",
        DEFAULT_RASTER_PX,
        style,
        &cells,
    );
    assert_eq!(shape_cache.misses(), 1);
    let _ = shape_cache.get_or_shape(
        &mut rasterizer,
        "JetBrainsMono Nerd Font",
        DEFAULT_RASTER_PX,
        style,
        &cells,
    );
    assert_eq!(
        shape_cache.misses(),
        2,
        "font.family must be part of the shape cache key so old metrics are not reused"
    );

    let mut row_cache = RowGlyphCache::new();
    row_cache.resize(24);
    row_cache.insert(1, 0, 42, CachedRow::default());
    assert_eq!(row_cache.len(), 1);
    row_cache.invalidate_all();
    assert_eq!(row_cache.len(), 0, "font-family apply must invalidate cached shaped rows");
}
