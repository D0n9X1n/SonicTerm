//! OSC 21, the kitty color protocol: `key=?` queries, `key=spec` sets, and `key=` or a bare `key`
//! resets, for palette entries 0-255 and the named colors, answered in one `OSC 21` reply.
//!
//! A packet is read whole into a bounded item list before anything applies, so a packet past the
//! item budget changes nothing. Bytes outside printable ASCII are removed first because the reply
//! echoes keys, and an unknown key is answered by its base64 name, never verbatim. The selection
//! and cursor-text keys share their state with OSC 17 and 19. The visual-bell and
//! transparent-background keys are recognized but not held: a query answers empty and a set or
//! reset is ignored.

use super::*;
use base64::Engine;

/// The byte bound of one OSC 21 payload; a longer packet is refused whole. A counted field adds at
/// most 12 bytes plus 4/3 of its own bytes to the reply, so 526 fields within 16 KiB answer in
/// under 28.2 KB, one 32 KiB reply-spool submission.
pub(super) const MAX_KITTY_COLOR_BYTES: usize = 16 * 1024;

/// The most fields of one packet that may query, set, reset or name an unknown key, as Ghostty
/// bounds them; a packet with more is refused whole.
const MAX_KITTY_COLOR_ITEMS: usize = 526;

/// A color an OSC 21 key names.
#[derive(Clone, Copy)]
enum KittyColor {
    /// Palette entry 0-255.
    Palette(u8),
    /// The foreground, background, cursor, selection colors or cursor text.
    Dynamic(DynamicColor),
    /// A color kitty keeps and this terminal does not: answered empty, never stored.
    Unheld,
}

/// One counted field of a packet, borrowing its key from the normalized payload.
enum KittyItem<'payload> {
    /// `key=?`: answer the color under the key as sent.
    Query(&'payload [u8], KittyColor),
    /// `key=spec`, `key=` or a bare `key`: store the color, `None` resetting it to the theme.
    Store(KittyColor, Option<Rgb>),
    /// A key this terminal does not know, answered `unknown=` with its base64 name.
    Unknown(&'payload [u8]),
}

/// The counted items of normalized `payload` in request order; `None` when more than
/// [`MAX_KITTY_COLOR_ITEMS`] fields count, which refuses the whole packet.
fn kitty_color_items(payload: &[u8]) -> Option<Vec<KittyItem<'_>>> {
    let mut items = Vec::new();
    for field in payload.split(|byte| *byte == b';') {
        let mut parts = field.splitn(2, |byte| *byte == b'=');
        let key = parts.next().unwrap_or_default();
        if key.is_empty() {
            // When: the field or its `key` is empty (`;;`, `=?`), it is skipped and not counted.
            continue;
        }
        let Some(item) = kitty_color_item(key, parts.next()) else {
            // When: `kitty_color_item` finds an unreadable spec, the field is skipped, uncounted.
            continue;
        };
        if items.len() == MAX_KITTY_COLOR_ITEMS {
            // When: `items` is already full, this counted field refuses the whole packet.
            return None;
        }
        items.push(item);
    }
    Some(items)
}

/// The item a field makes from its `key` and the `value` after its first `=` (`None` for a bare
/// key); `None` when a set's spec names no color.
fn kitty_color_item<'payload>(
    key: &'payload [u8],
    value: Option<&[u8]>,
) -> Option<KittyItem<'payload>> {
    let Some(color) = kitty_color_key(key) else {
        // When: `kitty_color_key` knows no such key, every form answers `unknown=`.
        return Some(KittyItem::Unknown(key));
    };
    // The value is trimmed of spaces, as Ghostty reads it; the key is not.
    let value = value.map(<[u8]>::trim_ascii).unwrap_or_default();
    if value == b"?" {
        // When: `value` is `?`, the color is answered, not stored.
        return Some(KittyItem::Query(key, color));
    }
    if value.is_empty() || matches!(color, KittyColor::Unheld) {
        // When: `value` is empty it resets the color; an unheld color ignores any value, counted.
        return Some(KittyItem::Store(color, None));
    }
    // A set drops an `@opacity` suffix before the color is read.
    let spec = value.split(|byte| *byte == b'@').next().unwrap_or_default();
    vt_color_spec::parse_color_spec(spec).map(|rgb| KittyItem::Store(color, Some(rgb)))
}

