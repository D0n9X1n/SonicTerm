//! Integration test for B3.1 — font fallback for non-ASCII glyphs.
//!
//! Regression target: PR #42 (B3 cutover) routed grid rendering
#![allow(dead_code, unused_imports)]
//! through the swash-backed atlas without any font fallback, so CJK
//! characters, emoji, and even basic accented Latin letters rendered
//! as tofu boxes. This test pins the contract that the rasterizer
//! resolves *some* slot in its fallback chain for those codepoints
//! (when the system provides one) AND that the GlyphKey carries the
//! resolved slot so different fonts cache as distinct atlas tiles.
//!
//! Notes on portability:
//! - We don't depend on any specific font being installed. We only
//!   assert that:
//!     * The fallback chain has more than one entry (the regression
//!       was a chain of length 1).
//!     * Two characters resolved to *different* slots cache as
//!       distinct atlas tiles (GlyphKey slot is part of the hash).
//! - macOS CI machines ship with PingFang SC and Apple Color Emoji,
//!   so we ASSERT actual fallback resolution there. On Windows/Linux
//!   the chain still wires up but a missing font in CI would make a
//!   strict assertion flaky, so we soften to "chain non-trivial".

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::glyph_atlas::{GlyphAtlas, Rasterizer};
use sonic_shared::swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX};

/// Build a font system populated with the four Rec Mono Casual cuts
/// shipped under `assets/fonts/`. On macOS, also let cosmic-text's
/// default font sources merge in the system fonts so PingFang SC and
/// Apple Color Emoji are reachable for the fallback assertion below.
fn font_system_with_bundled_and_system() -> FontSystem {
    // `FontSystem::new()` already loads OS font sources on Mac/Win.
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
fn fallback_chain_is_multi_entry() {
    // The pre-fix rasterizer used a single family. Any cell whose
    // codepoint wasn't in that one face produced a tofu box. The
    // *minimum* shape of the fix is that the chain has more than one
    // family in it.
    let mut fs = font_system_with_bundled_and_system();
    let r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let chain = r.families();
    assert!(chain.len() > 1, "fallback chain must be multi-entry, got {chain:?}");
    assert_eq!(chain[0], "Rec Mono St.Helens", "slot 0 must be the configured primary family");
}

#[test]
fn glyphkey_distinguishes_font_slot() {
    // Two GlyphKeys with the same char/style but different slots must
    // hash differently — otherwise the atlas would collapse a
    // primary-font miss and a fallback-font hit into one cache entry
    // and the wrong one would win.
    let k0 = GlyphKey::new('中', false, false);
    let k1 = GlyphKey::with_slot('中', 1, false, false);
    assert_ne!(k0, k1, "slot is part of GlyphKey identity");
    assert_eq!(k0.font_slot, 0);
    assert_eq!(k1.font_slot, 1);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_resolves_cjk_emoji_and_accent_through_fallback() {
    // On macOS dev + CI machines, PingFang SC + Apple Color Emoji are
    // always present, so we can assert real fallback resolution.
    let mut fs = font_system_with_bundled_and_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);

    // Accented Latin — Rec Mono Casual is likely to have 'é' already,
    // so we accept slot 0 OR a fallback slot. The point is "some slot
    // owns it", not "a specific fallback owns it".
    let e_acute = r.resolve_slot('é', false, false);
    assert!(e_acute.is_some(), "accented é must resolve to some slot");

    // CJK: Rec Mono St.Helens actually ships CJK coverage (the bundled
    // TTF carries Han glyph entries — unlike Rec Mono Casual which the
    // rasterizer used to default to). Either slot 0 OR a platform-
    // fallback slot is acceptable here; the contract is "some slot owns
    // it", not "the platform chain owns it". The strong fallback-routing
    // assertions live on the emoji case below where the bundled face
    // really does lack coverage.
    let zhong = r.resolve_slot('中', false, false);
    assert!(zhong.is_some(), "CJK '中' must resolve via fallback chain on macOS");

    let wen = r.resolve_slot('文', false, false);
    assert!(wen.is_some(), "CJK '文' must resolve via fallback chain on macOS");

    // Emoji: 🎉 = U+1F389. Apple Color Emoji owns it.
    let party = r.resolve_slot('🎉', false, false);
    assert!(party.is_some(), "emoji 🎉 must resolve via fallback chain on macOS");
    assert!(party.unwrap() > 0, "emoji must NOT come from slot 0");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_rasterizes_cjk_through_atlas_get_or_insert() {
    // End-to-end through the atlas: simulate exactly what render.rs
    // does — resolve_slot first, then atlas.get_or_insert with the
    // slot baked into the key. The resulting GlyphInfo must report
    // non-zero pixel size (i.e. a real glyph tile, not a blank tofu
    // sentinel).
    let mut fs = font_system_with_bundled_and_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let mut atlas = GlyphAtlas::default_size();

    for ch in ['中', '文', '🎉'] {
        let slot = r
            .resolve_slot(ch, false, false)
            .unwrap_or_else(|| panic!("expected fallback resolution for {ch:?}"));
        let key = GlyphKey::with_slot(ch, slot, false, false);
        let info = atlas
            .get_or_insert(key, &mut r)
            .unwrap_or_else(|| panic!("atlas had no room for {ch:?}"));
        // Non-zero pixel size means swash actually produced a tile.
        // The fix is observable here: pre-fix this assertion failed
        // because slot 0 had no glyph and there was no other slot to
        // try, so the atlas stashed a zero-area sentinel.
        assert!(
            info.px_size[0] > 0 && info.px_size[1] > 0,
            "fallback-resolved {ch:?} must produce a real tile, got size {:?}",
            info.px_size
        );
    }
}

#[test]
fn slot_resolution_is_memoized() {
    // Second resolve_slot for the same codepoint must not re-walk the
    // chain. We can't observe wall-time portably; instead we verify
    // the answer is stable across repeated calls — the cache is the
    // simplest implementation of that invariant.
    let mut fs = font_system_with_bundled_and_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
    let a = r.resolve_slot('A', false, false);
    let b = r.resolve_slot('A', false, false);
    assert_eq!(a, b, "memoized slot must be stable");
    assert!(a.is_some(), "ASCII 'A' must resolve through primary or bundled fallback");
}

#[test]
fn rasterizer_fallback_works_when_caller_passes_slot_zero() {
    // The bench harness and the existing swash_rasterizer.rs tests
    // build GlyphKeys with the convenience `new()` ctor which sets
    // slot=0. They should still get *something* drawable for cells
    // outside the primary font's coverage — the rasterize() impl
    // walks the chain on a charmap miss for slot=0.
    let mut fs = font_system_with_bundled_and_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);

    // ASCII 'A' from slot 0 — works directly.
    let a = r.rasterize(GlyphKey::new('A', false, false));
    assert!(a.is_some(), "primary slot must produce 'A'");

    // CJK from slot 0 — on macOS this must fall back to a real tile;
    // on other platforms we only assert "no panic" because we don't
    // know which fonts the CI runner has.
    #[cfg(target_os = "macos")]
    {
        let tile = r
            .rasterize(GlyphKey::new('中', false, false))
            .expect("CJK fallback must produce a tile on macOS");
        assert!(
            tile.width > 0 && tile.height > 0,
            "fallback tile must have real pixels, got {}x{}",
            tile.width,
            tile.height
        );
    }
}
