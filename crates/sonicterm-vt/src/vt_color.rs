//! Program color requests: OSC 4 and 10-19 sets and queries, the 104 and 110-119 resets, and
//! OSC 21, the kitty color protocol, which its own module reads.
//!
//! The raw OSC collector hands a whole message here only after its ST or BEL arrived, so a cut
//! or aborted message never applies a part. Requests apply in order and take the most accepting
//! reading among Ghostty, WezTerm and kitty: an invalid OSC 4 pair, OSC 104 index or dynamic
//! value is skipped and the rest still apply, an empty dynamic value advances to the next color,
//! and OSC 110-119 reset their color unless the first argument is `?`, which asks for it instead. The
//! foreground, background, cursor and the selection background (17) and foreground (19) are kept;
//! the other dynamic colors (13-16, 18 and their resets) are consumed silently. A change only
//! updates the pane's [`ColorOverrides`]; it raises no event and sends no mode 2031 report, and
//! the pane's next batch publication carries it to the renderer. A request moves the store's
//! generation once, and only when it changed a color the renderer draws, so a set equal to the
//! drawn color records presence without a repaint.

use super::*;
use sonicterm_types::{ColorOverrides, DynamicColor, Rgb};

/// The byte bound of one color request's payload other than OSC 21; a longer message is refused
/// whole.
const MAX_COLOR_OSC_BYTES: usize = 4096;

/// The OSC number of the last dynamic color; a chained request stops after it.
const LAST_DYNAMIC_OSC: u16 = 19;

/// Whether `code` is a color request this module consumes.
pub(super) fn is_color_osc(code: u16) -> bool {
    matches!(code, 4 | 10..=19 | 21 | 104 | 110..=119)
}

/// The whole-payload byte bound of color request `code`; a longer message is refused whole.
pub(super) fn color_osc_cap(code: u16) -> usize {
    if code == 21 {
        vt_kitty_color::MAX_KITTY_COLOR_BYTES
    } else {
        // When: `code` is any color request but OSC 21, the shared bound applies.
        MAX_COLOR_OSC_BYTES
    }
}

/// The color request `spelling` names: an optional `+` then digits, leading zeros allowed, as the
/// earlier query handler read OSC numbers; `None` for any other text or number.
pub(super) fn color_osc_number(spelling: &[u8]) -> Option<u16> {
    let digits = spelling.strip_prefix(b"+").unwrap_or(spelling);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        // When: `digits` is empty or holds a non-digit, it is no spelling of a color request.
        return None;
    }
    let code = digits
        .iter()
        .fold(0u32, |code, digit| code.saturating_mul(10).saturating_add(u32::from(digit - b'0')));
    u16::try_from(code).ok().filter(|code| is_color_osc(*code))
}

/// Where an OSC 4 or 104 index points: a palette entry, or one of the five special colors
/// (256-260) Ghostty parses and then ignores.
enum PaletteTarget {
    Entry(u8),
    Special,
}

/// The dynamic color OSC `code` (10-19) names, for those this terminal keeps.
fn dynamic_target(code: u16) -> Option<DynamicColor> {
    match code {
        10 => Some(DynamicColor::Foreground),
        11 => Some(DynamicColor::Background),
        12 => Some(DynamicColor::Cursor),
        17 => Some(DynamicColor::SelectionBackground),
        19 => Some(DynamicColor::SelectionForeground),
        _ => None,
    }
}

/// The palette target of an OSC 4 or 104 index, read by [`kitty_integer`]: 0-255 for an entry
/// and 256-260 for a special color.
fn palette_target(index: &[u8]) -> Option<PaletteTarget> {
    let value = kitty_integer(index)?;
    match value {
        0..=255 => u8::try_from(value).ok().map(PaletteTarget::Entry),
        256..=260 => Some(PaletteTarget::Special),
        _ => None,
    }
}

