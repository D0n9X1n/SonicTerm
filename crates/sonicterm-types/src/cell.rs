//! Cell value types: color, attribute flags, single grid cell.
//!
//! # Compact layout (Epic #300 P1)
//!
//! [`Cell`] is sized to fit in **24 bytes** on a 64-bit target. The hot
//! fields (`ch`, `fg`, `bg`, `flags`) stay inline; the rare attributes
//! (OSC-8 hyperlink id and trailing zero-width codepoints that form a
//! grapheme cluster with `ch`) are externalized to
//! [`FatAttributes`] behind an [`Option<Box<FatAttributes>>`]. A default
//! cell does **not** allocate — the box is only materialized the first
//! time a rare attribute is set.
//!
//! The shape mirrors WezTerm's `wezterm-cell::CellAttributes`
//! externalization pattern: pay 8 bytes (one nullable pointer) for the
//! 99 %+ of cells that have no hyperlink and no combining extras, and
//! pay the full fat allocation only for the few that do.

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

/// Terminal underline variant selected by SGR 4 / 4:n.
///
/// This mirrors the underline styles common to xterm and WezTerm:
/// single, double, curly, dotted, and dashed. The default rendering style is
/// [`UnderlineStyle::Single`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum UnderlineStyle {
    /// A single straight underline.
    #[default]
    Single,
    /// Two straight underline strokes.
    Double,
    /// A curly / wavy underline.
    Curly,
    /// A dotted underline.
    Dotted,
    /// A dashed underline.
    Dashed,
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

/// Rare per-cell attributes externalized behind a `Box` to keep the
/// hot [`Cell`] at 24 bytes.
///
/// Only allocated the first time a cell needs to carry a hyperlink
/// or trailing zero-width codepoints (combining marks, ZWJ sequences,
/// variation selectors). The default cell — plain ASCII space, no
/// link, no extras — leaves [`Cell::fat`] as `None` and pays nothing
/// beyond the inline pointer slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FatAttributes {
    /// Optional OSC-8 hyperlink id this cell belongs to.
    pub hyperlink: Option<HyperlinkId>,
    /// Trailing zero-width codepoints (ZWJ, combining marks) that
    /// belong to this cluster, encoded as UTF-8. ZWJ sequences like
    /// 👨‍👩‍👧 (MAN + ZWJ + WOMAN + ZWJ + GIRL) arrive as five
    /// separate `put_char` calls; the four zero-width codepoints
    /// get appended here so the shaper sees the full cluster on a
    /// single shape pass.
    pub extras: Option<Box<str>>,
    /// Optional non-default underline style. The default single underline
    /// stays implicit so normal underlined cells do not allocate just for
    /// style metadata.
    pub underline_style: Option<UnderlineStyle>,
    /// Optional underline colour from SGR 58. `None` means underline follows
    /// the cell foreground, matching terminal defaults.
    pub underline_color: Option<Color>,
}

impl FatAttributes {
    /// Return `true` when neither hyperlink nor extras carry data,
    /// i.e. dropping the box would lose nothing. Used by setters to
    /// re-collapse to `None` when the last rare attribute clears.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.hyperlink.is_none()
            && self.extras.is_none()
            && self.underline_style.is_none()
            && self.underline_color.is_none()
    }
}

/// A single grid cell.
///
/// Size goal (Epic #300 P1): **`size_of::<Cell>() <= 24`** on 64-bit
/// targets. Asserted in `tests/cell_size.rs`. The four inline fields
/// account for 12 bytes (4 + 4 + 4 + 2) with 2 bytes of trailing pad
/// before the 8-byte [`Option<Box<FatAttributes>>`]; total 24.
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
    /// Externalized rare attributes (hyperlink, grapheme cluster
    /// extras). `None` for the overwhelming majority of cells.
    /// Access via [`Cell::hyperlink`] / [`Cell::extras`] /
    /// [`Cell::set_hyperlink`] / [`Cell::set_extras`] /
    /// [`Cell::take_extras`] — direct access is intentionally
    /// limited to keep the box lifecycle (alloc on first rare
    /// attr, collapse to `None` when last clears) in one place.
    fat: Option<Box<FatAttributes>>,
}

