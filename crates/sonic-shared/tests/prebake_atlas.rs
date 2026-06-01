//! Pre-bake atlas test — perf audit #10.
//!
//! Asserts that `prebake_box_and_powerline` actually populates the
//! atlas with entries from the U+2500..U+259F (Box Drawing + Block
//! Elements) and U+E0A0..U+E0D7 (Powerline PUA) ranges when the
//! bundled fonts are present. The point is to verify the prebake is
//! wired up — without this, a future refactor that silently drops the
//! call (or replaces the rasterizer with one that doesn't honor
//! prebake keys) would only surface as a first-paint stutter regression,
//! invisible to the rest of the test suite.

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::glyph_atlas::GlyphAtlas;
use sonic_shared::swash_rasterizer::{
    prebake_box_and_powerline, SwashRasterizer, DEFAULT_RASTER_PX, PREBAKE_RANGES,
};

fn font_system_with_bundled() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
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
fn prebake_populates_atlas_with_box_drawing_glyphs() {
    let mut fs = font_system_with_bundled();
    let mut atlas = GlyphAtlas::default_size();
    let baseline = atlas.len();

    let inserted = {
        let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
        prebake_box_and_powerline(&mut rast, &mut atlas)
    };

    // The bundled Rec Mono Casual covers all of U+2500..U+259F; even on
    // a system without any Powerline-capable face, the box-drawing
    // range alone should yield well over 100 entries. We assert a very
    // conservative floor so the test passes on minimal CI font sets
    // while still catching the regression "prebake silently skipped all
    // glyphs."
    assert!(
        inserted >= 64,
        "prebake should resolve ≥64 box/powerline glyphs from bundled fonts, got {inserted}"
    );
    assert!(
        atlas.len() >= baseline + 64,
        "atlas len should grow by ≥64 after prebake (baseline {baseline}, after {})",
        atlas.len()
    );

    // Spot-check: U+2500 (HORIZONTAL BAR ─) must resolve to a real
    // glyph slot, and the corresponding atlas entry must exist with a
    // non-zero pixel footprint. If this regresses, the box-drawing
    // first-frame stutter is back. We look up via the same slot the
    // rasterizer resolved (it may not be slot 0 if the primary face
    // lacks the box-drawing block — Recursive Mono does not always).
    let slot = {
        let mut rast = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
        rast.resolve_slot('\u{2500}', false, false)
    };
    if let Some(slot) = slot {
        let key = GlyphKey::with_slot('\u{2500}', slot, false, false);
        let info = atlas.get(key).expect("U+2500 should be in the atlas after prebake");
        assert!(
            info.px_size[0] > 0 && info.px_size[1] > 0,
            "U+2500 atlas entry should have non-zero pixel size, got {:?}",
            info.px_size
        );
    }
}

#[test]
fn prebake_ranges_match_intended_coverage() {
    // Guard against future edits that accidentally widen or narrow the
    // prebake set — those should be deliberate, with a test bump.
    assert_eq!(PREBAKE_RANGES.len(), 2);
    assert_eq!(*PREBAKE_RANGES[0].start(), 0x2500);
    assert_eq!(*PREBAKE_RANGES[0].end(), 0x259F);
    assert_eq!(*PREBAKE_RANGES[1].start(), 0xE0A0);
    assert_eq!(*PREBAKE_RANGES[1].end(), 0xE0D7);

    let total: u32 = PREBAKE_RANGES.iter().map(|r| r.end() - r.start() + 1).sum();
    // ~250 codepoints — well under the ~16k-tile atlas budget.
    assert!(total < 300, "prebake set should stay small; got {total} codepoints");
}
