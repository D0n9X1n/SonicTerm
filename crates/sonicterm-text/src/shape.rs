//! Cell-row → shaped-glyph run data model.
//!
//! The shaping backend lives above this crate in `sonicterm-engine`.
//! This crate keeps the Sonic-facing run style and shaped-glyph records
//! used by the atlas/cache/renderer layers. That split keeps
//! `sonicterm-text` free of upstream WezTerm types while the engine
//! conversion continues.
//!
//! - the ASCII fast-path gate ([`run_is_ascii_fast`]) — purely
//!   cell-based; lets the renderer skip the shape call for the
//!   steady-state interactive shell where every cell is plain ASCII
//!   without ligature triggers, and
//! - the [`ShapedGlyph`] struct — a narrowed projection containing exactly
//!   the fields the GPU emit loop needs.

use sonicterm_types::{Cell, CellFlags};

/// Characters that commonly participate in programming ligatures across
/// the fonts SonicTerm ships (Rec Mono St.Helens, JetBrains Mono). If a
/// run contains ANY of these, the ASCII fast path must defer to the
/// shaper so contextual GSUB substitutions (`=>`, `!=`, `>=`, `->`,
/// `<-`, `::`, `||`, `&&`, etc.) actually render as the composed
/// ligature glyph instead of two separate cells.
///
/// Kept deliberately small — adding harmless ASCII (digits, letters)
/// here would needlessly defeat the fast path.
#[inline]
fn is_ligature_trigger(b: u8) -> bool {
    matches!(b, b'=' | b'!' | b'<' | b'>' | b'-' | b'_' | b':' | b'|' | b'&' | b'*')
}

/// True when every cell in `cells` is a plain printable-ASCII codepoint
/// (0x20..=0x7E) with no `extras` cluster AND the run contains none of
/// the characters that commonly trigger programming ligatures. The
/// renderer can then bypass sonicterm-font shaping entirely and emit one
/// `GlyphKey` per cell via the pre-shaping char→glyph lookup path.
/// ASCII shells (the steady-state for almost every interactive session)
/// hit this hundreds of times per frame; shaping was previously running
/// unconditionally.
///
/// Runs that contain a ligature-trigger byte (`=`, `!`, `<`, `>`, `-`,
/// `_`, `:`, `|`, `&`, `*`) MUST go through the shaper even when
/// otherwise pure ASCII — `=>`, `!=`, `>=`, `->`, `::`, `||`, `&&`,
/// etc. are pure ASCII and would otherwise silently miss ligature
/// shaping in the actual render path. The bias is deliberately toward
/// "shape it" — a few extra sonicterm-font calls on prompts containing
/// `=` cost less than a wrong rendering.
#[inline]
pub fn run_is_ascii_fast(cells: &[(u16, Cell)]) -> bool {
    cells.iter().all(|(_, c)| {
        c.extras().is_none()
            && {
                let n = c.ch as u32;
                (0x20..=0x7E).contains(&n) && !is_ligature_trigger(n as u8)
            }
            // Reject anything carrying cluster intent through a flag
            // we don't model in the fast path. WIDE_CONT shouldn't
            // reach a run at all (caller filters), but be defensive.
            && !c.flags.contains(CellFlags::WIDE_CONT)
            && !c.flags.contains(CellFlags::WIDE)
    })
}

/// One glyph the shaper produced for a style run.
///
/// This is a narrowed projection holding exactly the fields the sonicterm GPU
/// emit loop reads:
///
/// - `lead_col` + `cluster_cells` — where to paint the glyph in the
///   cell grid and how many cells the cluster spans.
/// - `font_slot` — index into the renderer's resolved fallback chain
///   (sourced from sonicterm-font's `FallbackIdx`, narrowed to `u8`).
///   Used as part of the atlas key so two faces never collide on the
///   same glyph id.
/// - `glyph_id` — freetype glyph index inside the resolved face
///   (`GlyphInfo::glyph_pos`). `0` means notdef; caller should treat
///   as missing.
/// - `x_advance`, `y_offset` — raster-px metrics for instance
///   placement; come straight from wezterm's `PixelLength`
///   (`.get() as f32`).
/// - `ch` — first codepoint of the cluster, kept purely for tofu /
///   shaper-trace diagnostics. Not consumed by the atlas key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// Column of the **lead cell** of the cluster this glyph belongs
    /// to. For 1:1 (plain ASCII) this is just the cell's own column;
    /// for a ligature (`=>` over two cells) both glyphs point at the
    /// leftmost cell — when there's only one glyph for the cluster,
    /// the second cell gets *no* glyph and rasterizes as a blank.
    pub lead_col: u16,
    /// Number of source cells this cluster collapses. `1` = plain 1:1;
    /// `>1` = ligature / ZWJ / wide glyph. sonicterm-font sources this
    /// from `GlyphInfo::num_cells` so the value is wezterm-authoritative.
    pub cluster_cells: u16,
    /// Resolved slot in the renderer's fallback chain. sonicterm-font's
    /// `FallbackIdx` (a `usize`) narrowed to `u8`; saturates at 255
    /// (the renderer's fallback chain is bounded well below that).
    pub font_slot: u8,
    /// Freetype glyph id inside the resolved face
    /// (`GlyphInfo::glyph_pos`). `0` = notdef.
    pub glyph_id: u32,
    /// Shaper-reported advance width in raster px
    /// (`GlyphInfo::x_advance.get() as f32`).
    pub x_advance: f32,
    /// Shaper-reported vertical offset in raster px
    /// (`GlyphInfo::y_offset.get() as f32`). Positive = down per
    /// sonicterm-font convention.
    pub y_offset: f32,
    /// First codepoint of the cluster — informational, used by tofu
    /// diagnostics and shaper-trace logging. Not part of the atlas
    /// key.
    pub ch: char,
}

/// Style a [`Cell`] contributes to a shape run. `(bold, italic)` —
/// these are the two attributes a font shaper *re-resolves* the face
/// for, so they bracket a single shape pass. Color is per-instance,
/// not per-glyph, so it's not part of the style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStyle {
    /// True if the run is bold.
    pub bold: bool,
    /// True if the run is italic.
    pub italic: bool,
}

impl RunStyle {
    /// Pull the (bold, italic) bits out of a cell.
    pub fn from_cell(c: &Cell) -> Self {
        Self {
            bold: c.flags.contains(CellFlags::BOLD),
            italic: c.flags.contains(CellFlags::ITALIC),
        }
    }
}
