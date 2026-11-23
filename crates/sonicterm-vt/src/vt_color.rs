//! Program color requests: OSC 4 and 10-19 sets and queries, the 104 and 110-119 resets, and
//! OSC 21, the kitty color protocol, which its own module reads.
//!
//! The raw OSC collector hands a whole message here only after its ST or BEL arrived, so a cut
//! or aborted message never applies a part. Requests apply in order and take the most accepting
//! reading among Ghostty, WezTerm and kitty: an invalid OSC 4 pair, OSC 104 index or dynamic
//! value is skipped and the rest still apply, an empty dynamic value advances to the next color,
//! and OSC 110-112 reset their color unless an argument is `?`, which asks for it instead. Only
//! the foreground, background and cursor are kept; the other dynamic colors (13-19, 113-119) are
//! consumed silently. A change only updates the pane's
//! [`ColorOverrides`]; it raises no event and sends no mode 2031 report, and the pane's next
//! batch publication carries it to the renderer.

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

/// The color request `digits` names, spelled exactly as Ghostty reads it (digits only, no
/// leading zero); `None` for every other number.
pub(super) fn color_osc_number(digits: &[u8]) -> Option<u16> {
    if digits.is_empty()
        || digits.len() > 3
        || digits[0] == b'0'
        || !digits.iter().all(u8::is_ascii_digit)
    {
        // When: `digits` is empty, too long, zero-padded or not all digits, it is no exact
        // spelling of a color request.
        return None;
    }
    let code = digits.iter().fold(0u16, |code, digit| code * 10 + u16::from(digit - b'0'));
    is_color_osc(code).then_some(code)
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
        _ => None,
    }
}

/// The palette target of an OSC 4 or 104 index: digits only, as Ghostty's protocol integer
/// parser reads them, 0-255 for an entry and 256-260 for a special color.
fn palette_target(index: &[u8]) -> Option<PaletteTarget> {
    if index.is_empty() || index.len() > 3 || !index.iter().all(u8::is_ascii_digit) {
        // When: `index` is empty, has no room for a value up to 260, or holds a non-digit
        // (a sign, a separator, a blank), it names no target.
        return None;
    }
    let value = index.iter().fold(0u16, |value, digit| value * 10 + u16::from(digit - b'0'));
    match value {
        0..=255 => u8::try_from(value).ok().map(PaletteTarget::Entry),
        256..=260 => Some(PaletteTarget::Special),
        _ => None,
    }
}

impl Parser {
    /// The colors the program set for this pane with OSC 4, 10, 11, 12 and 21; `None` while it has
    /// set none, so a pane draws with its theme alone.
    #[must_use]
    pub fn color_overrides(&self) -> Option<&ColorOverrides> {
        Some(&self.performer.color_overrides).filter(|overrides| !overrides.is_empty())
    }

    /// Lend the grid for a frame together with [`Parser::color_overrides`], which the renderer
    /// reads while it holds the grid.
    pub fn grid_and_color_overrides(&mut self) -> (&mut Grid, Option<&ColorOverrides>) {
        let performer = &mut self.performer;
        let overrides = Some(&performer.color_overrides).filter(|overrides| !overrides.is_empty());
        (&mut performer.grid, overrides)
    }
}

impl Performer {
    /// Apply one whole color request `code` with its `payload` (the bytes after `code;`), and
    /// answer its queries with the request's terminator, BEL when `bell_terminated`.
    pub(super) fn apply_color_osc(&mut self, code: u16, payload: &[u8], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        // Every `;`-separated field, empty ones included: an empty dynamic value still advances.
        let fields = payload.split(|byte| *byte == b';');
        // The code selects the palette, the kitty protocol, a palette reset, a dynamic reset or a
        // dynamic request.
        match code {
            21 => self.apply_kitty_color_osc(payload, terminator),
            // OSC 4 skips empty fields, as Ghostty's tokenizer does, before pairing them.
            4 => self
                .apply_palette_requests(&mut fields.filter(|field| !field.is_empty()), terminator),
            // OSC 104 lists nothing in a blank field (empty or spaces only), as kitty reads it.
            104 => self.reset_palette_entries(
                fields.filter(|field| !field.iter().all(u8::is_ascii_whitespace)),
            ),
            110..=119 => self.reset_or_query_dynamic(code, fields, terminator),
            _ => self.apply_dynamic_requests(code, fields, terminator),
        }
    }

