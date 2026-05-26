//! Renderer-level capability matrix — proves every character class we
//! claim to support actually rasterizes to a non-blank tile.
//!
//! Why this exists: PR #42's B3 cutover routed every cell through the
//! swash rasterizer, which has NO font fallback — if the configured
//! family doesn't have a glyph for `ch`, `SwashRasterizer::rasterize`
//! returns `None`, the atlas records a zero-area tile, `render.rs`
//! treats the cell as a "tofu" (missing) cell, and the user sees an
//! empty box. The pty_dump e2e and every existing test only used ASCII
//! so the regression slipped through.
//!
//! These tests probe the rasterizer + atlas directly — the same code
//! path the renderer uses on every frame — and assert that for every
//! character class we promise to support, `glyph_atlas.get_or_insert`
//! returns a tile with non-zero pixel dimensions. A failure pinpoints
//! exactly which class regressed.
//!
//! ## Failure semantics
//!
//! - A test that returns `None` from the rasterizer / zero-sized tile
//!   from the atlas → the renderer would draw a tofu box for that
//!   character. This is the CJK-tofu bug PR #42 shipped.
//!
//! ## Status
//!
//! - ASCII, Latin-1, box-drawing, powerline (Nerd Font PUA), and the
//!   fullwidth Latin block are bundled in `Rec Mono Casual` so those
//!   tests must pass on every commit.
//! - CJK Unified Ideographs, Hiragana / Katakana, Hangul, and emoji
//!   are NOT in `Rec Mono Casual`. They require the font-fallback path
//!   landing in `fix/atlas-font-fallback` to pass; until then they're
//!   `#[ignore]` with an explicit removal note. Removing the `#[ignore]`
//!   in that PR's rebase is the canonical green light that the fix
//!   shipped.

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::{
    glyph_atlas::GlyphAtlas,
    swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX},
};

/// Build a `FontSystem` populated with every TTF/OTF under
/// `assets/fonts/` — the same loader used by the real renderer.
fn font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
            let bytes = std::fs::read(&p).unwrap();
            fs.db_mut().load_font_data(bytes);
        }
    }
    fs
}

/// True iff `ch` rasterizes to a tile with non-zero pixel dimensions
/// through the production rasterizer + atlas path. Whitespace returns
/// `false` because the rasterizer (correctly) short-circuits it to a
/// zero-area tile — callers should filter whitespace out.
fn rasterizes(ch: char) -> bool {
    let mut fs = font_system();
    let mut atlas = GlyphAtlas::default_size();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let Some(info) = atlas.get_or_insert(GlyphKey::new(ch, false, false), &mut r) else {
        return false;
    };
    info.px_size[0] > 0 && info.px_size[1] > 0
}

/// Assert every char in `s` rasterizes. Skips whitespace because the
/// rasterizer short-circuits those (intentionally). Panics with a
/// human-readable list of which characters failed so the user can see
/// at a glance which class regressed.
fn assert_all_rasterize(class: &str, s: &str) {
    let missing: Vec<char> = s.chars().filter(|c| !c.is_whitespace() && !rasterizes(*c)).collect();
    assert!(
        missing.is_empty(),
        "[{class}] {} of {} chars rasterized to a blank tile (renderer would draw tofu boxes): {:?}",
        missing.len(),
        s.chars().filter(|c| !c.is_whitespace()).count(),
        missing
    );
}

// -----------------------------------------------------------------------
// PASSING CLASSES — bundled in Rec Mono Casual's primary face. Must
// stay green on every commit.
// -----------------------------------------------------------------------

#[test]
fn rasterizes_ascii_printable() {
    let s = "Hello, World! 0123456789 ~`!@#$%^&*()_+-=[]{}|;:'\",.<>/?";
    assert_all_rasterize("ascii_printable", s);
}

#[test]
fn rasterizes_latin1_supplement() {
    assert_all_rasterize("latin1_supplement", "café niño über ÆØÅ");
}

// -----------------------------------------------------------------------
// CLASSES THAT REQUIRE THE FONT-FALLBACK FIX (fix/atlas-font-fallback).
//
// On main today these FAIL because SwashRasterizer returns None when
// glyph_id == 0 in the configured family and has no sibling-family
// fallback. The exact code is sonic-shared/src/swash_rasterizer.rs §
// `if glyph_id == 0 { return None; }`.
//
// The breakage is broader than the CJK report — box-drawing, the
// Nerd-Font Powerline PUA range, and fullwidth Latin also need
// fallback because they live in sibling families inside the bundled
// font pack, not in the primary "Rec Mono Casual" face that the
// rasterizer hard-codes today.
//
// Remove the `#[ignore]` when fix/atlas-font-fallback lands. THE TEST
// BODIES ARE COMPLETE — the only thing the fix needs is to make
// `SwashRasterizer::rasterize` route through a fallback font when the
// primary family doesn't have a glyph.
// -----------------------------------------------------------------------

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_box_drawing() {
    assert_all_rasterize("box_drawing", "─╭╮╯╰│┤├┬┴┼");
}

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_powerline_pua() {
    // Canonical Nerd Font set used by most shell prompts.
    assert_all_rasterize("powerline_pua", "\u{e0b0}\u{e0b2}\u{e0a0}\u{f015}");
}

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_fullwidth_ascii() {
    // FULLWIDTH LEFT/RIGHT SQUARE BRACKET.
    assert_all_rasterize("fullwidth_ascii", "［］");
}

// -----------------------------------------------------------------------
// CJK / emoji — also gated on fix/atlas-font-fallback. These are the
// classes from the PR #42 bug report.
// -----------------------------------------------------------------------

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase. This is the test that would have caught the CJK-tofu regression on PR #42."]
fn rasterizes_cjk_unified_ideographs() {
    assert_all_rasterize("cjk_unified_ideographs", "中文测試");
}

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_hiragana() {
    assert_all_rasterize("hiragana", "ひらがな");
}

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_katakana() {
    assert_all_rasterize("katakana", "カタカナ");
}

#[test]
#[ignore = "Requires font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_hangul() {
    assert_all_rasterize("hangul", "한국어");
}

#[test]
#[ignore = "Requires emoji-font fallback (fix/atlas-font-fallback). Remove #[ignore] in that PR's rebase."]
fn rasterizes_emoji_single_codepoint() {
    assert_all_rasterize("emoji_single_codepoint", "🎉🚀");
}

#[test]
#[ignore = "Requires emoji-font fallback (fix/atlas-font-fallback) AND shaping for ZWJ sequences. The ZWJ scalar itself is zero-width so we only assert the base emoji rasterize."]
fn rasterizes_emoji_zwj_components() {
    // We don't (yet) draw the family as one cluster; what must work is
    // every base emoji rasterizes to a non-blank tile so the user sees
    // 'man woman girl' rather than three tofus.
    assert_all_rasterize("emoji_zwj_components", "👨👩👧");
}
