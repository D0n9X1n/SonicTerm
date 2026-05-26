//! Stable per-glyph identity used by the GPU glyph atlas.
//!
//! A `GlyphKey` is everything the rasterizer needs to know to decide
//! whether two cells produce the *same* alpha-mask tile in the atlas:
//! the character, and the two attributes that change the rendered glyph
//! shape (bold weight, italic style). Color is deliberately NOT part of
//! the key — the atlas stores 8-bit coverage, and the text pipeline
//! multiplies in the per-cell foreground color at sample time. Color is
//! a property of the *instance*, not of the tile.
//!
//! `GlyphKey` is `Copy + Hash + Eq` so it can be used as a `HashMap`
//! key cheaply; the type fits in a single machine word on 64-bit
//! targets (4 bytes char + 1 byte bool packed flags + padding).
//!
//! `from_cell` is the canonical constructor: it deliberately maps
//! `CellFlags::WIDE_CONT` cells to `None` so callers that walk a grid
//! skip the right half of a wide glyph without an explicit branch in
//! the hot loop. Wide-cell *leads* and ordinary single-width cells are
//! both `Some(key)`; the atlas treats the lead the same as a normal
//! cell — the rasterizer rasterizes the full glyph and the renderer
//! draws it at the wider rect spanning both columns.
//!
//! `Color` (fg/bg) is intentionally NOT part of `GlyphKey`. Mixing color
//! into the key would explode the working-set size of the atlas: a
//! typical shell prompt uses ~10 distinct colors × ~96 ASCII glyphs =
//! ~960 tiles instead of ~96. The user payoff of B3 is the high hit
//! rate, and that comes from collapsing those color variants into a
//! single mask.

use crate::grid::{Cell, CellFlags};

/// Stable identity of an atlas glyph tile. See module docs.
///
/// `font_slot` names which entry in the rasterizer's fallback chain
/// owns this glyph. Two faces rendering the same codepoint (e.g.
/// primary Rec Mono Casual which has no '中', and a PingFang SC fallback
/// which does) cache separately under this scheme — without `font_slot`
/// the atlas could not express the fallback at all.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct GlyphKey {
    /// The rendered character.
    pub ch: char,
    /// Index into the rasterizer's font fallback chain. `0` is the
    /// primary configured family; higher values are platform-specific
    /// fallbacks (PingFang SC, Apple Color Emoji, Microsoft YaHei, …).
    /// One byte: realistic chains are 3–6 entries.
    pub font_slot: u8,
    /// True when the cell carries `CellFlags::BOLD`. Bold and non-bold
    /// share the same glyph face from the rasterizer's point of view
    /// only if the font has no bold variant; we still key on the flag
    /// so that font-faces that DO have a bold cut don't get smushed
    /// together.
    pub weight_bold: bool,
    /// True when the cell carries `CellFlags::ITALIC`.
    pub italic: bool,
}

impl GlyphKey {
    /// Derive the key for a cell. Pre-fallback: the caller fills in
    /// `font_slot = 0` (primary) and the rasterizer may retry with
    /// higher slots when the primary lacks the glyph (see
    /// `SwashRasterizer::resolve_slot`).
    ///
    /// Returns `None` for cells the renderer should *not* request a glyph
    /// for: wide-glyph continuation cells (the right half of a CJK
    /// character, etc). These cells exist in the grid so cursor math and
    /// width tracking work, but their `ch` is a placeholder space and
    /// they must not produce an atlas tile of their own — the lead cell
    /// already covers them.
    #[inline]
    pub fn from_cell(c: &Cell) -> Option<Self> {
        if c.flags.contains(CellFlags::WIDE_CONT) {
            return None;
        }
        Some(Self {
            ch: c.ch,
            font_slot: 0,
            weight_bold: c.flags.contains(CellFlags::BOLD),
            italic: c.flags.contains(CellFlags::ITALIC),
        })
    }

    /// Convenience constructor for tests and the bench harness.
    /// Defaults to `font_slot = 0` (primary family).
    #[inline]
    pub fn new(ch: char, weight_bold: bool, italic: bool) -> Self {
        Self { ch, font_slot: 0, weight_bold, italic }
    }

    /// Constructor pinning a specific font slot. Used by the
    /// rasterizer's fallback loop and by tests that need to verify
    /// tiles from different fonts cache separately.
    #[inline]
    pub fn with_slot(ch: char, font_slot: u8, weight_bold: bool, italic: bool) -> Self {
        Self { ch, font_slot, weight_bold, italic }
    }

    /// Return a new key with `font_slot` replaced. Lets the rasterizer
    /// walk the fallback chain without mutating the caller's key.
    #[inline]
    #[must_use]
    pub fn with_font_slot(self, font_slot: u8) -> Self {
        Self { font_slot, ..self }
    }
}
