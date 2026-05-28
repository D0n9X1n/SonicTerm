//! Cell value types: color, attribute flags, single grid cell.

use serde::{Deserialize, Serialize};

use crate::hyperlink_id::HyperlinkId;

/// 24-bit RGB color or an indexed palette slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Color {
    /// The terminal's default foreground or background color.
    #[default]
    Default,
    /// An indexed palette slot (0–255 ANSI/xterm palette).
    Indexed(u8),
    /// A 24-bit truecolor RGB triple.
    Rgb(u8, u8, u8),
}

bitflags::bitflags! {
    /// SGR-derived attribute flags carried per cell.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u16 {
        /// Bold weight.
        const BOLD          = 1 << 0;
        /// Italic style.
        const ITALIC        = 1 << 1;
        /// Underline decoration.
        const UNDERLINE     = 1 << 2;
        /// Strike-through decoration.
        const STRIKETHROUGH = 1 << 3;
        /// Swap foreground and background.
        const INVERSE       = 1 << 4;
        /// Dim / faint intensity.
        const DIM           = 1 << 5;
        /// Hidden / concealed.
        const HIDDEN        = 1 << 6;
        /// Blinking text.
        const BLINK         = 1 << 7;
        /// Wide cell (occupies 2 columns)
        const WIDE          = 1 << 8;
        /// Continuation of a wide cell (right half)
        const WIDE_CONT     = 1 << 9;
    }
}

/// A single grid cell.
///
/// `extras` stores trailing zero-width codepoints (zero-width joiners
/// U+200D and combining marks) that follow the lead `ch` and must be
/// shaped together with it as part of the same cluster. ZWJ sequences
/// like 👨‍👩‍👧 (MAN + ZWJ + WOMAN + ZWJ + GIRL) reach the grid as
/// five separate `put_char` calls; the four zero-width codepoints
/// (ZWJs + each subsequent emoji's invisible joiners are zero-width
/// per `unicode-width`) get appended to the lead cell's `extras` so
/// the shaper sees the full cluster on a single shape pass. Boxed so
/// the common case (no extras) costs one machine word per cell, not
/// the 24-byte footprint of an inline `Vec<char>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    /// The lead character rendered in this cell.
    pub ch: char,
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// SGR attribute flags (bold, italic, wide, …).
    pub flags: CellFlags,
    /// Optional OSC-8 hyperlink id this cell belongs to.
    pub hyperlink: Option<HyperlinkId>,
    /// Trailing zero-width codepoints (ZWJ, combining marks) that
    /// belong to this cluster, encoded as UTF-8. `None` for the
    /// overwhelming majority of cells (plain ASCII, single emoji,
    /// single CJK glyph). `Box<str>` (rather than `String`) keeps the
    /// footprint at two machine words when present and zero
    /// allocations beyond the boxed slice itself.
    pub extras: Option<Box<str>>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: None,
            extras: None,
        }
    }
}