    /// OSC 4 `index;spec` pairs in order; a pair with an invalid index or spec is skipped and
    /// the pairs after it still apply, and an odd trailing index is ignored.
    fn apply_palette_requests<'payload>(
        &mut self,
        params: &mut impl Iterator<Item = &'payload [u8]>,
        terminator: &[u8],
    ) {
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
                self.color_overrides.set_palette(entry, Some(rgb));
            }
        }
    }

    /// OSC 104: reset each listed entry, skipping an invalid index; a bare request or one that
    /// lists only blanks resets every overridden entry, and a list of only invalid indices
    /// changes nothing.
    fn reset_palette_entries<'payload>(&mut self, listed: impl Iterator<Item = &'payload [u8]>) {
        let mut listed_any = false;
        for field in listed {
            listed_any = true;
            if let Some(PaletteTarget::Entry(entry)) = palette_target(field) {
                self.color_overrides.set_palette(entry, None);
            }
        }
        if !listed_any {
            // A request that listed nothing names the whole palette.
            self.color_overrides.clear_palette();
        }
    }

    /// OSC 110-119: a `?` argument asks for the named color, reported under the request's own
    /// number as kitty does; any other argument, or none, resets it. Only the named color is
    /// touched, and 113-119 name colors this terminal does not keep.
    fn reset_or_query_dynamic<'payload>(
        &mut self,
        code: u16,
        mut fields: impl Iterator<Item = &'payload [u8]>,
        terminator: &[u8],
    ) {
        let Some(target) = dynamic_target(code - 100) else {
            // When: `dynamic_target` keeps no color for `code` (113-119), it is consumed silently.
            return;
        };
        if fields.any(|field| field == b"?") {
            // A `?` argument asks for the named color instead of resetting it.
            self.reply_dynamic(code, target, terminator);
        } else {
            // When: none of the `fields` is `?`, the named color resets whatever else follows.
            self.color_overrides.set_dynamic(target, None);
        }
    }

    /// OSC 10-19: each value targets the next dynamic color from `code` on, up to OSC 19; `?`
    /// answers it and a spec sets it. An empty value, a value naming a color this terminal does
    /// not keep (13-19), or an unreadable spec does nothing and the walk goes on.
    fn apply_dynamic_requests<'payload>(
        &mut self,
        code: u16,
        fields: impl Iterator<Item = &'payload [u8]>,
        terminator: &[u8],
    ) {
        for (osc, value) in (code..=LAST_DYNAMIC_OSC).zip(fields) {
            let Some(target) = dynamic_target(osc) else {
                // When: `dynamic_target` keeps no color for `osc` (13-19), its value is consumed
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
                self.color_overrides.set_dynamic(target, Some(rgb));
            }
        }
    }

    /// The color palette entry `index` draws with: the program's override, else the theme seed
    /// for 0-15 or the shared 256-color table; `None` for an unseeded theme slot.
    fn effective_palette(&self, index: u8) -> Option<Rgb> {
        self.color_overrides.palette(index).or_else(|| {
            match self.theme_palette.get(usize::from(index)) {
                Some(seeded) => *seeded,
                None => sonicterm_types::indexed_rgb(index),
            }
        })
    }

    /// The color dynamic `target` draws with: the program's override, else the theme default;
    /// a cursor with neither falls back to the effective foreground.
    fn effective_dynamic(&self, target: DynamicColor) -> Option<Rgb> {
        let theme_default = match target {
            DynamicColor::Foreground => self.theme_fg,
            DynamicColor::Background => self.theme_bg,
            DynamicColor::Cursor => self.theme_cursor,
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

    /// Answer an OSC 10-12 query for `target`, reported under OSC number `osc`.
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
