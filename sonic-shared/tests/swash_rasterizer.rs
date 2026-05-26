//! Tests for `sonic_shared::swash_rasterizer::SwashRasterizer`.
//!
//! These exercise real glyph rasterization against the bundled
//! Rec Mono Casual font shipped in `assets/fonts/`. The test harness
//! loads that font into a fresh `cosmic_text::FontSystem` so the
//! results are reproducible across machines that don't have the font
//! installed system-wide.

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::glyph_atlas::Rasterizer;
use sonic_shared::swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX};

/// Build a font system populated with the four Rec Mono Casual cuts
/// shipped under `assets/fonts/`. Returns the system; the rasterizer
/// borrows from it.
fn font_system_with_bundled() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read assets/fonts ({dir:?}): {e}"));
    let mut loaded = 0;
    for e in entries.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
            let bytes = std::fs::read(&p).unwrap();
            fs.db_mut().load_font_data(bytes);
            loaded += 1;
        }
    }
    assert!(loaded > 0, "expected at least one bundled font in {dir:?}");
    fs
}

#[test]
fn rasterizes_capital_a_with_non_empty_coverage() {
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let tile = r.rasterize(GlyphKey::new('A', false, false)).expect("A -> Some");
    assert!(tile.width > 0 && tile.height > 0, "A must have visible pixels");
    let any_lit = tile.coverage.iter().any(|&b| b > 0);
    assert!(any_lit, "A's coverage mask must have at least one non-zero pixel");
}

#[test]
fn space_returns_empty_tile() {
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let tile = r.rasterize(GlyphKey::new(' ', false, false)).expect("space -> Some");
    assert!(
        tile.is_empty(),
        "space must be treated as a blank tile (width={} height={})",
        tile.width,
        tile.height
    );
}

#[test]
fn bold_and_regular_differ() {
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let reg = r.rasterize(GlyphKey::new('e', false, false)).expect("e regular");
    let bold = r.rasterize(GlyphKey::new('e', true, false)).expect("e bold");
    assert_ne!(
        reg.coverage, bold.coverage,
        "bold cut must produce a different coverage mask than the regular cut"
    );
}

#[test]
fn italic_and_upright_differ() {
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let up = r.rasterize(GlyphKey::new('e', false, false)).expect("e upright");
    let it = r.rasterize(GlyphKey::new('e', false, true)).expect("e italic");
    assert_ne!(
        up.coverage, it.coverage,
        "italic cut must produce a different coverage mask than the upright cut"
    );
}

#[test]
fn determinism_same_key_twice() {
    let mut fs = font_system_with_bundled();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let a1 = r.rasterize(GlyphKey::new('@', false, false)).expect("@1");
    let a2 = r.rasterize(GlyphKey::new('@', false, false)).expect("@2");
    assert_eq!(a1.width, a2.width);
    assert_eq!(a1.height, a2.height);
    assert_eq!(a1.coverage, a2.coverage, "same key must produce byte-identical coverage");
}

#[test]
fn missing_family_returns_none() {
    // No font loaded — every lookup must fail gracefully (no panic).
    let mut fs = FontSystem::new();
    let mut r = SwashRasterizer::new(&mut fs, "Definitely Not A Real Font 42", 14.0);
    let res = r.rasterize(GlyphKey::new('A', false, false));
    assert!(res.is_none(), "unknown family must produce None, got {res:?}");
}

#[test]
fn px_and_family_reflect_constructor_args() {
    // Regression for PR #42 review: render.rs used to hardcode
    // "Rec Mono Casual" / DEFAULT_RASTER_PX (14.0) when building the
    // atlas rasterizer, ignoring user `config.font_family` /
    // `config.font_size`. The renderer now threads those values
    // through SwashRasterizer::new; this test pins the contract that
    // the rasterizer actually retains whatever the caller asked for
    // (so config-honoring at the call site is observable).
    let mut fs = font_system_with_bundled();
    let r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", 20.0);
    assert_eq!(r.px(), 20.0, "raster px must equal the configured font_size");
    assert_eq!(r.family(), "Rec Mono Casual", "family must equal the configured font_family");
    assert!(
        (r.px() - DEFAULT_RASTER_PX).abs() > f32::EPSILON,
        "test must use a non-default size to be meaningful"
    );
}
