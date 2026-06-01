//! #439 regression guard, #453 cycle 2: the bundled Rec Mono St.Helens
//! MUST cover the Powerline PUA (U+E0B0), the geometric-shapes block
//! (U+25B6), and the supplementary-PUA-A range used by Nerd-Font icons
//! (U+F0001) at slot 0 of the resolve chain.
//!
//! Earlier revisions of this test used `if let Some(slot) = ...` and
//! silently skipped U+F0001 if absent, which let the previously-shipped
//! tofu class slip past CI. Every assertion below is now strict:
//! `assert_eq!(resolve_slot(...), Some(0), ...)` so that both a wrong
//! slot (fallback chain serving the glyph) AND a missing glyph (None)
//! fail loudly. No skips.

use cosmic_text::FontSystem;
use sonicterm_text::{
    glyph_atlas::Rasterizer,
    swash_rasterizer::{load_bundled_fonts, SwashRasterizer, DEFAULT_RASTER_PX},
};
use sonicterm_types::GlyphKey;

fn make_rasterizer(fs: &mut FontSystem) -> SwashRasterizer<'_> {
    SwashRasterizer::new(fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX)
}

#[test]
fn load_bundled_fonts_actually_loads_st_helens() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let has_st_helens = fs.db().faces().any(|f| {
        f.families
            .iter()
            .any(|(name, _)| name.contains("St.Helens") || name.contains("St Helens"))
    });
    assert!(
        has_st_helens,
        "Rec Mono St.Helens MUST be in fontdb after load_bundled_fonts (asset discovery broken)"
    );
}

#[test]
fn st_helens_resolves_powerline_e0b0_to_slot_0() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut r = make_rasterizer(&mut fs);
    assert_eq!(
        r.resolve_slot('\u{e0b0}', false, false),
        Some(0),
        "bundled St.Helens MUST cover U+E0B0 Powerline chevron at slot 0 \
         (slot 0 = primary family; any other slot means a fallback served the glyph, \
         which means the bundled font is missing this codepoint and Powerline prompts will tofu)"
    );
}

#[test]
fn st_helens_resolves_filled_arrow_25b6_to_slot_0() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut r = make_rasterizer(&mut fs);
    assert_eq!(
        r.resolve_slot('\u{25b6}', false, false),
        Some(0),
        "bundled St.Helens MUST cover U+25B6 filled right-pointing triangle at slot 0 \
         (geometric-shapes block; any other slot means fallback served it)"
    );
}

#[test]
fn st_helens_resolves_nf_pua_f0001_to_slot_0() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut r = make_rasterizer(&mut fs);
    assert_eq!(
        r.resolve_slot('\u{f0001}', false, false),
        Some(0),
        "bundled St.Helens MUST cover U+F0001 NerdFont supplementary-PUA-A at slot 0 \
         (no skip-if-absent: a missing PUA-A glyph IS the #439-class regression we are guarding)"
    );
}

#[test]
fn st_helens_rasterizes_powerline_e0b0_to_nonempty_tile() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut r = make_rasterizer(&mut fs);
    let key = GlyphKey::with_slot('\u{e0b0}', 0, false, false);
    let tile = r
        .rasterize(key)
        .expect("U+E0B0 MUST rasterize from bundled St.Helens slot 0");
    assert!(
        tile.width > 0 && tile.height > 0,
        "U+E0B0 MUST produce non-empty tile from slot 0, got {}x{}",
        tile.width,
        tile.height
    );
}

#[test]
fn st_helens_rasterizes_filled_arrow_25b6_to_nonempty_tile() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut r = make_rasterizer(&mut fs);
    let key = GlyphKey::with_slot('\u{25b6}', 0, false, false);
    let tile = r
        .rasterize(key)
        .expect("U+25B6 MUST rasterize from bundled St.Helens slot 0");
    assert!(
        tile.width > 0 && tile.height > 0,
        "U+25B6 MUST produce non-empty tile from slot 0, got {}x{}",
        tile.width,
        tile.height
    );
}

#[test]
fn st_helens_rasterizes_nf_pua_f0001_to_nonempty_tile() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);
    let mut r = make_rasterizer(&mut fs);
    let key = GlyphKey::with_slot('\u{f0001}', 0, false, false);
    let tile = r
        .rasterize(key)
        .expect("U+F0001 MUST rasterize from bundled St.Helens slot 0 (no skip)");
    assert!(
        tile.width > 0 && tile.height > 0,
        "U+F0001 MUST produce non-empty tile from slot 0, got {}x{}",
        tile.width,
        tile.height
    );
}
