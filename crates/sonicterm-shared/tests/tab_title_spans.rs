//! Regression: every character within the active tab's title region must
//! be coloured with `tab.active_fg` (gold) — not just the leading icon /
//! `#N` digits. The pre-fix gap-fill painted any trailing space slots in
//! the inactive cream colour, leaving the rest of the title visibly
//! mismatched against the prefix glyphs.

use glyphon::Color as GColor;
use sonicterm_shared::render::{build_tab_title_spans, TabSpanInput};

const ACTIVE: GColor = GColor::rgb(0xfa, 0xbd, 0x2f);
const INACTIVE: GColor = GColor::rgb(0x92, 0x83, 0x74);

#[test]
fn active_tab_span_covers_full_title_rect_width() {
    // Single tab, full rect 40 chars wide at 10px-per-glyph.
    // Body is 4 chars; centering pads it inside the span on both sides
    // so the active tint still covers the entire rect (38 usable +
    // 2 edge cells from the 12px padding budget = 40 total).
    let title = "#1 X"; // 4 chars / 4 bytes (ASCII for test determinism)
    let avg_glyph_w = 10.0_f32;
    let title_w = 400.0_f32; // → full_chars = 40, max_chars = 38
    let inputs =
        [TabSpanInput { index: 0, title, title_x: 0.0, title_w, is_active: true, badge: None }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);

    assert_eq!(spans.len(), 1);
    let (range, color) = &spans[0];
    assert_eq!(*color, ACTIVE, "active tab span must be gold");
    // Padded with leading + trailing spaces to cover the full rect.
    assert_eq!(range.end - range.start, 40, "active span must cover the full rect width");
    let slice = &text[range.clone()];
    assert_eq!(slice.chars().count(), 40);
    // Centered: body sits roughly in the middle, surrounded by spaces.
    assert!(slice.contains("#1 X"));
    let before = slice.split("#1 X").next().unwrap();
    let after = slice.split("#1 X").nth(1).unwrap();
    let lead = before.chars().count();
    let trail = after.chars().count();
    // Centered within ±1 cell tolerance.
    assert!(
        (lead as i32 - trail as i32).abs() <= 1,
        "expected centered: lead={lead} trail={trail}"
    );
}

#[test]
fn every_char_in_active_span_is_gold() {
    // Mirrors what the renderer does: build the per-character colour
    // sequence and assert that every byte of the active tab's span is
    // gold (the active span now wraps body in leading/trailing spaces
    // to keep covering the full rect after centering).
    let title = "#2 nvim editor";
    let avg_glyph_w = 8.0_f32;
    let title_w = 160.0_f32; // → full_chars = 20
    let inputs =
        [TabSpanInput { index: 0, title, title_x: 0.0, title_w, is_active: true, badge: None }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);

    let mut per_byte = vec![INACTIVE; text.len()];
    for (r, c) in &spans {
        for i in r.clone() {
            per_byte[i] = *c;
        }
    }
    // Every byte within the active span must be gold.
    let (range, _) = &spans[0];
    for i in range.clone() {
        assert_eq!(per_byte[i], ACTIVE, "byte {i} in active span must be gold");
    }
    // And the span covers the full rect's char count.
    assert_eq!(text[range.clone()].chars().count(), 20);
}

#[test]
fn inactive_tab_span_uses_inactive_fg() {
    let inputs = [TabSpanInput {
        index: 0,
        title: "#1 shell",
        title_x: 0.0,
        title_w: 80.0,
        is_active: false,
        badge: None,
    }];
    let (_text, spans) = build_tab_title_spans(&inputs, 10.0, ACTIVE, INACTIVE);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].1, INACTIVE);
}

#[test]
fn no_text_separator_between_inactive_and_active_tab() {
    // WezTerm parity: no separator at all in a gap that borders the
    // active tab. The 1px quad separator handles the inactive↔inactive
    // case in the renderer; the text path no longer emits `│ ` (which
    // previously stacked with the quad and produced a doubled
    // `| │` glyph between adjacent inactive tabs).
    let inputs = [
        TabSpanInput {
            index: 0,
            title: "#1 shell",
            title_x: 0.0,
            title_w: 80.0,
            is_active: false,
            badge: None,
        },
        TabSpanInput {
            index: 1,
            title: "#2 nvim",
            title_x: 90.0,
            title_w: 80.0,
            is_active: true,
            badge: None,
        },
    ];
    let (text, spans) = build_tab_title_spans(&inputs, 10.0, ACTIVE, INACTIVE);
    // Only title spans now: [inactive #1, active #2]. No separator span.
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].1, INACTIVE);
    assert_eq!(spans[1].1, ACTIVE);
    assert!(!text.contains('\u{2502}'), "must not emit `│` text separator");
}

