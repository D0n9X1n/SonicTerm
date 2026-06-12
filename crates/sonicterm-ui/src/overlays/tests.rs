
use super::*;

#[test]
fn search_bar_uses_300_to_600_width_window() {
    let small = SearchBarLayout::compute(1000.0, 800.0, 10.0, 1.0);
    assert_eq!(small.border.w, SEARCH_BAR_MIN_WIDTH);

    let medium = SearchBarLayout::compute(1000.0, 800.0, 360.0, 1.0);
    assert!(medium.border.w > SEARCH_BAR_MIN_WIDTH);
    assert!(medium.border.w < SEARCH_BAR_WIDTH);

    let large = SearchBarLayout::compute(1000.0, 800.0, 1000.0, 1.0);
    assert_eq!(large.border.w, SEARCH_BAR_WIDTH);
}

#[test]
fn search_bar_second_row_sits_below_first_row() {
    let first = SearchBarLayout::compute_at_row(1000.0, 800.0, 10.0, 0, 1.0);
    let second = SearchBarLayout::compute_at_row(1000.0, 800.0, 10.0, 1, 1.0);
    assert!(second.border.y > first.border.y);
}

#[test]
fn search_bar_row1_clears_dpi_scaled_readonly_badge() {
    // Regression (#657-adjacent): at scale 2.0 the read-only badge is
    // SEARCH_BAR_HEIGHT*2 tall anchored at SEARCH_BAR_MARGIN. The search
    // bar at row 1 must sit BELOW the badge's bottom edge, not overlap it.
    let scale = 2.0;
    let badge_bottom = SEARCH_BAR_MARGIN + SEARCH_BAR_HEIGHT * scale;
    let row1 = SearchBarLayout::compute_at_row(4000.0, 2400.0, 10.0, 1, scale);
    assert!(
        row1.border.y >= badge_bottom,
        "search bar row1 (y={}) overlaps the scaled badge (bottom={})",
        row1.border.y,
        badge_bottom
    );
}

#[test]
fn search_bar_height_scales_2x_on_large_window() {
    // Window is huge so the window-relative clamps never bind; only
    // the SIZE terms drive the result and they should double at 2x.
    let one = SearchBarLayout::compute(4000.0, 2400.0, 5000.0, 1.0);
    let two = SearchBarLayout::compute(4000.0, 2400.0, 5000.0, 2.0);
    assert_eq!(two.border.h, one.border.h * 2.0);
    // Content saturates the cap at both scales, so the width is the
    // scaled SEARCH_BAR_WIDTH and must double too.
    assert_eq!(one.border.w, SEARCH_BAR_WIDTH);
    assert_eq!(two.border.w, SEARCH_BAR_WIDTH * 2.0);
}

#[test]
fn search_bar_clamp_binds_on_small_window_at_2x() {
    // 2x scale would want an 88px-tall, up-to-1200px-wide bar, but the
    // 700x400 window forces the clamp. The bar must stay inside it.
    let layout = SearchBarLayout::compute(700.0, 400.0, 100.0, 2.0);
    assert!(layout.border.x + layout.border.w <= 700.0);
    assert!(layout.border.y + layout.border.h <= 400.0);
    assert!(layout.bg.x + layout.bg.w <= 700.0);
    assert!(layout.bg.y + layout.bg.h <= 400.0);
}

#[test]
fn search_bar_position_is_window_anchored() {
    // The right-edge gap is a window-relative POSITION term: it equals
    // SEARCH_BAR_MARGIN regardless of scale.
    let one = SearchBarLayout::compute(4000.0, 2400.0, 200.0, 1.0);
    let two = SearchBarLayout::compute(4000.0, 2400.0, 200.0, 2.0);
    assert_eq!(4000.0 - (one.border.x + one.border.w), SEARCH_BAR_MARGIN);
    assert_eq!(4000.0 - (two.border.x + two.border.w), SEARCH_BAR_MARGIN);
}

#[test]
fn caret_prefix_is_label_text_up_to_the_caret_marker() {
    // The prefix must equal the slice of `search_bar_label` that precedes
    // the `▏` caret, so both IME anchor sites land at the end of the query
    // (not after the ` — N/M` suffix). Build the label, split on `▏`, and
    // assert the prefix matches the leading half verbatim.
    let mut search = SearchState::new();
    search.query = "ni hao".to_string();
    let label = search_bar_label(&search, "");
    let prefix = search_query_caret_prefix(&search, "");
    let (head, _tail) = label.split_once('▏').expect("label carries a caret marker");
    assert_eq!(prefix, head);
    assert_eq!(prefix, "/ ni hao");
}

#[test]
fn caret_prefix_empty_query_is_just_the_prompt() {
    let search = SearchState::new();
    let label = search_bar_label(&search, "");
    let prefix = search_query_caret_prefix(&search, "");
    let (head, _tail) = label.split_once('▏').expect("label carries a caret marker");
    assert_eq!(prefix, head);
    assert_eq!(prefix, "/ ");
}

#[test]
fn command_palette_query_label_places_preedit_at_caret() {
    let mut palette = CommandPalette::new();
    palette.open();
    for ch in "nihao".chars() {
        palette.input_char(ch);
    }
    palette.move_cursor_left();
    palette.move_cursor_left();

    let label = command_palette_query_label(&palette, "中");
    let prefix = command_palette_query_caret_prefix(&palette, "中");
    let (head, tail) = label.split_once('▏').expect("label carries caret marker");

    assert_eq!(prefix, head);
    assert_eq!(head, "nih中");
    assert_eq!(tail, "ao");
}

#[test]
fn command_palette_uses_compact_spacing_tokens() {
    assert!(PALETTE_HEIGHT <= 400.0);
    assert!(PALETTE_MAX_HEIGHT <= 460.0);
    assert!(PALETTE_QUERY_HEIGHT <= 42.0);
    assert!(PALETTE_QUERY_PAD_Y <= 6.0);
    assert!(PALETTE_ROW_HEIGHT <= 28.0);
    assert!(PALETTE_ROW_GAP <= 2.0);
    assert!(PALETTE_FOOTER_HEIGHT <= 30.0);
}

#[test]
fn command_palette_layout_is_dense_but_keeps_text_centered() {
    let mut palette = CommandPalette::new();
    palette.open();
    palette.input_char('r');
    let layout = PaletteLayout::compute(&mut palette, 1800.0, 1000.0, PALETTE_INNER_PAD, 1.0)
        .expect("open palette has layout");

    assert_eq!(layout.border.h, PALETTE_HEIGHT);
    assert_eq!(layout.query_row.h, PALETTE_QUERY_HEIGHT);
    assert!(layout.rows.len() >= 10, "compact layout should fit a useful command list");
    for row in &layout.rows {
        assert_eq!(row.rect.h, PALETTE_ROW_HEIGHT);
    }
    assert_eq!(layout.footer.h, PALETTE_FOOTER_HEIGHT);
    assert_eq!(
        layout.query_icon.y,
        layout.query_row.y + (layout.query_row.h - layout.query_icon.h) * 0.5,
        "query icon remains vertically centered after compacting padding"
    );
}
