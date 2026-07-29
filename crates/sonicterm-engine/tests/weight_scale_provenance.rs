//! Weight scaling must act on the configured family, and identify it by
//! provenance rather than by handle index.
//!
//! This lives in `tests/` rather than beside the unit tests because it needs a
//! live `FontStack` built with a family that cannot resolve. `install_default_config`
//! installs the primary family into a process-wide slot exactly once, so a test
//! that installs a deliberately-missing family would decide the primary for
//! every sibling test sharing the binary — and which one ran first would decide
//! the outcome. An integration test compiles to its own binary, so the
//! process-global install is scoped to this file alone.
//!
//! What it pins that the unit tests cannot: those drive the predicate
//! directly, so a build whose *call site* reverted to `font_idx == 0` passes
//! all of them. The mismatch only appears against a real resolution where the
//! configured family is absent and a fallback has inherited index 0.

use sonicterm_engine::FontStack;
use sonicterm_text::glyph_atlas::Rasterizer;
use sonicterm_types::glyph_key::GlyphKey;

/// A family name no font can satisfy, so resolution must fall through.
const MISSING_FAMILY: &str = "SonicTermNoSuchFamily-A7F3E1";

/// Well past 1.0, so outline growth is unambiguous if it happens at all.
const HEAVY: f32 = 3.0;

fn tile_for(family: &str, weight: f32, ch: char) -> Option<(u32, u32)> {
    let mut stack = FontStack::try_new_full_with_weight(family, 14.0, 72, weight).ok()?;
    let tile = stack.rasterize(GlyphKey {
        ch,
        font_slot: 0,
        weight_bold: false,
        italic: false,
        // Zero forces the re-shape branch, which resolves the real handle
        // index instead of trusting the slot we passed in.
        glyph_id: 0,
    })?;
    Some((tile.width, tile.height))
}

/// A glyph drawn from a fallback must not grow when the weight is raised,
/// even when that fallback occupies handle index 0.
///
/// With the configured family unresolvable, the first fallback is pushed into
/// index 0. A gate that reads the index calls that "the configured family" and
/// emboldens it; a gate that asks the font for its provenance does not. The
/// two answers differ only here, which is what makes this the test that
/// distinguishes them.
#[test]
fn a_fallback_at_index_zero_is_not_reweighted() {
    // A Powerline separator, from the Private Use Area. The character has to
    // be one the font at index 0 actually contains: a Latin letter falls
    // through to a *later* handle, where an index gate and a provenance gate
    // agree and the mutant survives. With the configured family missing, the
    // first declared fallback takes index 0, and a PUA symbol is what it
    // covers.
    const PUA_SYMBOL: char = '\u{e0b0}';
    let Some(base) = tile_for(MISSING_FAMILY, 1.0, PUA_SYMBOL) else {
        // No font stack available in this environment; nothing to assert.
        return;
    };
    let Some(heavy) = tile_for(MISSING_FAMILY, HEAVY, PUA_SYMBOL) else {
        return;
    };

    assert_eq!(
        heavy, base,
        "the configured family could not resolve, so every glyph here comes from a \
         fallback and none may be reweighted — but raising the weight to {HEAVY} grew the \
         tile from {base:?} to {heavy:?}, which is what an index-based gate does when a \
         fallback inherits index 0"
    );
}

/// The complement: with a family that *does* resolve, the weight must still
/// act on it.
///
/// Without this, the test above is satisfied by a build that disabled weight
/// scaling altogether — trading a wrong-glyph defect for a dead setting.
#[test]
fn the_configured_family_is_still_reweighted() {
    // Mirrors `fontstack::DEFAULT_FONT_FAMILY`, which is not re-exported.
    // Widening the crate's public surface to serve a test would be the
    // wrong trade; if the default ever changes, this resolves to nothing
    // and the test skips rather than asserting something false.
    let family = "Rec Mono St.Helens";
    let Some(base) = tile_for(family, 1.0, 'm') else {
        return;
    };
    let Some(heavy) = tile_for(family, HEAVY, 'm') else {
        return;
    };

    assert!(
        heavy.0 >= base.0 && heavy.1 >= base.1 && heavy != base,
        "raising the weight to {HEAVY} on the configured family must grow the glyph \
         outline, but the tile went from {base:?} to {heavy:?} — a gate that excluded \
         everything would satisfy the fallback test while leaving the setting inert"
    );
}
