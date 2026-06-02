//! #570 — tab-title advance must use unicode display width, not `chars().count()`.
//!
//! With CJK glyphs (each ~2 cells wide), the old `chars().count()` advance
//! collapsed the wide char to a single cell column, so the trailing ASCII
//! glyph landed on top of the CJK glyph's second cell. This test feeds a
//! mixed ASCII/CJK/ASCII title through `emit_tab_title_glyphs` and asserts
//! that each character lands on the column predicted by `unicode-width`:
//! ASCII advances by 1 cell, CJK by 2.

use cosmic_text::FontSystem;
use glyphon::Color as GColor;
use sonicterm_gpu::core::{emit_tab_title_glyphs, TabTitleGlyphDebug};
use sonicterm_gpu::text_pipeline::GlyphInstance;
use sonicterm_text::{glyph_atlas::GlyphAtlas, swash_rasterizer::SwashRasterizer};
use sonicterm_ui::tab_spans::{
    build_tab_title_rich_text_spans, build_tab_title_spans, tab_title_font_size, TabSpanInput,
};
use sonicterm_ui::tabs::{Tab, TabBar};
use unicode_width::UnicodeWidthChar;

const FONT_SIZE: f32 = 14.0;
const SCALE_FACTOR: f32 = 2.0;
const FONT_FAMILY: &str = "Rec Mono St.Helens";

fn font_system_with_bundled() -> FontSystem {
    let mut fs = FontSystem::new();
    sonicterm_text::swash_rasterizer::load_bundled_fonts(&mut fs);
    fs
}

#[test]
fn cjk_title_advances_by_double_width() {
    // "a日b" — ASCII(1) + CJK(2) + ASCII(1). Expected lead-col layout = [0, 1, 3].
    let title = "a日b";
    let mut tabs = TabBar::new();
    tabs.push(Tab::new(title));

    let tab_font_size = tab_title_font_size(FONT_SIZE);
    let avg_glyph_w: f32 = 9.0;
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

    // 1) Output non-empty — guards against silently dropping every glyph.
    assert!(
        !glyphs.is_empty(),
        "expected non-empty glyph_instances for title {title:?}"
    );
    assert!(
        !debug.is_empty(),
        "expected non-empty debug records for title {title:?}"
    );
    assert_eq!(
        debug.len(),
        title.chars().count(),
        "expected one debug record per character of {title:?}, got {:?}",
        debug
    );

    // 2) Sort by x so we can index by visual order (the emit loop does not
    //    guarantee shaper-output ordering matches column order).
    let mut entries: Vec<&TabTitleGlyphDebug> = debug.iter().collect();
    entries.sort_by(|a, b| a.rect[0].partial_cmp(&b.rect[0]).unwrap());

    // 3) Compute expected per-char advance from unicode-width: each char
    //    occupies `width(ch)` cells, so the gap from char[i] to char[i+1] is
    //    `width(char[i]) * avg_glyph_w`. NOTE: glyph absolute x is offset by
    //    tab-bar centering padding (`build_tab_title_spans` injects leading
    //    spaces); whitespace padding glyphs are skipped inside the emit loop,
    //    so the 3 debug entries correspond 1:1 to the 3 title chars but their
    //    leading x is not zero. We therefore assert on RELATIVE gaps only.
    let chars: Vec<char> = title.chars().collect();
    let widths: Vec<u16> = chars
        .iter()
        .map(|c| UnicodeWidthChar::width(*c).unwrap_or(0).max(1) as u16)
        .collect();
    assert_eq!(
        widths,
        vec![1, 2, 1],
        "test setup: expected per-char widths for {title:?}"
    );

    // 4) Per-pair gap assertions — the core regression signal.
    //
    //    `gx = lead_col * avg_glyph_w + px_offset[x] * inv_s`. The
    //    column-driven advance is the dominant term; per-glyph bearings
    //    (px_offset[x]) add a small skew. With the FIX:
    //        gap_ab ≈ 1 * avg_glyph_w + bearing_skew_ab
    //        gap_bc ≈ 2 * avg_glyph_w + bearing_skew_bc
    //    so `gap_bc - gap_ab ≈ avg_glyph_w` (the extra wide-cell of '日').
    //    With the BUG (`chars().count()`) both gaps cover 1 cell column, so
    //    `gap_bc - gap_ab ≈ 0` regardless of bearings — this single delta
    //    isolates the regression while remaining bearing-tolerant.
    let gap_ab = entries[1].rect[0] - entries[0].rect[0]; // 'a' → '日'
    let gap_bc = entries[2].rect[0] - entries[1].rect[0]; // '日' → 'b'
    let extra = gap_bc - gap_ab;
    let tol = avg_glyph_w * 0.5;
    assert!(
        (extra - avg_glyph_w).abs() <= tol,
        "CJK '日' must contribute one extra cell of advance: \
         expected (gap_bc - gap_ab) ≈ {avg_glyph_w}, got {extra} \
         (gap_ab={gap_ab}, gap_bc={gap_bc}, tol={tol}) — \
         this is the chars().count() regression signal",
    );
    // Belt-and-braces: a wide character's trailing gap must strictly exceed
    // its leading (narrow→wide) gap. Old chars().count() would have made
    // them comparable (or even gap_bc < gap_ab due to bearings).
    assert!(
        gap_bc > gap_ab,
        "CJK trailing gap ({gap_bc}) must exceed ASCII leading gap ({gap_ab})",
    );
}
