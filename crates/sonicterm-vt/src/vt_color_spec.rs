//! Color specifications a program writes in OSC 4, 10, 11 and 12, read the way Ghostty reads
//! them: `rgb:` and `rgbi:`, `#` hex with 1 to 4 digits per channel, unprefixed 3 or 6 hex
//! digits, and X11 color names. A spec that matches no form is refused whole.

use super::vt_x11_colors::X11_COLORS;
use sonicterm_types::Rgb;

/// Fraction digits past this scale add nothing an 8-bit channel can show, so `rgbi:` stops
/// accumulating them there, as Ghostty does, and a long fraction cannot overflow.
const FRACTION_SCALE_LIMIT: u64 = 1_000_000_000_000_000_000;

/// The color `spec` names, ignoring blanks around it; `None` when it names no color.
pub(super) fn parse_color_spec(spec: &[u8]) -> Option<Rgb> {
    let input = trim_blanks(spec);
    if input.is_empty() {
        // When: the trimmed `input` is empty, there is no spec to read.
        return None;
    }
    if let Some(digits) = input.strip_prefix(b"#") {
        // When: `input` starts with `#`, only 3, 6, 9 or 12 hex `digits` name a color.
        return hex_triplet(digits);
    }
    if let Some(rgb) = x11_color(input) {
        // When: `x11_color` knows the name, it wins over the bare-hex reading of the same text.
        return Some(rgb);
    }
    if matches!(input.len(), 3 | 6) {
        // When: `input` length matches 3 or 6 and it is no name, it can only be unprefixed hex.
        return hex_triplet(input);
    }
    let (intensity, channels) = if let Some(rest) = input.strip_prefix(b"rgbi:") {
        (true, rest)
    } else {
        // When: `input` is not `rgbi:`, only the `rgb:` hex form remains.
        (false, input.strip_prefix(b"rgb:")?)
    };
    let mut parts = channels.splitn(3, |byte| *byte == b'/');
    let (red, green, blue) = (parts.next()?, parts.next()?, parts.next()?);
    let channel: fn(&[u8]) -> Option<u8> = if intensity { intensity_channel } else { hex_channel };
    Some((channel(red)?, channel(green)?, channel(blue)?))
}

/// `spec` without the spaces and tabs around it.
fn trim_blanks(spec: &[u8]) -> &[u8] {
    let is_blank = |byte: &u8| matches!(byte, b' ' | b'\t');
    let start = spec.iter().position(|byte| !is_blank(byte)).unwrap_or(spec.len());
    let end = spec.iter().rposition(|byte| !is_blank(byte)).map_or(start, |last| last + 1);
    &spec[start..end]
}

/// Three equal hex channels of 1 to 4 digits each, read from 3, 6, 9 or 12 `digits`.
fn hex_triplet(digits: &[u8]) -> Option<Rgb> {
    let width = match digits.len() {
        3 => 1,
        6 => 2,
        9 => 3,
        12 => 4,
        _ => {
            // When: `digits` has any other length, it names no color.
            return None;
        }
    };
    let (red, rest) = digits.split_at(width);
    let (green, blue) = rest.split_at(width);
    Some((hex_channel(red)?, hex_channel(green)?, hex_channel(blue)?))
}

/// One hex channel of 1 to 4 digits, scaled from its own width to 8 bits, truncating.
fn hex_channel(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 || !digits.iter().all(u8::is_ascii_hexdigit) {
        // When: `digits` is empty, longer than 4 or holds a non-hex byte, it is no channel.
        return None;
    }
    let value = digits.iter().fold(0u32, |value, digit| {
        value * 16 + char::from(*digit).to_digit(16).unwrap_or_default()
    });
    let max = (1u32 << (4 * digits.len())) - 1;
    u8::try_from(value * 255 / max).ok()
}

/// One `rgbi:` channel: a decimal intensity in 0..=1 with an optional sign, scaled to 8 bits,
/// truncating.
fn intensity_channel(text: &[u8]) -> Option<u8> {
    let (negative, number) = match text.split_first() {
        Some((b'+', rest)) => (false, rest),
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    let (whole, fraction) = match number.iter().position(|byte| *byte == b'.') {
        Some(dot) => (&number[..dot], &number[dot + 1..]),
        None => (number, &[][..]),
    };
    if whole.len() + fraction.len() == 0 || !whole.iter().chain(fraction).all(u8::is_ascii_digit) {
        // When: `whole` and `fraction` hold no digit, or a byte that is not a digit (a second
        // dot, an exponent), the text is no decimal intensity.
        return None;
    }
    let whole_value =
        whole.iter().fold(0f64, |value, digit| value * 10.0 + f64::from(digit - b'0'));
    let (mut numerator, mut scale) = (0u64, 1u64);
    for digit in fraction {
        if scale < FRACTION_SCALE_LIMIT {
            // Below the limit a digit still changes the 8-bit result.
            numerator = numerator * 10 + u64::from(digit - b'0');
            scale *= 10;
        }
    }
    let magnitude = whole_value + numerator as f64 / scale as f64;
    if negative && magnitude != 0.0 {
        // When: `negative` with a nonzero magnitude, the intensity is below 0 and names no color.
        return None;
    }
    (magnitude <= 1.0).then_some((magnitude * 255.0) as u8)
}

/// The X11 color `name` names, matched case-insensitively against the sorted table.
fn x11_color(name: &[u8]) -> Option<Rgb> {
    X11_COLORS
        .binary_search_by(|(entry, _)| entry.bytes().cmp(name.iter().map(u8::to_ascii_lowercase)))
        .ok()
        .map(|index| X11_COLORS[index].1)
}

#[cfg(test)]
#[path = "vt_color_spec_tests.rs"]
mod vt_color_spec_tests;
