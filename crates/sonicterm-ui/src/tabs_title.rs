//! Tab-title text helpers: the `#N` index prefix, the title body, and fitting a title into a
//! measured width with an ellipsis.

use unicode_segmentation::UnicodeSegmentation;

/// Rebuild a tab title by keeping `template`'s `#N` index and any short
/// symbolic icon token that follows it, while `body` replaces the rest. When
/// `template` carries no such index prefix, `body` becomes the whole title.
pub fn title_with_replaced_body(template: &str, body: &str) -> String {
    let trimmed = template.trim();
    let Some(rest) = trimmed.strip_prefix('#') else {
        // When: trimmed opens with no '#', so there is no index to preserve and
        // body stands alone as the title.
        return body.to_string();
    };
    let Some(space) = rest.find(' ') else {
        // When: rest holds no separator, so the template is a bare index with
        // no body to keep and body replaces the whole title.
        return body.to_string();
    };
    let index = &trimmed[..space + 1];
    let after_index = trimmed[space + 1..].trim_start();
    let mut parts = after_index.splitn(2, ' ');
    let first = parts.next().unwrap_or_default();
    let rest = parts.next();
    let keep_icon = rest.is_some()
        && first.chars().count() <= 2
        && !first.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '/' || character == '~'
        });
    if keep_icon {
        format!("{index} {first} {body}")
    } else {
        // When: keep_icon is false, so first was ordinary title text rather
        // than an icon and only the index survives alongside body.
        format!("{index} {body}")
    }
}

/// Pixel slack allowed when a measured title is compared with its rect, so
/// sub-pixel layout rounding never cuts a title that fits.
pub const TITLE_FIT_TOLERANCE_PX: f32 = 0.5;

/// A tab title fitted into the width its tab draws it in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FittedTitle {
    /// The drawn text: the whole title, or a grapheme prefix followed by `…`.
    pub text: String,
    /// Drawn width of `text` in raster pixels.
    pub width_px: f32,
    /// Whether `text` was cut to fit.
    pub cut: bool,
}

/// Fit `text` into `available_px` by measured width.
///
/// `advances` are the shaped pen advances of `text` as `(cluster byte offset,
/// advance)` pairs in left-to-right order, and `ellipsis_px` is the measured
/// width of `…`. Text that fits is returned whole. Otherwise the longest
/// prefix that ends on a grapheme boundary and fits beside `…` is kept, so a
/// CJK character, an emoji sequence or a combining mark is never split, and a
/// leading `#N` index, process icon and command badge survive whenever they
/// fit beside `…`. When even `…` does not fit, nothing is drawn.
#[must_use]
pub fn fit_title_to_width(
    text: &str,
    advances: &[(usize, f32)],
    ellipsis_px: f32,
    available_px: f32,
) -> FittedTitle {
    let whole_px: f32 = advances.iter().map(|(_, advance)| advance).sum();
    if whole_px <= available_px + TITLE_FIT_TOLERANCE_PX {
        // When: `whole_px` fits `available_px`, draw the whole title without an ellipsis.
        return FittedTitle { text: text.to_string(), width_px: whole_px, cut: false };
    }
    let budget_px = available_px + TITLE_FIT_TOLERANCE_PX - ellipsis_px;
    if budget_px < 0.0 {
        // When: `budget_px` is negative, not even the ellipsis fits, so nothing is drawn.
        return FittedTitle { text: String::new(), width_px: 0.0, cut: true };
    }
    let mut kept_end = 0;
    let mut kept_px = 0.0;
    let mut prefix_px = 0.0;
    let mut cluster = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        while cluster < advances.len() && advances[cluster].0 < end {
            prefix_px += advances[cluster].1;
            cluster += 1;
        }
        if prefix_px > budget_px {
            // When: `prefix_px` passes `budget_px`, this grapheme no longer fits beside the
            // ellipsis, so the cut lands on its start.
            break;
        }
        kept_end = end;
        kept_px = prefix_px;
    }
    let mut kept = String::with_capacity(kept_end + '…'.len_utf8());
    kept.push_str(&text[..kept_end]);
    kept.push('…');
    FittedTitle { text: kept, width_px: kept_px + ellipsis_px, cut: true }
}

pub(super) fn title_body(title: &str) -> &str {
    let trimmed = title.trim();
    let Some(rest) = trimmed.strip_prefix('#') else {
        // When: trimmed opens with no '#', so nothing was prefixed and the
        // whole title already is the body.
        return trimmed;
    };
    let Some(space) = rest.find(' ') else {
        // When: rest holds no separator, so no body was appended after the
        // index and trimmed is returned unchanged.
        return trimmed;
    };
    let after_index = trimmed[space + 1..].trim_start();
    let mut parts = after_index.splitn(2, ' ');
    let first = parts.next().unwrap_or_default();
    let rest = parts.next();
    let has_icon = rest.is_some()
        && first.chars().count() <= 2
        && !first.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '/' || character == '~'
        });
    if has_icon {
        rest.unwrap_or_default().trim_start()
    } else {
        // When: has_icon is false, so no leading glyph was split off and the
        // whole after_index span is the body.
        after_index
    }
}

/// Strip a leading `#<digits>` index prefix (if any) from a tab title,
/// returning the remaining body. Used by `recompute_all_titles` so a tab
/// can be re-prefixed with its current position without doubling up the
/// `#N`. The new wezterm-parity format places the icon directly after
/// the digits with no space (`#1{icon} body`), so we strip only the
/// `#<digits>` portion; any space (legacy bare-title fallback) is left
/// in the body verbatim.
pub(super) fn strip_index_prefix(title: &str) -> Option<&str> {
    let rest = title.strip_prefix('#')?;
    let digits_end = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
    if digits_end == 0 {
        // When: digits_end is zero, so no digit follows the '#' and the title
        // carries no position number to strip.
        return None;
    }
    Some(&rest[digits_end..])
}

#[cfg(test)]
#[path = "tabs_title_tests.rs"]
mod tabs_title_tests;