#[test]
fn three_inactive_no_double_separator() {
    // Reproduces the user-visible regression: with three inactive tabs
    // followed by an active one, the OLD code emitted `│ ` text prefix
    // on every tab past the first AND the renderer painted a quad
    // separator in the inactive↔inactive gaps, producing `| │` doubled
    // bars. After the fix the text path is silent and only the quad
    // separator survives.
    let inputs = [
        TabSpanInput {
            index: 0,
            title: "#1 a",
            title_x: 0.0,
            title_w: 80.0,
            is_active: false,
            badge: None,
        },
        TabSpanInput {
            index: 1,
            title: "#2 b",
            title_x: 90.0,
            title_w: 80.0,
            is_active: false,
            badge: None,
        },
        TabSpanInput {
            index: 2,
            title: "#3 c",
            title_x: 180.0,
            title_w: 80.0,
            is_active: false,
            badge: None,
        },
        TabSpanInput {
            index: 3,
            title: "#4 d",
            title_x: 270.0,
            title_w: 80.0,
            is_active: true,
            badge: None,
        },
    ];
    let (text, _spans) = build_tab_title_spans(&inputs, 10.0, ACTIVE, INACTIVE);
    assert_eq!(
        text.matches('\u{2502}').count(),
        0,
        "no `│` glyphs — quad pipeline owns the separator now"
    );
}

#[test]
fn tab_title_centered_within_rect() {
    // Inactive tab so the span is the raw body (no full-rect padding):
    // we can directly inspect where the body lands.
    let title = "abcd"; // 4 chars
    let avg_glyph_w = 10.0_f32;
    let title_x = 100.0_f32;
    let title_w = 100.0_f32; // rect spans x=100..200
    let inputs =
        [TabSpanInput { index: 0, title, title_x, title_w, is_active: false, badge: None }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);
    let (range, _) = &spans[0];
    let body = &text[range.clone()];
    assert_eq!(body, "abcd");
    // The body should start at the column corresponding to the
    // rect-center minus half the text width: center_x = 150,
    // text_w = 40, leading_px = 150 - 20 = 130 → col 13.
    let start_col_chars = text[..range.start].chars().count();
    let expected_start_col =
        ((title_x + (title_w - 4.0 * avg_glyph_w) / 2.0) / avg_glyph_w).floor() as usize;
    let delta = (start_col_chars as i32 - expected_start_col as i32).abs();
    assert!(
        delta <= 1,
        "centered text start col {start_col_chars} should be within ±1 of {expected_start_col}"
    );
}

#[test]
fn tab_title_truncates_with_ellipsis_when_overflow() {
    // Long title at small rect → must truncate with `…`.
    let title = "#1 super-long-tab-title-that-overflows";
    let avg_glyph_w = 10.0_f32;
    // Rect = 100px; usable after 6px×2 padding = 88px → max_chars=8.
    let title_w = 100.0_f32;
    let inputs =
        [TabSpanInput { index: 0, title, title_x: 0.0, title_w, is_active: false, badge: None }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);
    let body = &text[spans[0].0.clone()];
    assert!(body.ends_with('…'), "truncated title must end with `…`, got: {body:?}");
    assert!(body.chars().count() <= 8, "truncated body must fit usable width");
    assert!(body.starts_with("#1 "), "leading prefix must survive truncation");
}

#[test]
fn tab_title_no_truncation_when_fits() {
    let title = "#1 ok";
    let avg_glyph_w = 10.0_f32;
    let title_w = 200.0_f32;
    let inputs =
        [TabSpanInput { index: 0, title, title_x: 0.0, title_w, is_active: false, badge: None }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);
    let body = &text[spans[0].0.clone()];
    assert!(!body.contains('…'));
    assert!(body.contains("#1 ok"));
}

#[test]
fn tab_font_size_one_pt_larger_than_body() {
    use sonicterm_ui::tab_spans::tab_title_font_size;
    // The +1.0 contract holds across the configurable size range.
    for body in [10.0_f32, 12.0, 14.0, 15.0, 16.0, 18.0, 22.0, 28.0] {
        let tab = tab_title_font_size(body);
        let delta = tab - body;
        assert!(
            (delta - 1.0).abs() < f32::EPSILON,
            "tab font {tab} should be exactly 1.0pt above body {body}; got delta={delta}"
        );
    }
}