/// The color OSC 21 `key` names: palette digits up to 255 (leading zeros allowed, as kitty reads
/// them), the named colors, and transparent background slots 1-8; `None` for any other key.
fn kitty_color_key(key: &[u8]) -> Option<KittyColor> {
    if let Some(index) = decimal(key) {
        // When: `key` is all digits, it names a palette entry only up to 255.
        return u8::try_from(index).ok().map(KittyColor::Palette);
    }
    match key {
        b"foreground" => Some(KittyColor::Dynamic(DynamicColor::Foreground)),
        b"background" => Some(KittyColor::Dynamic(DynamicColor::Background)),
        b"cursor" => Some(KittyColor::Dynamic(DynamicColor::Cursor)),
        b"selection_background" => Some(KittyColor::Dynamic(DynamicColor::SelectionBackground)),
        b"selection_foreground" => Some(KittyColor::Dynamic(DynamicColor::SelectionForeground)),
        b"cursor_text" => Some(KittyColor::Dynamic(DynamicColor::CursorText)),
        b"visual_bell" => Some(KittyColor::Unheld),
        _ => {
            // Any other key is known only as a transparent background slot 1-8.
            let slot = key.strip_prefix(b"transparent_background_color").and_then(decimal)?;
            (1..=8).contains(&slot).then_some(KittyColor::Unheld)
        }
    }
}

/// The value of `digits` when it is a nonempty run of ASCII digits, saturating rather than
/// overflowing on a long run.
fn decimal(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        // When: `digits` is empty or holds a non-digit, it is no number.
        return None;
    }
    Some(digits.iter().fold(0u32, |value, digit| {
        value.saturating_mul(10).saturating_add(u32::from(digit - b'0'))
    }))
}

impl Performer {
    /// Apply one whole OSC 21 `payload` in request order and answer its queries and unknown keys
    /// in one reply ending in `terminator`; a packet past the item budget changes and answers
    /// nothing, and a packet with nothing to answer sends no reply.
    pub(super) fn apply_kitty_color_osc(&mut self, payload: &[u8], terminator: &[u8]) {
        // Only printable ASCII is read, because the reply echoes keys back to the program.
        let printable: Vec<u8> =
            payload.iter().copied().filter(|byte| (0x20..=0x7e).contains(byte)).collect();
        let Some(items) = kitty_color_items(&printable) else {
            // When: `kitty_color_items` refuses the packet past its item budget, nothing applies.
            return;
        };
        let mut answers = Vec::new();
        for item in items {
            match item {
                KittyItem::Query(key, color) => {
                    answers.push(b';');
                    answers.extend_from_slice(key);
                    answers.push(b'=');
                    if let Some((red, green, blue)) = self.kitty_color_value(color) {
                        // An unknown color answers with an empty value.
                        let rgb = format!("rgb:{red:02x}/{green:02x}/{blue:02x}");
                        answers.extend_from_slice(rgb.as_bytes());
                    }
                }
                KittyItem::Store(color, rgb) => self.store_kitty_color(color, rgb),
                KittyItem::Unknown(key) => {
                    let name = base64::engine::general_purpose::STANDARD_NO_PAD.encode(key);
                    answers.extend_from_slice(b";unknown=");
                    answers.extend_from_slice(name.as_bytes());
                }
            }
        }
        if answers.is_empty() {
            // When: `answers` is empty, the packet only set or reset colors and gets no reply.
            return;
        }
        let mut reply = b"\x1b]21".to_vec();
        reply.extend_from_slice(&answers);
        reply.extend_from_slice(terminator);
        self.reply(&reply);
    }

    /// The color an OSC 21 query answers: the effective palette entry or dynamic color, and none
    /// for a color this terminal does not hold.
    fn kitty_color_value(&self, color: KittyColor) -> Option<Rgb> {
        match color {
            KittyColor::Palette(index) => self.effective_palette(index),
            KittyColor::Dynamic(target) => self.effective_dynamic(target),
            KittyColor::Unheld => None,
        }
    }

    /// Store `rgb` as the program's override of `color`, `None` resetting it to the theme; an
    /// unheld color ignores it.
    fn store_kitty_color(&mut self, color: KittyColor, rgb: Option<Rgb>) {
        match color {
            KittyColor::Palette(index) => self.color_overrides.set_palette(index, rgb),
            KittyColor::Dynamic(target) => self.color_overrides.set_dynamic(target, rgb),
            KittyColor::Unheld => {
                // When: `color` is Unheld, this terminal keeps no such color, so the set is dropped.
            }
        }
    }
}

#[cfg(test)]
#[path = "vt_kitty_color_tests.rs"]
mod vt_kitty_color_tests;
