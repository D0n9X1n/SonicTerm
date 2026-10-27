//! Shared status-text sanitization and localized numeric, byte, and duration formatting.

use super::*;

/// Which characters the sanitizer replaces besides the removed controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizePolicy {
    /// Paths, titles, process names and preedit text.
    Ordinary,
    /// Program-reported text: also replaces separator-like and origin-marker characters.
    Program,
}

/// Caps applied after character filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizeLimits {
    /// Byte cap, applied on a character boundary.
    pub max_bytes: usize,
    /// Display-column cap, applied by middle ellipsis on grapheme boundaries.
    pub max_columns: usize,
    /// Collapse whitespace runs to one space and trim the ends.
    pub collapse_whitespace: bool,
}

/// Whether `character` is a general-category `Cf` format character.
fn is_format_character(character: char) -> bool {
    matches!(
        u32::from(character),
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x890..=0x891
            | 0x8E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Whether the sanitizer removes `character` under every policy: C0 and C1
/// controls, line and paragraph separators, and every `Cf` except ZWJ, which
/// emoji sequences need.
fn is_removed(character: char) -> bool {
    matches!(u32::from(character), 0x00..=0x1F | 0x7F..=0x9F | 0x2028 | 0x2029)
        || (is_format_character(character) && character != '\u{200D}')
}

/// The one shared sanitizer for text shown on the strip.
pub fn sanitize_display_text(text: &str, policy: SanitizePolicy, limits: SanitizeLimits) -> String {
    let mut out = String::with_capacity(text.len().min(limits.max_bytes));
    let mut after_space = true;
    for character in text.chars() {
        if is_removed(character) {
            // When: `is_removed(character)`, it is a control or format character and is dropped.
            continue;
        }
        let character = match character {
            '│' | '|' | '⟨' | '⟩' | '<' | '>' if policy == SanitizePolicy::Program => ' ',
            other => other,
        };
        if limits.collapse_whitespace && character.is_whitespace() {
            // When: `collapse_whitespace` and `character` is whitespace, a run collapses to one space.
            if after_space {
                // When: `after_space`, a whitespace run is already open, so collapse into it.
                continue;
            }
            after_space = true;
        } else {
            // When: `character` is not collapsing whitespace, any open run ends.
            after_space = false;
        }
        let character =
            if limits.collapse_whitespace && character.is_whitespace() { ' ' } else { character };
        if out.len() + character.len_utf8() > limits.max_bytes {
            // When: the next character would pass `max_bytes`, stop on this boundary.
            break;
        }
        out.push(character);
    }
    if limits.collapse_whitespace {
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
    }
    ellipsize_middle_columns(&out, limits.max_columns)
}

/// `text` middle-ellipsized to at most `max_bytes` UTF-8 bytes, ellipsis included, cut on
/// grapheme boundaries; the head keeps the extra grapheme when the halves cannot match.
pub fn ellipsize_middle_bytes(text: &str, max_bytes: usize) -> String {
    const ELLIPSIS: &str = "…";
    if text.len() <= max_bytes {
        // When: text already fits max_bytes, keep it whole.
        return text.to_string();
    }
    let Some(budget) = max_bytes.checked_sub(ELLIPSIS.len()) else {
        // When: max_bytes cannot hold even the ellipsis, nothing is shown.
        return String::new();
    };
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let (mut head, mut tail) = (0, graphemes.len());
    let (mut head_bytes, mut tail_bytes) = (0, 0);
    // Take graphemes from the front and the back alternately while they fit.
    loop {
        let take_head = head_bytes <= tail_bytes;
        let next = if take_head {
            graphemes.get(head)
        } else {
            // When: take_head is false, the back end is shorter, so it grows next.
            graphemes.get(tail - 1)
        };
        let Some(next) = next.filter(|_| head < tail) else {
            // When: next is None or head met tail; the length check above makes that unreachable.
            break;
        };
        if head_bytes + tail_bytes + next.len() > budget {
            // When: the next grapheme would pass the budget, the cut stops here.
            break;
        }
        if take_head {
            head_bytes += next.len();
            head += 1;
        } else {
            // When: take_head is false, the grapheme came from the back.
            tail_bytes += next.len();
            tail -= 1;
        }
    }
    format!("{}{ELLIPSIS}{}", graphemes[..head].concat(), graphemes[tail..].concat())
}

/// `text` middle-ellipsized to at most `max_columns` display columns, cut on
/// grapheme boundaries.
pub fn ellipsize_middle_columns(text: &str, max_columns: usize) -> String {
    if text.width() <= max_columns {
        // When: the `width` is within `max_columns`, keep it whole.
        return text.to_string();
    }
    if max_columns == 0 {
        // When: `max_columns` is 0, nothing can be shown.
        return String::new();
    }
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let budget = max_columns - 1;
    let head_budget = budget.div_ceil(2);
    let mut head = String::new();
    let mut used = 0;
    let mut head_end = 0;
    for grapheme in &graphemes {
        if used + grapheme.width() > head_budget {
            // When: the next `grapheme` would pass `head_budget`, the head ends.
            break;
        }
        used += grapheme.width();
        head.push_str(grapheme);
        head_end += 1;
    }
    let tail_budget = budget - used;
    let mut tail_start = graphemes.len();
    let mut tail_used = 0;
    while tail_start > head_end && tail_used + graphemes[tail_start - 1].width() <= tail_budget {
        tail_start -= 1;
        tail_used += graphemes[tail_start].width();
    }
    format!("{head}{ELLIPSIS}{}", graphemes[tail_start..].concat())
}

/// Decimal separator and digit grouping of the UI locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumberStyle {
    /// Thousands separator.
    pub group: char,
    /// Decimal separator.
    pub decimal: char,
}

impl NumberStyle {
    /// `12,345.6`: English, Chinese and Japanese.
    pub const POINT: Self = Self { group: ',', decimal: '.' };

    /// The style of a locale tag such as `de-DE` or `zh-CN`.
    pub fn for_locale(tag: &str) -> Self {
        let language = tag.split(['-', '_']).next().unwrap_or_default();
        match language {
            "de" | "es" | "it" | "nl" | "pt" | "tr" | "id" | "da" => {
                Self { group: '.', decimal: ',' }
            }
            "fr" | "ru" | "pl" | "cs" | "sv" | "fi" | "nb" | "uk" => {
                Self { group: '\u{202F}', decimal: ',' }
            }
            _ => Self::POINT,
        }
    }
}

/// `value` with the locale's digit grouping (`12,345`).
pub fn format_count(value: u64, style: NumberStyle) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 * 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(style.group);
        }
        out.push(digit);
    }
    out
}

