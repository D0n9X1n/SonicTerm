//! Tab title span builders extracted from `render.rs` (issue #143).
//!
//! T14 (wezterm-takeover G3): the legacy the legacy chrome layer `Attrs` / `Color`
//! types are gone. Spans are now built around two local types:
//!
//! - [`TabSpanColor`] — sRGB-encoded `(r, g, b, a)` u8s, byte-identical
//!   to the now-deleted `legacy chrome color` so renderer-side colour-LUT
//!   conversions don't change.
//! - [`TabSpanAttrs`] — `(bold, italic)` toggles. The renderer no
//!   longer needs the cosmic-text `Family::Name(...)` attribute slot;
//!   chrome shaping reaches the user's configured face via
//!   `FontStack::default_font()` and per-span bold/italic is applied
//!   at chrome_text::layout time.
//!
//! `build_tab_title_rich_text_spans` returns `(text, TabSpanColor,
//! TabSpanAttrs)` tuples that the renderer feeds straight into
//! `emit_tab_title_glyphs`.

/// Per-span colour for tab titles. sRGB-encoded u8 channels, matching
/// the byte layout of the deleted `legacy chrome color` so renderer-side
/// sRGB→linear conversion (`chrome_color_to_linear_rgba`) is
/// bit-identical to the legacy path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSpanColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl TabSpanColor {
    /// Construct an opaque colour.
    #[inline]
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Construct a colour with explicit alpha.
    #[inline]
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Red channel accessor.
    #[inline]
    #[must_use]
    pub const fn r(&self) -> u8 {
        self.r
    }
    /// Green channel accessor.
    #[inline]
    #[must_use]
    pub const fn g(&self) -> u8 {
        self.g
    }
    /// Blue channel accessor.
    #[inline]
    #[must_use]
    pub const fn b(&self) -> u8 {
        self.b
    }
    /// Alpha channel accessor.
    #[inline]
    #[must_use]
    pub const fn a(&self) -> u8 {
        self.a
    }
}

/// Per-span style for tab titles. Matches the chrome_text `ChromeAttrs`
/// shape so renderer call sites can forward the value without
/// translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabSpanAttrs {
    pub bold: bool,
    pub italic: bool,
}

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
    pub badge: Option<&'a str>,
}

/// Horizontal padding (in logical pixels) reserved on EACH side of a tab's
/// title region before truncation kicks in. 6px on each side = 12px total
/// of breathing room, matching the design polish requirement.
#[doc(hidden)]
pub const TAB_TITLE_PADDING_PX: f32 = 6.0;

/// Tab-title font size given the body terminal font size, in logical px.
/// Tab titles render exactly 1.0 pt larger than the body — see PR
/// "feat(tabbar): centered title with config font, larger size".
/// Picked the additive `+ 1.0` form over a ratio because it scales
/// consistently across user font-size choices (a hard-coded ratio
/// quickly drifts at extreme sizes).
#[must_use]
pub fn tab_title_font_size(body_font_size: f32) -> f32 {
    body_font_size + 1.0
}

/// Output of [`build_tab_title_rich_text_spans`]: a vec of
/// `(text, colour, attrs)` tuples plus a default-attribute fallback.
/// The renderer collects this and feeds each tuple into
/// `chrome_text::layout` (one call per span) so the entire title row
/// shares the wezterm-driven atlas.
#[doc(hidden)]
pub struct TabTitleRichTextSpans<'a> {
    pub spans: Vec<(&'a str, TabSpanColor, TabSpanAttrs)>,
    pub default_color: TabSpanColor,
    pub default_attrs: TabSpanAttrs,
}

#[doc(hidden)]
#[must_use]
pub fn build_tab_title_rich_text_spans<'a>(
    title_text: &'a str,
    tab_spans: &[(std::ops::Range<usize>, TabSpanColor)],
    _font_family: &'a str,
    inactive_fg: TabSpanColor,
) -> TabTitleRichTextSpans<'a> {
    let mut spans: Vec<(&str, TabSpanColor, TabSpanAttrs)> = Vec::new();
    let mut cursor = 0usize;
    for (range, color) in tab_spans {
        if range.start > cursor {
            spans.push((&title_text[cursor..range.start], inactive_fg, TabSpanAttrs::default()));
        }
        spans.push((&title_text[range.start..range.end], *color, TabSpanAttrs::default()));
        cursor = range.end;
    }
    if cursor < title_text.len() {
        spans.push((&title_text[cursor..], inactive_fg, TabSpanAttrs::default()));
    }

    TabTitleRichTextSpans {
        spans,
        default_color: inactive_fg,
        default_attrs: TabSpanAttrs::default(),
    }
}

#[doc(hidden)]
pub fn build_tab_title_spans(
    tabs: &[TabSpanInput<'_>],
    avg_glyph_w: f32,
    active_fg: TabSpanColor,
    inactive_fg: TabSpanColor,
) -> (String, Vec<(std::ops::Range<usize>, TabSpanColor)>) {
    let mut title_text = String::new();
    let mut spans: Vec<(std::ops::Range<usize>, TabSpanColor)> = Vec::new();
    for (i, t) in tabs.iter().enumerate() {
        let color = if t.is_active { active_fg } else { inactive_fg };
        // Reserve TAB_TITLE_PADDING_PX on each side before clipping.
        let usable_w = (t.title_w - 2.0 * TAB_TITLE_PADDING_PX).max(avg_glyph_w);
        let max_chars = ((usable_w / avg_glyph_w).floor() as usize).max(1);
        let full_chars = ((t.title_w / avg_glyph_w).floor() as usize).max(max_chars);

        // Truncate with `…` if the title overflows usable width.
        let display_title;
        let title = if let Some(badge) = t.badge {
            display_title = format!("{badge} {}", t.title);
            display_title.as_str()
        } else {
            t.title
        };
        let title_chars: Vec<char> = title.chars().collect();
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
            s.extend(std::iter::repeat(' ').take(leading_pad));
            s.push_str(&body);
            s.extend(std::iter::repeat(' ').take(trailing_pad));
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
