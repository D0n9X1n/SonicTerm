//! Diagnostic + regression tests for issue #439: Powerline U+E0B0 and
//! filled-arrow U+25B6 render as missing/tofu despite the bundled
//! `Rec Mono St.Helens` font containing both glyphs.
//!
//! The chain we exercise here is the SAME loading path used in
//! production: `load_bundled_fonts` (also used by
//! `sonicterm_shared::render`), through `SwashRasterizer` with primary
//! family `"Rec Mono St.Helens"`. Each test pins a different stage of
//! the pipeline so a future regression points directly at the broken
//! stage:
//!
//! - H1: `resolve_slot` (charmap walk against the resolved fontdb id)
//! - H3: `rasterize` (swash `Render` produces a non-empty tile)
//! - H4: `GlyphAtlas::get_or_insert` (atlas surfaces the non-empty tile
//!   rather than the zero-size sentinel)
//!
//! NOTE: The bundled `Rec Mono St.Helens-Regular.ttf` cmap was verified
//! by hand to cover U+E0B0 (gid 29759), U+25B6 (gid 889) and U+F0001
//! (gid 33628). Any failure of these assertions is therefore a pipeline
//! bug, not a font-coverage gap.

use cosmic_text::FontSystem;
use sonicterm_text::{
    glyph_atlas::{GlyphAtlas, Rasterizer},
    shape::{shape_run, RunStyle},
    swash_rasterizer::{load_bundled_fonts, SwashRasterizer, DEFAULT_RASTER_PX},
};
use sonicterm_types::{Cell, GlyphKey};

fn setup() -> (FontSystem, GlyphAtlas) {
    // The rasterizer borrows the FontSystem mutably, so we can't return
    // it from a helper that owns the FontSystem. Instead we return the
    // FontSystem and an atlas — callers construct the rasterizer locally.
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let atlas = GlyphAtlas::new(512, 512);
    (fs, atlas)
}

