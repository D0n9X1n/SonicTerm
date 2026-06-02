use cosmic_text::FontSystem;
use glyphon::Color as GColor;
use sonicterm_gpu::core::{emit_tab_title_glyphs, TabTitleGlyphDebug};
use sonicterm_gpu::text_pipeline::GlyphInstance;
use sonicterm_text::{glyph_atlas::GlyphAtlas, swash_rasterizer::SwashRasterizer};
use sonicterm_ui::tab_spans::{
    build_tab_title_rich_text_spans, build_tab_title_spans, tab_title_font_size, TabSpanInput,
};
use sonicterm_ui::tabs::{Tab, TabBar};

const FONT_SIZE: f32 = 14.0;
const SCALE_FACTOR: f32 = 2.0;
const FONT_FAMILY: &str = "Rec Mono St.Helens";

fn font_system_with_bundled() -> FontSystem {
    let mut fs = FontSystem::new();
    sonicterm_text::swash_rasterizer::load_bundled_fonts(&mut fs);
    fs
}

#[test]
fn tab_title_glyphs_use_device_scaled_sonic_atlas() {
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("Sharp"));

    let tab_font_size = tab_title_font_size(FONT_SIZE);
    let avg_glyph_w = 9.0;
    let inputs = [TabSpanInput {
        index: 0,
        title: &tabs.tabs()[0].title,
        title_x: 0.0,
        title_w: 160.0,
        is_active: true,
        badge: None,
    }];
    let (title_text, tab_spans) = build_tab_title_spans(
        &inputs,
        avg_glyph_w,
        GColor::rgb(0xFF, 0xFF, 0xFF),
        GColor::rgb(0xAA, 0xAA, 0xAA),
    );
    let spans = build_tab_title_rich_text_spans(
        &title_text,
        &tab_spans,
        FONT_FAMILY,
        GColor::rgb(0xAA, 0xAA, 0xAA),
    )
    .spans;

    let mut fs = font_system_with_bundled();
    let raster_px = tab_font_size * SCALE_FACTOR;
    let mut rasterizer = SwashRasterizer::new(&mut fs, FONT_FAMILY, raster_px);
    let mut atlas = GlyphAtlas::default_size();
    let mut glyphs: Vec<GlyphInstance> = Vec::new();
    let mut debug: Vec<TabTitleGlyphDebug> = Vec::new();

    emit_tab_title_glyphs(
        &mut atlas,
        FONT_FAMILY,
        raster_px,
        SCALE_FACTOR,
        &mut rasterizer,
        &spans,
        24.0,
        avg_glyph_w,
        800.0,
        600.0,
        &mut glyphs,
        Some(&mut debug),
    );

    assert!(!glyphs.is_empty(), "tab title must emit SonicTerm atlas glyph instances");
    let first = debug.first().expect("debug record for first tab-title glyph");
    assert_eq!(first.raster_px, raster_px, "tab title rasterizer must use device-scale px");
    assert!(
        first.px_size[1] as f32 > tab_font_size,
        "2x raster tile height should be larger than the logical tab font size"
    );
    assert!(
        first.rect[3] <= first.px_size[1] as f32 / SCALE_FACTOR + 0.01,
        "logical output height must be scaled back by 1 / scale_factor"
    );
    assert!(
        first.rect[3] < first.px_size[1] as f32,
        "logical glyph rect must not use physical tile height directly"
    );
}