/// The value of `field` as kitty's integer parser reads an index: ASCII blanks around it, an
/// optional `+` or `-`, then digits that single underscores may separate; `None` for any other
/// text or a negative value. A long digit run saturates rather than overflowing.
fn kitty_integer(field: &[u8]) -> Option<u32> {
    let trimmed = field.trim_ascii();
    let (negative, digits) = match trimmed.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, trimmed),
    };
    let edge_digit = |byte: Option<&u8>| byte.is_some_and(u8::is_ascii_digit);
    if !edge_digit(digits.first())
        || !edge_digit(digits.last())
        || digits.windows(2).any(|pair| pair == b"__")
    {
        // When: `digits` is empty, starts or ends without a digit, or doubles an underscore, it
        // is no integer.
        return None;
    }
    let mut value = 0u32;
    for digit in digits {
        match digit {
            b'0'..=b'9' => {
                value = value.saturating_mul(10).saturating_add(u32::from(digit - b'0'));
            }
            b'_' => {
                // When: `digit` is an underscore between digits, it only groups them.
            }
            _ => {
                // When: `digit` is any other byte, the field is no integer.
                return None;
            }
        }
    }
    // Only zero may carry a minus sign; every other negative value names no index.
    (!negative || value == 0).then_some(value)
}

impl Parser {
    /// The colors the program set for this pane with OSC 4, 10-12, 17, 19 and 21; `None` while it
    /// has set none, so a pane draws with its theme alone.
    #[must_use]
    pub fn color_overrides(&self) -> Option<&ColorOverrides> {
        Some(&self.performer.color_overrides).filter(|overrides| !overrides.is_empty())
    }

    /// Tell the parser the theme selection background, answering `OSC 17 ; ?` while the program
    /// set none.
    pub fn set_theme_selection_bg(&mut self, red: u8, green: u8, blue: u8) {
        self.performer.theme_selection_bg = Some((red, green, blue));
    }

    /// Tell the parser the theme selection foreground, answering `OSC 19 ; ?` while the program
    /// set none.
    pub fn set_theme_selection_fg(&mut self, red: u8, green: u8, blue: u8) {
        self.performer.theme_selection_fg = Some((red, green, blue));
    }

    /// Tell the parser the theme color of the glyph under a block cursor, answering the OSC 21
    /// `cursor_text` key while the program set none.
    pub fn set_theme_cursor_text(&mut self, red: u8, green: u8, blue: u8) {
        self.performer.theme_cursor_text = Some((red, green, blue));
    }

    /// Lend the grid for a frame together with the pane's overrides, which the renderer reads
    /// while it holds the grid: lent while any is held, and after a drawn change until a fresh
    /// parser, so a store emptied by a reset still repaints the pane to the theme.
    pub fn grid_and_color_overrides(&mut self) -> (&mut Grid, Option<&ColorOverrides>) {
        let performer = &mut self.performer;
        let overrides = Some(&performer.color_overrides)
            .filter(|overrides| !overrides.is_empty() || overrides.generation() != 0);
        (&mut performer.grid, overrides)
    }
}

impl Performer {
    /// Apply one whole color request `code` with its `payload` (the bytes after `code;`), and
    /// answer its queries with the request's terminator, BEL when `bell_terminated`.
    pub(super) fn apply_color_osc(&mut self, code: u16, payload: &[u8], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        // Every `;`-separated field, empty ones included: an empty dynamic value still advances
        // and an empty OSC 4 field still holds its pair position.
        let mut fields = payload.split(|byte| *byte == b';');
        // The code selects the palette, the kitty protocol, a palette reset, a dynamic reset or a
        // dynamic request; each reports whether a drawn color changed.
        let drawn_changed = match code {
            21 => self.apply_kitty_color_osc(payload, terminator),
            4 => self.apply_palette_requests(&mut fields, terminator),
            // OSC 104 lists nothing in a blank field (empty or spaces only), as kitty reads it.
            104 => self.reset_palette_entries(
                fields.filter(|field| !field.iter().all(u8::is_ascii_whitespace)),
            ),
            110..=119 => self.reset_or_query_dynamic(code, fields.next(), terminator),
            _ => self.apply_dynamic_requests(code, fields, terminator),
        };
        if drawn_changed {
            // The request changed a drawn color, so the generation moves once for all of it.
            self.color_overrides.advance_generation();
        }
    }

    /// Store `rgb` as the override of palette entry `index`, `None` removing it; whether the color
    /// the entry draws changed.
    pub(super) fn store_palette(&mut self, index: u8, rgb: Option<Rgb>) -> bool {
        let before = self.effective_palette(index);
        self.color_overrides.set_palette(index, rgb) && self.effective_palette(index) != before
    }

