use cosmic_text::FontSystem;
use sonic_text::{
    glyph_atlas::Rasterizer,
    swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX},
};
use sonic_types::GlyphKey;

fn font_system_with_assets() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                if let Ok(bytes) = std::fs::read(&p) {
                    sonic_text::load_font_data_with_sonic_overrides(&mut fs, bytes);
                }
            }
        }
    }
    fs
}

#[test]
fn lcd_outline_glyph_is_not_marked_color() {
    let mut fs = font_system_with_assets();
    let mut rasterizer = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let tile = rasterizer.rasterize(GlyphKey::new('A', false, false)).expect("rasterize A");

    assert!(!tile.is_color, "LCD outline glyph must not be flagged as color");
}
