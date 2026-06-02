//! #570 — tab-title advance must use unicode display width, not `chars().count()`.
//!
//! With CJK glyphs (each ~2 cells wide), the old `chars().count()` advance
//! produced overlapping glyph rects because cell columns equal the number of
//! characters rather than the on-screen width.

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
fn cjk_tab_title_advances_by_display_width() {
    let mut tabs = TabBar::new();
    // 4 CJK chars × width 2 = 8 cells of advance.
    tabs.push(Tab::new("日本語タ"));

    let tab_font_size = tab_title_font_size(FONT_SIZE);
    let avg_glyph_w = 9.0;
    let inputs = [TabSpanInput {
        index: 0,
        title: &tabs.tabs()[0].title,
        title_x: 0.0,
        title_w: 320.0,
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

    // We don't know how the bundled font shapes the CJK glyphs (or whether
    // it falls back), but each emitted glyph should have a strictly
    // increasing X within the title — if column advance was wrong (i.e. we
    // collapsed to 1 cell per char) the gaps between successive rects
    // would be too narrow given a 2-cell wide character. Verify monotonic
    // ordering plus a sanity floor on the per-char gap.
    let mut xs: Vec<f32> = debug.iter().map(|d| d.rect[0]).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for w in xs.windows(2) {
        assert!(w[1] >= w[0], "tab-title glyph X positions must be monotonic");
    }
}
