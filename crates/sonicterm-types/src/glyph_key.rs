//! Stable per-glyph identity used by the GPU glyph atlas.
//!
//! See `sonicterm-core::glyph_key` for the full design discussion (kept in the
//! original module for historical link continuity). The type itself lives
//! here so non-engine crates can carry a `GlyphKey` without depending on
//! `sonicterm-core`.

use crate::cell::{Cell, CellFlags};

/// Stable identity of an atlas glyph tile.
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub struct GlyphKey {
    /// The rendered character. For shaped keys (`glyph_id != 0`) this is
    /// informational — it carries the *first* codepoint of the cluster
    /// that produced the shaped glyph and is useful for diagnostics, but
    /// the rasterizer ignores it in favor of `glyph_id`.
    pub ch: char,
    /// Index into the rasterizer's font fallback chain. `0` is the
    /// primary configured family; higher values are platform-specific
    /// fallbacks (PingFang SC, Apple Color Emoji, Microsoft YaHei, …).
    pub font_slot: u8,
    /// True when the cell carries `CellFlags::BOLD`.
    pub weight_bold: bool,
    /// True when the cell carries `CellFlags::ITALIC`.
    pub italic: bool,
    /// Pre-shaped glyph identifier inside the resolved font. `0` is
    /// reserved as the "no shaping was used" sentinel — the rasterizer
    /// falls back to the char-based charmap lookup in that case.
    ///
    /// Widened to u32 to hold sonicterm-font freetype glyph indices
    /// (Phase 4) which can exceed u16 for large fonts (e.g. CJK).
    pub glyph_id: u32,
}

impl GlyphKey {
    /// Derive the key for a cell. Pre-fallback: the caller fills in
    /// `font_slot = 0` (primary) and the rasterizer may retry with
    /// higher slots when the primary lacks the glyph.
    ///
    /// Returns `None` for cells the renderer should *not* request a glyph
    /// for: wide-glyph continuation cells (the right half of a CJK
    /// character, etc).
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
            glyph_id: 0,
        })
    }

    /// Convenience constructor for tests.
    #[inline]
    pub fn new(ch: char, weight_bold: bool, italic: bool) -> Self {
        Self { ch, font_slot: 0, weight_bold, italic, glyph_id: 0 }
    }

    /// Constructor pinning a specific font slot.
    #[inline]
    pub fn with_slot(ch: char, font_slot: u8, weight_bold: bool, italic: bool) -> Self {
        Self { ch, font_slot, weight_bold, italic, glyph_id: 0 }
    }

    /// Constructor for a *shaped* glyph: identity comes from
    /// `(font_slot, glyph_id, weight_bold, italic)`, not the codepoint.
    #[inline]
    pub fn shaped(ch: char, font_slot: u8, glyph_id: u32, weight_bold: bool, italic: bool) -> Self {
        Self { ch, font_slot, weight_bold, italic, glyph_id }
    }

    /// Return a new key with `font_slot` replaced.
    #[inline]
    #[must_use]
    pub fn with_font_slot(self, font_slot: u8) -> Self {
        Self { font_slot, ..self }
    }
}
