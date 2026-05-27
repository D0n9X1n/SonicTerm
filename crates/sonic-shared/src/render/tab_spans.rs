//! Tab title span builders extracted from `render.rs` (issue #143).

use glyphon::{Attrs, Color as GColor};
use sonic_text::terminal_font_attrs;

/// Input describing one tab for [`build_tab_title_spans`]: which slot it
/// occupies, its formatted title, its layout rect's x/width in logical
/// pixels, and whether it is the active tab.
#[doc(hidden)]
pub struct TabSpanInput<'a> {
    pub index: usize,
    pub title: &'a str,
    pub title_x: f32,
    pub title_w: f32,
    pub is_active: bool,
}

/// Horizontal padding (in logical pixels) reserved on EACH side of a tab's
/// title region before truncation kicks in. 6px on each side = 12px total
/// of breathing room, matching the design polish requirement.
#[doc(hidden)]
pub const TAB_TITLE_PADDING_PX: f32 = 6.0;

/// Tab-title font size given the body terminal font size, in logical px.
/// Tab titles render exactly 1.0 pt larger than the body — see PR
/// "feat(tabbar): centered title with config font, larger size".
/// Picked the additive `+ 1.0` form over `* 1.0625` because it scales
/// consistently across user font-size choices (a hard-coded ratio
/// quickly drifts at extreme sizes: a 10pt body would gain ~0.6pt,
/// a 24pt body ~1.5pt, neither matching the user's intent of "one
/// step up").
#[must_use]
pub fn tab_title_font_size(body_font_size: f32) -> f32 {
    body_font_size + 1.0
}

#[doc(hidden)]
pub struct TabTitleRichTextSpans<'a> {
    pub spans: Vec<(&'a str, Attrs<'a>)>,
    pub default_attrs: Attrs<'a>,
}

#[doc(hidden)]
#[must_use]
pub fn build_tab_title_rich_text_spans<'a>(
    title_text: &'a str,
    tab_spans: &[(std::ops::Range<usize>, GColor)],
    font_family: &'a str,
    inactive_fg: GColor,
) -> TabTitleRichTextSpans<'a> {
    let mut spans: Vec<(&str, Attrs<'_>)> = Vec::new();
    let mut cursor = 0usize;
    for (range, color) in tab_spans {
        if range.start > cursor {
            spans.push((
                &title_text[cursor..range.start],
                terminal_font_attrs(font_family).color(inactive_fg),
            ));
        }
        spans.push((
            &title_text[range.start..range.end],
            terminal_font_attrs(font_family).color(*color),
        ));
        cursor = range.end;
    }
    if cursor < title_text.len() {
        spans.push((&title_text[cursor..], terminal_font_attrs(font_family).color(inactive_fg)));
    }

    TabTitleRichTextSpans {
        spans,
        default_attrs: terminal_font_attrs(font_family).color(inactive_fg),
    }
}

#[doc(hidden)]
pub fn build_tab_title_spans(
    tabs: &[TabSpanInput<'_>],
    avg_glyph_w: f32,
    active_fg: GColor,
    inactive_fg: GColor,
) -> (String, Vec<(std::ops::Range<usize>, GColor)>) {
    let mut title_text = String::new();
    let mut spans: Vec<(std::ops::Range<usize>, GColor)> = Vec::new();
    for (i, t) in tabs.iter().enumerate() {
        let color = if t.is_active { active_fg } else { inactive_fg };
        // Reserve TAB_TITLE_PADDING_PX on each side before clipping.
        let usable_w = (t.title_w - 2.0 * TAB_TITLE_PADDING_PX).max(avg_glyph_w);
        let max_chars = ((usable_w / avg_glyph_w).floor() as usize).max(1);
        let full_chars = ((t.title_w / avg_glyph_w).floor() as usize).max(max_chars);

        // Truncate with `…` if the title overflows usable width.
        let title_chars: Vec<char> = t.title.chars().collect();
        let body: String = if title_chars.len() > max_chars {
            let keep = max_chars.saturating_sub(1);
            let mut s: String = title_chars.iter().take(keep).collect();
            s.push('…');
            s
        } else {
            title_chars.iter().collect()
        };
        let body_chars = body.chars().count();

        // Centering: text starts at title_x + (title_w - text_w)/2.
        // For ACTIVE tabs the leading & trailing pad spaces stay INSIDE
        // the colored span so the active tint covers the full rect
        // (preserves the pre-centering invariant). For INACTIVE tabs the
        // leading pad is plain prefix space — no need to tint empty cells.
        let text_w = body_chars as f32 * avg_glyph_w;
        let leading_px = t.title_x + ((t.title_w - text_w) / 2.0).max(0.0);
        let rect_left_col = (t.title_x / avg_glyph_w).floor() as usize;
        let center_col = (leading_px / avg_glyph_w).floor() as usize;
        let leading_pad = center_col.saturating_sub(rect_left_col);
        let trailing_pad = full_chars.saturating_sub(body_chars + leading_pad);

        let (anchor_col, raw) = if t.is_active {
            let mut s = String::with_capacity(leading_pad + body.len() + trailing_pad);
            s.extend(std::iter::repeat_n(' ', leading_pad));
            s.push_str(&body);
            s.extend(std::iter::repeat_n(' ', trailing_pad));
            (rect_left_col, s)
        } else {
            (center_col, body)
        };

        while title_text.chars().count() < anchor_col {
            title_text.push(' ');
        }
        // WezTerm-parity separator: the 1px vertical separator between
        // adjacent INACTIVE tabs is painted by the quad pipeline (see
        // the `tab_separator` block in `compute_quads`) — we MUST NOT
        // also inject a `│ ` text glyph here, or the user sees `| │`
        // doubled between every pair of inactive tabs. The quad alone
        // is the source of truth for tab separators.
        let _ = i;
        let start = title_text.len();
        title_text.push_str(&raw);
        let end = title_text.len();
        spans.push((start..end, color));
    }
    (title_text, spans)
}
