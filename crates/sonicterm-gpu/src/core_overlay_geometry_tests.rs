//! Renderer overlay geometry contracts, sharing one root fixture owner.

use super::*;

// --- Inline IME preedit opaque background -------------------------

#[test]
fn badge_width_never_uses_a_narrower_shaped_or_invalid_measurement() {
    assert_eq!(conservative_badge_text_width(120.0, Some(160.0)), 160.0);
    assert_eq!(conservative_badge_text_width(120.0, Some(90.0)), 120.0);
    assert_eq!(conservative_badge_text_width(120.0, Some(f32::NAN)), 120.0);
    assert_eq!(conservative_badge_text_width(120.0, None), 120.0);
}

#[test]
fn preedit_bg_rect_covers_the_glyph_run() {
    // The glyphs are emitted at emit_x = start_x + pad, across `pre_w`.
    // The background mask must start no later than start_x and extend past
    // the glyph run's right edge, so app placeholder/hint text under the
    // composing pinyin is fully masked.
    let start_x = 100.0;
    let top_y = 50.0;
    let pre_w = 64.0;
    let pad = 2.0;
    let line_h = 20.0;

    let (x, y, w, h) = preedit_bg_rect(start_x, top_y, pre_w, pad, line_h);

    // Left edge aligns with the cursor cell (no later than where glyphs start).
    assert_eq!(x, start_x);
    assert!(x <= start_x + pad, "mask starts at/under the glyph emit_x");
    // One line tall, anchored at the cell top.
    assert_eq!(y, top_y);
    assert_eq!(h, line_h);
    // Right edge reaches past the glyph run end (emit_x + pre_w).
    let glyph_right = (start_x + pad) + pre_w;
    assert!(x + w >= glyph_right, "mask must cover the full glyph run, got right={}", x + w);
}

#[test]
fn preedit_bg_rect_width_is_at_least_pre_w() {
    // Width must never be narrower than the run width used to lay glyphs,
    // otherwise the tail of the composing text shows through to whatever is
    // underneath. It also must not be absurdly wide (bleeding onto adjacent
    // cells): width == pre_w + pad exactly.
    let (_, _, w, _) = preedit_bg_rect(0.0, 0.0, 80.0, 2.0, 18.0);
    assert!(w >= 80.0, "mask at least as wide as the glyph run");
    assert_eq!(w, 82.0, "mask is exactly pre_w + pad, not wider");
}

#[test]
fn preedit_bg_rect_zero_pad_equals_pre_w() {
    let (_, _, w, _) = preedit_bg_rect(10.0, 10.0, 40.0, 0.0, 16.0);
    assert_eq!(w, 40.0);
}

#[test]
fn preedit_overlay_skips_whitespace_only_preedit() {
    // Regression: a whitespace-only preedit (macOS can momentarily deliver a
    // bare space as marked text during ordinary typing) carries no glyph
    // ink, but the inline overlay's underline is clamped to >= one cell —
    // so drawing it left a stray ~1-cell underscore at the cursor that
    // lingered until the next repaint. The overlay must be suppressed.
    assert!(!preedit_has_visible_ink(""), "empty preedit draws nothing");
    assert!(!preedit_has_visible_ink(" "), "a bare space must not draw an underline");
    assert!(!preedit_has_visible_ink("   "), "all-whitespace must not draw");
    assert!(!preedit_has_visible_ink("\t"), "a tab is whitespace");
}

#[test]
fn preedit_overlay_draws_for_real_composition() {
    // Genuine composition always carries non-whitespace ink, so the overlay
    // is never suppressed for real CJK / multi-key input.
    assert!(preedit_has_visible_ink("ni"), "latin composing run draws");
    assert!(preedit_has_visible_ink("\u{4f60}"), "CJK composing run draws");
    assert!(preedit_has_visible_ink("a b"), "ink with embedded space draws");
}

#[test]
fn preedit_caret_advance_zero_for_whitespace_only() {
    // the terminal-cursor caret advance MUST use the same visible-ink
    // gate as the glyph overlay. macOS delivers a whitespace-only marked
    // string during ordinary typing / bare Enter with a CJK source active;
    // advancing the cursor for it shoved the cursor-colored block into empty
    // prompt space with no glyph under it (the stray "yellow line/block").
    assert_eq!(preedit_caret_advance("", 0, 16.0), 0.0, "empty → no advance");
    assert_eq!(preedit_caret_advance(" ", 1, 16.0), 0.0, "bare space → no advance");
    assert_eq!(preedit_caret_advance("   ", 3, 16.0), 0.0, "all-whitespace → no advance");
    assert_eq!(preedit_caret_advance("\t", 1, 16.0), 0.0, "tab → no advance");
}

#[test]
fn preedit_caret_advance_nonzero_for_real_composition() {
    // Real composition still advances the caret to the insertion point.
    assert!(preedit_caret_advance("ni", 2, 16.0) > 0.0, "latin composing run advances the caret");
    assert!(
        preedit_caret_advance("\u{4f60}", 3, 16.0) > 0.0,
        "CJK composing run advances the caret"
    );
    // A caret byte that lands off a char boundary falls back to full width
    // rather than panicking.
    let full = preedit_caret_advance("\u{4f60}", 3, 16.0);
    assert_eq!(
        preedit_caret_advance("\u{4f60}", 1, 16.0),
        full,
        "non-boundary caret byte falls back to full width"
    );
}