impl Cell {
    /// Construct a plain cell with no rare attributes. Equivalent
    /// to `Cell { ch, fg, bg, flags, fat: None }`. Does **not**
    /// allocate.
    #[inline]
    pub fn plain(ch: char, fg: Color, bg: Color, flags: CellFlags) -> Self {
        Cell { ch, fg, bg, flags, fat: None }
    }

    /// Read the OSC-8 hyperlink id, if any.
    #[inline]
    pub fn hyperlink(&self) -> Option<HyperlinkId> {
        self.fat.as_ref().and_then(|f| f.hyperlink)
    }

    /// Read the trailing zero-width codepoint cluster, if any.
    #[inline]
    pub fn extras(&self) -> Option<&str> {
        self.fat.as_ref().and_then(|f| f.extras.as_deref())
    }

    /// Read this cell's underline style. Cells without a fat style use the
    /// terminal default: a single straight underline.
    #[inline]
    pub fn underline_style(&self) -> UnderlineStyle {
        self.fat.as_ref().and_then(|f| f.underline_style).unwrap_or(UnderlineStyle::Single)
    }

    /// Read this cell's explicit underline colour, if SGR 58 set one.
    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        self.fat.as_ref().and_then(|f| f.underline_color)
    }

    /// Set the hyperlink id, allocating [`FatAttributes`] on first
    /// rare-attr write. Passing `None` clears the field and, if no
    /// other fat attribute remains, drops the box.
    #[inline]
    pub fn set_hyperlink(&mut self, id: Option<HyperlinkId>) {
        match (&mut self.fat, id) {
            (Some(fat), id) => {
                fat.hyperlink = id;
                if fat.is_empty() {
                    self.fat = None;
                }
            }
            (None, Some(id)) => {
                self.fat = Some(Box::new(FatAttributes {
                    hyperlink: Some(id),
                    extras: None,
                    underline_style: None,
                    underline_color: None,
                }));
            }
            (None, None) => {}
        }
    }

    /// Set the extras cluster, allocating on first rare-attr write.
    /// Passing `None` clears and collapses to `None` if otherwise empty.
    #[inline]
    pub fn set_extras(&mut self, extras: Option<Box<str>>) {
        match (&mut self.fat, extras) {
            (Some(fat), ex) => {
                fat.extras = ex;
                if fat.is_empty() {
                    self.fat = None;
                }
            }
            (None, Some(ex)) => {
                self.fat = Some(Box::new(FatAttributes {
                    hyperlink: None,
                    extras: Some(ex),
                    underline_style: None,
                    underline_color: None,
                }));
            }
            (None, None) => {}
        }
    }

    /// Set this cell's underline style. Passing [`UnderlineStyle::Single`]
    /// clears the explicit style because single is the default.
    #[inline]
    pub fn set_underline_style(&mut self, style: UnderlineStyle) {
        let style = (style != UnderlineStyle::Single).then_some(style);
        match (&mut self.fat, style) {
            (Some(fat), style) => {
                fat.underline_style = style;
                if fat.is_empty() {
                    self.fat = None;
                }
            }
            (None, Some(style)) => {
                self.fat = Some(Box::new(FatAttributes {
                    hyperlink: None,
                    extras: None,
                    underline_style: Some(style),
                    underline_color: None,
                }));
            }
            (None, None) => {}
        }
    }

    /// Set this cell's explicit underline colour. Passing `None` clears it
    /// and falls back to the cell foreground.
    #[inline]
    pub fn set_underline_color(&mut self, color: Option<Color>) {
        match (&mut self.fat, color) {
            (Some(fat), color) => {
                fat.underline_color = color;
                if fat.is_empty() {
                    self.fat = None;
                }
            }
            (None, Some(color)) => {
                self.fat = Some(Box::new(FatAttributes {
                    hyperlink: None,
                    extras: None,
                    underline_style: None,
                    underline_color: Some(color),
                }));
            }
            (None, None) => {}
        }
    }

    /// Take the extras cluster out, leaving `None`. Collapses the
    /// box if nothing else remains.
    #[inline]
    pub fn take_extras(&mut self) -> Option<Box<str>> {
        let taken = self.fat.as_mut().and_then(|f| f.extras.take());
        if let Some(fat) = &self.fat {
            if fat.is_empty() {
                self.fat = None;
            }
        }
        taken
    }

    /// Internal: whether this cell has materialized its fat box.
    /// Used by tests asserting the no-alloc default path.
    #[doc(hidden)]
    #[inline]
    pub fn has_fat(&self) -> bool {
        self.fat.is_some()
    }
}

