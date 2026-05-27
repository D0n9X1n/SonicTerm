//! Cell value types: color, attribute flags, single grid cell.

use serde::{Deserialize, Serialize};

use crate::hyperlink_id::HyperlinkId;

/// 24-bit RGB color or an indexed palette slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u16 {
        const BOLD          = 1 << 0;
        const ITALIC        = 1 << 1;
        const UNDERLINE     = 1 << 2;
        const STRIKETHROUGH = 1 << 3;
        const INVERSE       = 1 << 4;
        const DIM           = 1 << 5;
        const HIDDEN        = 1 << 6;
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
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
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
