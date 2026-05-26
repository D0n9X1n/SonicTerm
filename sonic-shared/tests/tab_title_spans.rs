//! Regression: every character within the active tab's title region must
//! be coloured with `tab.active_fg` (gold) — not just the leading icon /
//! `#N` digits. The pre-fix gap-fill painted any trailing space slots in
//! the inactive cream colour, leaving the rest of the title visibly
//! mismatched against the prefix glyphs.

use glyphon::Color as GColor;
use sonic_shared::render::{build_tab_title_spans, TabSpanInput};

const ACTIVE: GColor = GColor::rgb(0xfa, 0xbd, 0x2f);
const INACTIVE: GColor = GColor::rgb(0x92, 0x83, 0x74);

#[test]
fn active_tab_span_covers_full_title_rect_width() {
    // Single tab, 40 chars wide at 10px-per-glyph. Title is just `#1 ⚙` —
    // pre-fix `raw` was 4 chars, so only those four would be gold. After
    // the fix `raw` is padded with spaces out to max_chars (40), so the
    // active span covers byte_range == raw_bytes.
    let title = "#1 X"; // 4 chars / 4 bytes (ASCII for test determinism)
    let avg_glyph_w = 10.0_f32;
    let title_w = 400.0_f32; // → max_chars = 40
    let inputs = [TabSpanInput { index: 0, title, title_x: 0.0, title_w, is_active: true }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);

    // Exactly one span for one tab (no separator for index 0).
    assert_eq!(spans.len(), 1);
    let (range, color) = &spans[0];
    assert_eq!(*color, ACTIVE, "active tab span must be gold");
    // Padded out to 40 chars / 40 bytes.
    assert_eq!(range.end - range.start, 40, "active span must cover the full rect width");
    assert_eq!(&text[range.clone()], "#1 X                                    ");
}

#[test]
fn every_char_in_active_span_is_gold() {
    // Mirrors what the renderer does: build the per-character colour
    // sequence and assert that EVERY char of the active tab's region is
    // gold, not just the prefix.
    let title = "#2 nvim editor";
    let avg_glyph_w = 8.0_f32;
    let title_w = 160.0_f32; // → max_chars = 20
    let inputs = [TabSpanInput { index: 0, title, title_x: 0.0, title_w, is_active: true }];
    let (text, spans) = build_tab_title_spans(&inputs, avg_glyph_w, ACTIVE, INACTIVE);

    // Resolve each byte to its span colour (default = inactive when
    // outside any span). The renderer mirrors this exact logic when
    // building the spans2 vector handed to glyphon::set_rich_text.
    let mut per_byte = vec![INACTIVE; text.len()];
    for (r, c) in &spans {
        for i in r.clone() {
            per_byte[i] = *c;
        }
    }
    // The active span occupies bytes 0..max_chars=20.
    for (i, c) in per_byte.iter().enumerate().take(20) {
        assert_eq!(*c, ACTIVE, "byte {i} in active tab region must be gold");
    }
}

#[test]
fn inactive_tab_span_uses_inactive_fg() {
    let inputs = [TabSpanInput {
        index: 0,
        title: "#1 shell",
        title_x: 0.0,
        title_w: 80.0,
        is_active: false,
    }];
    let (_text, spans) = build_tab_title_spans(&inputs, 10.0, ACTIVE, INACTIVE);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].1, INACTIVE);
}

#[test]
fn separator_span_always_uses_inactive_fg_even_next_to_active_tab() {
    let inputs = [
        TabSpanInput { index: 0, title: "#1 shell", title_x: 0.0, title_w: 80.0, is_active: false },
        TabSpanInput { index: 1, title: "#2 nvim", title_x: 90.0, title_w: 80.0, is_active: true },
    ];
    let (_text, spans) = build_tab_title_spans(&inputs, 10.0, ACTIVE, INACTIVE);
    // Spans: [inactive title #1, inactive separator, active title #2]
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].1, INACTIVE, "tab 1 title");
    assert_eq!(spans[1].1, INACTIVE, "separator must stay dim");
    assert_eq!(spans[2].1, ACTIVE, "tab 2 title gold");
}