// Manual Serialize/Deserialize so on-disk format stays compatible with
// the pre-compact Cell layout (named fields: ch, fg, bg, flags,
// hyperlink, extras; newer underline fields are optional). External consumers and the existing
// serde_roundtrip test see no change.
mod cell_serde {
    use super::*;
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeStruct;
    use std::fmt;

    impl Serialize for Cell {
        fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
            let mut s = ser.serialize_struct("Cell", 8)?;
            s.serialize_field("ch", &self.ch)?;
            s.serialize_field("fg", &self.fg)?;
            s.serialize_field("bg", &self.bg)?;
            s.serialize_field("flags", &self.flags.bits())?;
            s.serialize_field("hyperlink", &self.hyperlink())?;
            s.serialize_field("extras", &self.extras())?;
            s.serialize_field(
                "underline_style",
                &self.fat.as_ref().and_then(|f| f.underline_style),
            )?;
            s.serialize_field("underline_color", &self.underline_color())?;
            s.end()
        }
    }

    impl<'de> Deserialize<'de> for Cell {
        fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            #[derive(Deserialize)]
            #[serde(field_identifier, rename_all = "lowercase")]
            enum Field {
                Ch,
                Fg,
                Bg,
                Flags,
                Hyperlink,
                Extras,
                #[serde(rename = "underline_style")]
                UnderlineStyle,
                #[serde(rename = "underline_color")]
                UnderlineColor,
            }

            struct CellVisitor;
            impl<'de> Visitor<'de> for CellVisitor {
                type Value = Cell;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("struct Cell")
                }
                fn visit_map<V: MapAccess<'de>>(self, mut map: V) -> Result<Cell, V::Error> {
                    let mut ch: Option<char> = None;
                    let mut fg: Option<Color> = None;
                    let mut bg: Option<Color> = None;
                    let mut flag_bits: Option<u16> = None;
                    let mut hyperlink: Option<Option<HyperlinkId>> = None;
                    let mut extras: Option<Option<String>> = None;
                    let mut underline_style: Option<Option<UnderlineStyle>> = None;
                    let mut underline_color: Option<Option<Color>> = None;
                    while let Some(k) = map.next_key()? {
                        match k {
                            Field::Ch => ch = Some(map.next_value()?),
                            Field::Fg => fg = Some(map.next_value()?),
                            Field::Bg => bg = Some(map.next_value()?),
                            Field::Flags => flag_bits = Some(map.next_value()?),
                            Field::Hyperlink => hyperlink = Some(map.next_value()?),
                            Field::Extras => extras = Some(map.next_value()?),
                            Field::UnderlineStyle => underline_style = Some(map.next_value()?),
                            Field::UnderlineColor => underline_color = Some(map.next_value()?),
                        }
                    }
                    let mut cell = Cell::plain(
                        ch.ok_or_else(|| de::Error::missing_field("ch"))?,
                        fg.unwrap_or_default(),
                        bg.unwrap_or_default(),
                        CellFlags::from_bits_truncate(flag_bits.unwrap_or(0)),
                    );
                    if let Some(Some(h)) = hyperlink {
                        cell.set_hyperlink(Some(h));
                    }
                    if let Some(Some(ex)) = extras {
                        cell.set_extras(Some(ex.into_boxed_str()));
                    }
                    if let Some(Some(style)) = underline_style {
                        cell.set_underline_style(style);
                    }
                    if let Some(Some(color)) = underline_color {
                        cell.set_underline_color(Some(color));
                    }
                    Ok(cell)
                }
            }
            de.deserialize_struct(
                "Cell",
                &[
                    "ch",
                    "fg",
                    "bg",
                    "flags",
                    "hyperlink",
                    "extras",
                    "underline_style",
                    "underline_color",
                ],
                CellVisitor,
            )
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            fat: None,
        }
    }
}