    /// Store `rgb` as the override of dynamic `target`, `None` removing it; whether what the
    /// target draws changed.
    pub(super) fn store_dynamic(&mut self, target: DynamicColor, rgb: Option<Rgb>) -> bool {
        let before = self.drawn_dynamic(target);
        self.color_overrides.set_dynamic(target, rgb) && self.drawn_dynamic(target) != before
    }

    /// What decides the drawing of dynamic `target`: its effective color, except the selection
    /// foreground, which recolors selected glyphs only while a program override is present.
    fn drawn_dynamic(&self, target: DynamicColor) -> Option<Rgb> {
        if target == DynamicColor::SelectionForeground {
            // When: `target` is the selection foreground, presence alone changes the drawing.
            return self.color_overrides.dynamic(target);
        }
        self.effective_dynamic(target)
    }

    /// Remove every palette override (a bare OSC 104, RIS); whether a removed entry drew a color
    /// other than its default.
    pub(super) fn clear_palette_overrides(&mut self) -> bool {
        let drawn_change = (0..=255u8).any(|index| {
            self.color_overrides
                .palette(index)
                .is_some_and(|rgb| Some(rgb) != self.default_palette(index))
        });
        self.color_overrides.clear_palette();
        drawn_change
    }

    /// OSC 4 `index;spec` pairs in order; a pair with an invalid or empty index or spec is skipped
    /// and the pairs after it still apply, and an odd trailing index is ignored. Whether a drawn
    /// color changed.
    fn apply_palette_requests<'payload>(
        &mut self,
        params: &mut impl Iterator<Item = &'payload [u8]>,
        terminator: &[u8],
    ) -> bool {
        let mut drawn_changed = false;
        while let (Some(index), Some(spec)) = (params.next(), params.next()) {
            let Some(PaletteTarget::Entry(entry)) = palette_target(index) else {
                // When: `palette_target` refuses the index or names a special color (256-260),
                // which is not kept, this pair is skipped.
                continue;
            };
            if spec == b"?" {
                self.reply_palette(entry, terminator);
            } else if let Some(rgb) = vt_color_spec::parse_color_spec(spec) {
                // When: `parse_color_spec` reads the spec, `entry` takes the color; an
                // unreadable spec skips only this pair.
                drawn_changed |= self.store_palette(entry, Some(rgb));
            }
        }
        drawn_changed
    }

    /// OSC 104: reset each listed entry, skipping an invalid index; a bare request or one that
    /// lists only blanks resets every overridden entry, and a list of only invalid indices
    /// changes nothing. Whether a drawn color changed.
    fn reset_palette_entries<'payload>(
        &mut self,
        listed: impl Iterator<Item = &'payload [u8]>,
    ) -> bool {
        let mut listed_any = false;
        let mut drawn_changed = false;
        for field in listed {
            listed_any = true;
            if let Some(PaletteTarget::Entry(entry)) = palette_target(field) {
                drawn_changed |= self.store_palette(entry, None);
            }
        }
        if !listed_any {
            // A request that listed nothing names the whole palette.
            drawn_changed = self.clear_palette_overrides();
        }
        drawn_changed
    }

    /// OSC 110-119: a `?` `first` argument asks for the named color, reported under the
    /// request's own number as kitty does; any other first argument, or none, resets it, and later
    /// arguments are ignored (kitty's chaining is not copied). Only the named color is touched,
    /// and 113-116 and 118 name colors this terminal does not keep. Whether a drawn color changed.
    fn reset_or_query_dynamic(
        &mut self,
        code: u16,
        first: Option<&[u8]>,
        terminator: &[u8],
    ) -> bool {
        let Some(target) = dynamic_target(code - 100) else {
            // When: `dynamic_target` keeps no color for `code` (113-116, 118), it is consumed silently.
            return false;
        };
        if first == Some(b"?".as_slice()) {
            // A `?` first argument asks for the named color instead of resetting it.
            self.reply_dynamic(code, target, terminator);
            false
        } else {
            // When: the `first` argument is not `?`, the named color resets whatever follows.
            self.store_dynamic(target, None)
        }
    }

    /// OSC 10-19: each value targets the next dynamic color from `code` on, up to OSC 19; `?`
    /// answers it and a spec sets it. An empty value, a value naming a color this terminal does
    /// not keep (13-16, 18), or an unreadable spec does nothing and the walk goes on. Whether a
    /// drawn color changed.
    fn apply_dynamic_requests<'payload>(
        &mut self,
        code: u16,
        fields: impl Iterator<Item = &'payload [u8]>,
        terminator: &[u8],
    ) -> bool {
        let mut drawn_changed = false;
        for (osc, value) in (code..=LAST_DYNAMIC_OSC).zip(fields) {
            let Some(target) = dynamic_target(osc) else {
                // When: `dynamic_target` keeps no color for `osc` (13-16, 18), its value is consumed
                // silently: no reply, no change.
                continue;
            };
            if value.is_empty() {
                // When: `value` is an empty field, it advances to the next color and does nothing.
                continue;
            }
            if value == b"?" {
                // When: `value` is `?`, the color is answered, not set.
                self.reply_dynamic(osc, target, terminator);
                continue;
            }
            if let Some(rgb) = vt_color_spec::parse_color_spec(value) {
                // An unreadable value skips only this color.
                drawn_changed |= self.store_dynamic(target, Some(rgb));
            }
        }
        drawn_changed
    }

    /// The color palette entry `index` draws with: the program's override, else its default.
    fn effective_palette(&self, index: u8) -> Option<Rgb> {
        self.color_overrides.palette(index).or_else(|| self.default_palette(index))
    }

    /// The default of palette entry `index`: the theme seed for 0-15, else the shared 256-color
    /// table; `None` for an unseeded theme slot.
    fn default_palette(&self, index: u8) -> Option<Rgb> {
        match self.theme_palette.get(usize::from(index)) {
            Some(seeded) => *seeded,
            None => sonicterm_types::indexed_rgb(index),
        }
    }

    /// The color dynamic `target` draws with: the program's override, else the theme default;
    /// a cursor with neither falls back to the effective foreground.
    fn effective_dynamic(&self, target: DynamicColor) -> Option<Rgb> {
        let theme_default = match target {
            DynamicColor::Foreground => self.theme_fg,
            DynamicColor::Background => self.theme_bg,
            DynamicColor::Cursor => self.theme_cursor,
            DynamicColor::SelectionBackground => self.theme_selection_bg,
            DynamicColor::SelectionForeground => self.theme_selection_fg,
            DynamicColor::CursorText => self.theme_cursor_text,
        };
        let color = self.color_overrides.dynamic(target).or(theme_default);
        if target == DynamicColor::Cursor {
            // When: `target` is the cursor, an unset cursor reads as the effective foreground.
            return color.or_else(|| self.effective_dynamic(DynamicColor::Foreground));
        }
        color
    }

    /// Answer an OSC 4 query for palette entry `index`; silent for an unseeded slot.
    pub(super) fn reply_palette(&self, index: u8, terminator: &[u8]) {
        if let Some(rgb) = self.effective_palette(index) {
            // An unknown color is not reported.
            self.reply_color(&format!("4;{index}"), rgb, terminator);
        }
    }

    /// Answer an OSC 10-19 or 110-119 query for `target`, reported under OSC number `osc`.
    fn reply_dynamic(&self, osc: u16, target: DynamicColor, terminator: &[u8]) {
        if let Some(rgb) = self.effective_dynamic(target) {
            // An unknown color is not reported.
            self.reply_color(&osc.to_string(), rgb, terminator);
        }
    }

    /// Send `ESC ] selector ; rgb:rrrr/gggg/bbbb` and `terminator`, each channel widened to 16
    /// bits as xterm and Ghostty report it.
    fn reply_color(&self, selector: &str, (red, green, blue): Rgb, terminator: &[u8]) {
        let mut reply = format!(
            "\x1b]{selector};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}"
        )
        .into_bytes();
        reply.extend_from_slice(terminator);
        self.reply(&reply);
    }
}

#[path = "vt_kitty_color.rs"]
mod vt_kitty_color;

#[cfg(test)]
#[path = "vt_color_tests.rs"]
mod vt_color_tests;

#[cfg(test)]
#[path = "vt_color_theme_tests.rs"]
mod vt_color_theme_tests;

#[cfg(test)]
#[path = "vt_color_selection_tests.rs"]
mod vt_color_selection_tests;