/// Byte size in binary units with three significant figures and fixed unit
/// symbols (`412 MB`, `1.02 GB`); the unit rolls over at 1024.
pub fn format_bytes(bytes: u64, style: NumberStyle) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        // When: under 1 KB, whole bytes are exact.
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    // Three significant figures: 0, 1 or 2 decimals by magnitude, from these lower bounds.
    let decimals_for = |value: f64| {
        [(99.95, 0), (9.995, 1)]
            .into_iter()
            .find(|(lower, _)| value >= *lower)
            .map_or(2, |(_, decimals)| decimals)
    };
    let round_to = |value: f64, decimals: i32| {
        let scale = 10f64.powi(decimals);
        (value * scale).round() / scale
    };
    let mut decimals = decimals_for(value);
    let mut rounded = round_to(value, decimals);
    if rounded >= 1024.0 && unit < UNITS.len() - 1 {
        // Rounding reached 1024, roll over to the next unit.
        unit += 1;
        decimals = 2;
        rounded = round_to(rounded / 1024.0, decimals);
    }
    let number =
        format!("{rounded:.*}", decimals as usize).replace('.', &style.decimal.to_string());
    format!("{number} {}", UNITS[unit])
}

/// Duration with fixed unit suffixes, never localized: `42s`, `2m14s`, `1h05m`.
pub fn format_duration(total_secs: u64) -> String {
    let (hours, minutes, seconds) = (total_secs / 3600, total_secs / 60 % 60, total_secs % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m{seconds:02}s"),
        _ => format!("{hours}h{minutes:02}m"),
    }
}

/// Direction of a memory change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    /// Grew by at least 1%.
    Up,
    /// Shrank by at least 1%.
    Down,
}

/// The trend from `previous` to `current`, or `None` inside the 1% deadband.
pub fn mem_trend(previous: Option<u64>, current: u64) -> Option<Trend> {
    let previous = previous.filter(|previous| *previous > 0)?;
    let delta = current as f64 - previous as f64;
    if delta.abs() < previous as f64 * 0.01 {
        // When: `delta` is under 1% of `previous`, allocation noise shows no arrow.
        return None;
    }
    Some(if delta > 0.0 { Trend::Up } else { Trend::Down })
}

#[cfg(test)]
#[path = "status_line_text_tests.rs"]
mod status_line_text_tests;