#[test]
fn st_helens_resolves_powerline_e0b0_to_slot_0() {
    let (mut fs, _atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    assert_eq!(
        rast.resolve_slot('\u{e0b0}', false, false),
        Some(0),
        "St.Helens bundled font should cover U+E0B0 Powerline chevron in slot 0; \
         if this fails, fontdb's view of the bundled face has lost cmap (see \
         crates/sonicterm-text/src/lib.rs::load_font_data_with_sonic_overrides)"
    );
}

#[test]
fn st_helens_resolves_filled_arrow_25b6_to_slot_0() {
    let (mut fs, _atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    assert_eq!(
        rast.resolve_slot('\u{25b6}', false, false),
        Some(0),
        "St.Helens covers U+25B6 BLACK RIGHT-POINTING TRIANGLE (gid 889 in shipped TTF); \
         a None here means the cmap walk is missing this glyph despite the cmap subtable \
         containing it — investigate metadata override in load_font_data_with_sonic_overrides"
    );
}

#[test]
fn st_helens_resolves_nf_pua_f0001_to_slot_0() {
    let (mut fs, _atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    assert_eq!(
        rast.resolve_slot('\u{f0001}', false, false),
        Some(0),
        "St.Helens covers U+F0001 (gid 33628 in shipped TTF); a None here means the cmap \
         walk is missing this glyph despite cmap subtable containing it"
    );
}

#[test]
fn st_helens_rasterizes_powerline_e0b0_to_nonempty_tile() {
    let (mut fs, _atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let Some(slot) = rast.resolve_slot('\u{e0b0}', false, false) else {
        panic!("preconditions failed: resolve_slot returned None for U+E0B0");
    };
    let key = GlyphKey::with_slot('\u{e0b0}', slot, false, false);
    let tile = rast.rasterize(key).expect("rasterize U+E0B0 must succeed");
    assert!(
        tile.width > 0 && tile.height > 0,
        "U+E0B0 should produce non-empty tile, got ({}, {})",
        tile.width,
        tile.height
    );
}

#[test]
fn st_helens_rasterizes_filled_arrow_25b6_to_nonempty_tile() {
    let (mut fs, _atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let Some(slot) = rast.resolve_slot('\u{25b6}', false, false) else {
        panic!("preconditions failed: resolve_slot returned None for U+25B6");
    };
    let key = GlyphKey::with_slot('\u{25b6}', slot, false, false);
    let tile = rast.rasterize(key).expect("rasterize U+25B6 must succeed");
    assert!(
        tile.width > 0 && tile.height > 0,
        "U+25B6 should produce non-empty tile, got ({}, {})",
        tile.width,
        tile.height
    );
}

#[test]
fn st_helens_rasterizes_nf_pua_f0001_to_nonempty_tile() {
    let (mut fs, _atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let Some(slot) = rast.resolve_slot('\u{f0001}', false, false) else {
        panic!("preconditions failed: resolve_slot returned None for U+F0001");
    };
    let key = GlyphKey::with_slot('\u{f0001}', slot, false, false);
    let tile = rast.rasterize(key).expect("rasterize U+F0001 must succeed");
    assert!(
        tile.width > 0 && tile.height > 0,
        "U+F0001 should produce non-empty tile, got ({}, {})",
        tile.width,
        tile.height
    );
}

#[test]
fn atlas_returns_real_uv_for_e0b0() {
    let (mut fs, mut atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let slot = rast.resolve_slot('\u{e0b0}', false, false).expect("resolve_slot must succeed");
    let key = GlyphKey::with_slot('\u{e0b0}', slot, false, false);
    let info = atlas
        .get_or_insert(key, &mut rast)
        .expect("atlas.get_or_insert must return Some for resolvable glyph");
    assert!(
        info.px_size[0] > 0 && info.px_size[1] > 0,
        "U+E0B0 atlas entry must have non-zero px_size (got {:?}); a zero size means the \
         atlas saw an empty tile / rasterizer failure and is now caching the tofu sentinel",
        info.px_size
    );
}

#[test]
fn atlas_returns_real_uv_for_25b6() {
    let (mut fs, mut atlas) = setup();
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let slot = rast.resolve_slot('\u{25b6}', false, false).expect("resolve_slot must succeed");
    let key = GlyphKey::with_slot('\u{25b6}', slot, false, false);
    let info = atlas
        .get_or_insert(key, &mut rast)
        .expect("atlas.get_or_insert must return Some for resolvable glyph");
    assert!(
        info.px_size[0] > 0 && info.px_size[1] > 0,
        "U+25B6 atlas entry must have non-zero px_size (got {:?})",
        info.px_size
    );
}

// -----------------------------------------------------------------------
// H2: shape_run path — these reproduce exactly what the production
// renderer does in `crates/sonicterm-shared/src/render/core.rs:4509+`:
// shape a row of cells, then for each ShapedGlyph either use its
// (font_slot, glyph_id) directly (shaped path) or fall back to a char-
// based resolve_slot when glyph_id == 0. End-to-end the result must be
// a non-empty atlas tile — anything else means the user sees missing /
// tofu glyphs.
//
// Both U+E0B0 and U+25B6 are single-cell, so the shape.rs cluster
// logic zeros their shaped glyph_id (see shape.rs:290-295) and the
// renderer takes the char-based fallback. The tests below assert the
// FALLBACK actually produces a drawable tile.
// -----------------------------------------------------------------------

fn cell(ch: char) -> Cell {
    let mut c = Cell::default();
    c.ch = ch;
    c
}

fn shape_one_to_atlas(ch: char) {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut atlas = GlyphAtlas::new(512, 512);
    let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);

    let style = RunStyle { bold: false, italic: false };
    let cells = vec![(0u16, cell(ch))];
    let shaped = shape_run(&mut rast, "Rec Mono St.Helens", DEFAULT_RASTER_PX, style, &cells);
    assert!(
        !shaped.is_empty(),
        "shape_run produced no glyphs for U+{:04X} — cosmic-text shaping failed entirely",
        ch as u32
    );

    for g in &shaped {
        // Mimics the render/core.rs branch starting at line 4529:
        // single-cell glyphs come back from shape_run with glyph_id == 0
        // (see shape.rs:290-295). We must resolve a slot via charmap walk
        // and rasterize through the char path.
        if g.glyph_id == 0 {
            let slot = rast.resolve_slot(g.ch, false, false).unwrap_or_else(|| {
                panic!(
                    "char-fallback resolve_slot returned None for U+{:04X} → renderer would draw \
                     tofu. This is the regression at issue #439.",
                    g.ch as u32
                )
            });
            let key = GlyphKey::with_slot(g.ch, slot, false, false);
            let info = atlas.get_or_insert(key, &mut rast).unwrap_or_else(|| {
                panic!("atlas.get_or_insert returned None for U+{:04X}", g.ch as u32)
            });
            assert!(
                info.px_size[0] > 0 && info.px_size[1] > 0,
                "U+{:04X} char-fallback produced empty tile {:?} → renderer skips draw, glyph \
                 invisible",
                g.ch as u32,
                info.px_size
            );
        } else {
            let key = GlyphKey::shaped(g.ch, g.font_slot, g.glyph_id, false, false);
            let info = atlas.get_or_insert(key, &mut rast).unwrap_or_else(|| {
                panic!(
                    "shaped-path atlas.get_or_insert returned None for U+{:04X} gid={}",
                    g.ch as u32, g.glyph_id
                )
            });
            assert!(
                info.px_size[0] > 0 && info.px_size[1] > 0,
                "U+{:04X} shaped-path produced empty tile {:?}",
                g.ch as u32,
                info.px_size
            );
        }
    }
}

#[test]
fn shape_run_to_atlas_powerline_e0b0_end_to_end() {
    shape_one_to_atlas('\u{e0b0}');
}

#[test]
fn shape_run_to_atlas_filled_arrow_25b6_end_to_end() {
    shape_one_to_atlas('\u{25b6}');
}

#[test]
fn shape_run_to_atlas_nf_pua_f0001_end_to_end() {
    shape_one_to_atlas('\u{f0001}');
}

// -----------------------------------------------------------------------
// H1-deep: assert the metadata-override code in
// `load_font_data_with_sonic_overrides` (lib.rs:50-88) does not
// silently detach the binary source from the face when it removes the
// original face and pushes a corrected FaceInfo. Without this guard a
// future regression where `push_face_info` ends up with a dangling
// source reference would produce a fontdb face that *resolves* via
// query but returns an empty charmap from `as_swash().charmap()` —
// which would manifest as missing PUA glyphs (issue #439 class).
// -----------------------------------------------------------------------

#[test]
fn metadata_patched_face_still_has_live_charmap() {
    use cosmic_text::fontdb;
    let mut fs = FontSystem::new();
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    sonicterm_text::load_font_data_with_sonic_overrides(&mut fs, bytes);

    let id = sonicterm_text::swash_rasterizer::lookup_id_in_db(
        fs.db(),
        "Rec Mono St.Helens",
        false,
        false,
    )
    .expect("upright St.Helens face must resolve from fontdb after override");

    // The face must still have its binary source loaded — otherwise
    // get_font returns None and the rasterizer falls through to tofu.
    let font = fs.get_font(id, fontdb::Weight::NORMAL).expect(
        "font binary must be live for the patched face — if this is None, \
         the override's remove_face + push_face_info dropped the source",
    );
    let swash_font = font.as_swash();
    let charmap = swash_font.charmap();

    // These exact gids are the verified facts from issue #439.
    assert_ne!(
        charmap.map('\u{e0b0}'),
        0,
        "patched face's charmap must still map U+E0B0 (was gid 29759 pre-patch)"
    );
    assert_ne!(charmap.map('\u{25b6}'), 0, "patched face's charmap must still map U+25B6");
    assert_ne!(charmap.map('\u{f0001}'), 0, "patched face's charmap must still map U+F0001");
    // Sanity: ASCII works too.
    assert_ne!(charmap.map('A'), 0);
}
