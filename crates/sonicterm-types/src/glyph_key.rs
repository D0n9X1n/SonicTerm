//! Stable per-glyph identity used by the GPU glyph atlas.
//!
//! See `sonicterm-core::glyph_key` for the full design discussion (kept in the
//! original module for historical link continuity). The type itself lives
//! here so non-engine crates can carry a `GlyphKey` without depending on
//! `sonicterm-core`.

use crate::cell::{Cell, CellFlags};

/// Native raster role of a glyph tile within one renderer atlas lifetime.
#[repr(u8)]
#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug)]
pub enum GlyphRasterVariant {
    /// Terminal text and chrome rendered at the configured body size.
    Normal,
    /// Command-palette footer rendered natively one logical pixel smaller.
    PaletteFooter,
    /// Tab titles rendered natively one logical pixel larger.
    TabTitle,
}

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
    /// Uses `u32` because FreeType glyph indices can exceed `u16` for large
    /// fonts such as CJK families.
    pub glyph_id: u32,
    /// Native raster role. Roles with different point sizes must not share one
    /// atlas entry even when their font slot and glyph id match.
    pub raster_variant: GlyphRasterVariant,
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
            // When: `WIDE_CONT` marks the trailing half of a wide glyph, which must not allocate a separate atlas tile.
            return None;
        }
        Some(Self {
            ch: c.ch,
            font_slot: 0,
            weight_bold: c.flags.contains(CellFlags::BOLD),
            italic: c.flags.contains(CellFlags::ITALIC),
            glyph_id: 0,
            raster_variant: GlyphRasterVariant::Normal,
        })
    }

    /// Convenience constructor for tests.
    #[inline]
    pub fn new(ch: char, weight_bold: bool, italic: bool) -> Self {
        Self {
            ch,
            font_slot: 0,
            weight_bold,
            italic,
            glyph_id: 0,
            raster_variant: GlyphRasterVariant::Normal,
        }
    }

    /// Constructor pinning a specific font slot.
    #[inline]
    pub fn with_slot(ch: char, font_slot: u8, weight_bold: bool, italic: bool) -> Self {
        Self {
            ch,
            font_slot,
            weight_bold,
            italic,
            glyph_id: 0,
            raster_variant: GlyphRasterVariant::Normal,
        }
    }

    /// Constructor for a *shaped* glyph: identity comes from
    /// `(font_slot, glyph_id, weight_bold, italic)`, not the codepoint.
    #[inline]
    pub fn shaped(ch: char, font_slot: u8, glyph_id: u32, weight_bold: bool, italic: bool) -> Self {
        Self {
            ch,
            font_slot,
            weight_bold,
            italic,
            glyph_id,
            raster_variant: GlyphRasterVariant::Normal,
        }
    }

    /// Return a new key with `font_slot` replaced.
    #[inline]
    #[must_use]
    pub fn with_font_slot(self, font_slot: u8) -> Self {
        Self { font_slot, ..self }
    }

    /// Return a new key assigned to a native raster role.
    #[inline]
    #[must_use]
    pub fn with_raster_variant(self, raster_variant: GlyphRasterVariant) -> Self {
        Self { raster_variant, ..self }
    }
}
