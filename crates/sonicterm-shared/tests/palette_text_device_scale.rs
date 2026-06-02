//! Regression coverage for #384: command-palette text must render
//! through the SonicTerm glyph atlas at device pixel scale (mirroring
//! `emit_tab_title_glyphs`) so it stays crisp on Windows HiDPI.
//!
//! Pre-fix the palette routed through `glyphon::TextRenderer::set_text`
//! which bypassed `scale_factor`, producing blurry text on any
//! fractional / 2× display. This test pins the new code path in place
//! by asserting that the emitter rasterizes at `font_size * scale`
//! pixels and emits glyph instances whose logical rect is the physical
//! tile divided by `scale_factor`.

use cosmic_text::FontSystem;
use glyphon::Color as GColor;
use sonicterm_gpu::text_pipeline::GlyphInstance;
use sonicterm_shared::render::{emit_overlay_text_glyphs, OverlayTextGlyphDebug};
use sonicterm_text::{glyph_atlas::GlyphAtlas, swash_rasterizer::SwashRasterizer};

const FONT_SIZE: f32 = 14.0;
const SCALE_FACTOR: f32 = 2.0;
const FONT_FAMILY: &str = "Rec Mono St.Helens";

fn font_system_with_bundled() -> FontSystem {
    let mut fs = FontSystem::new();
    sonicterm_text::swash_rasterizer::load_bundled_fonts(&mut fs);
    fs
}

#[test]
fn palette_text_glyphs_use_device_scaled_sonic_atlas() {
    // Representative palette label — short ASCII string like
    // "New Tab" or "Split Vertical" the user actually sees in the
    // command palette overlay.
    let label = "New Tab";

    let mut fs = font_system_with_bundled();
    let raster_px = FONT_SIZE * SCALE_FACTOR;
    let mut rasterizer = SwashRasterizer::new(&mut fs, FONT_FAMILY, raster_px);
    let mut atlas = GlyphAtlas::default_size();
    let mut glyphs: Vec<GlyphInstance> = Vec::new();
    let mut debug: Vec<OverlayTextGlyphDebug> = Vec::new();

    // PaletteLayout-like sample geometry: a 600×40 row at (100, 200)
    // inside an 800×600 surface. Origin sits at the row's left padding,
    // baseline ~80 % of the way down the row.
    let row_x = 100.0;
    let row_y = 200.0;
    let row_w = 600.0;
    let row_h = 40.0;
    let origin_x = row_x + 12.0; // PALETTE_ROW_PAD_X
    let baseline_y = row_y + (row_h + FONT_SIZE * 0.8) * 0.5;

    emit_overlay_text_glyphs(
        &mut atlas,
        FONT_FAMILY,
        FONT_SIZE,
        SCALE_FACTOR,
        &mut rasterizer,
        label,
        GColor::rgb(0xFF, 0xFF, 0xFF),
        origin_x,
        baseline_y,
        [row_x, row_y, row_w, row_h],
        800.0,
        600.0,
        &mut glyphs,
        Some(&mut debug),
    );

    assert!(!glyphs.is_empty(), "palette label must emit SonicTerm-atlas glyph instances (#384)");
    assert!(!debug.is_empty(), "debug records must mirror emitted glyphs");

    let first = debug.first().expect("debug record for first palette glyph");
    assert_eq!(
        first.raster_px,
        FONT_SIZE * SCALE_FACTOR,
        "palette rasterizer MUST use font_size * scale_factor for crisp HiDPI output (#384)"
    );
    assert_eq!(first.font_size, FONT_SIZE, "debug font_size echoes the logical font_size input");
    assert_eq!(
        first.scale_factor, SCALE_FACTOR,
        "debug scale_factor echoes the device pixel scale input"
    );
    // 2× rasterized tile must be physically taller than the logical
    // font_size — that is, atlas tiles are stored at *physical* pixels.
    assert!(
        first.px_size[1] as f32 > FONT_SIZE,
        "2x raster tile height ({}) must exceed logical font size ({})",
        first.px_size[1],
        FONT_SIZE
    );
    // The emitted *logical* rect height must be the physical tile
    // divided by scale_factor — i.e. the SonicTerm atlas device-scale
    // contract: rasterize big, draw small. (Pre-fix glyphon path
    // drew at logical size with logical rasterization → blur.)
    let logical_h = first.rect[3];
    let physical_h = first.px_size[1] as f32;
    assert!(
        (logical_h - physical_h / SCALE_FACTOR).abs() < 0.01,
        "logical rect height ({}) must equal physical tile height ({}) / scale_factor ({})",
        logical_h,
        physical_h,
        SCALE_FACTOR
    );
    assert!(
        logical_h < physical_h,
        "logical glyph rect ({}) must NOT use the physical tile height ({}) directly",
        logical_h,
        physical_h
    );
}
