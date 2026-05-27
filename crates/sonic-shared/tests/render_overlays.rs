//! Tests for the state-overlay layout module (`sonic_shared::overlays`).
//!
//! These are pure-data tests: no wgpu device is required. They cover the
//! contract that the renderer relies on — that overlays appear when the
//! state machine is open and disappear when it is closed.

use sonic_shared::command_palette::CommandPalette;
use sonic_shared::ime::ImeState;
use sonic_shared::overlays::{
    search_bar_label, ImePreeditLayout, PaletteLayout, SearchBarLayout, PALETTE_FOOTER_HEIGHT,
    PALETTE_HEIGHT, PALETTE_QUERY_HEIGHT, PALETTE_ROW_ACCENT_W, PALETTE_ROW_HEIGHT, PALETTE_WIDTH,
};
use sonic_shared::search::SearchState;

#[test]
fn palette_layout_is_none_when_closed() {
    let mut p = CommandPalette::new();
    assert!(!p.is_open());
    assert!(PaletteLayout::compute(&mut p, 1200.0, 800.0).is_none());
}

#[test]
fn palette_layout_is_some_when_open() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("palette open");
    // Modal sits inside the window.
    assert!(layout.border.x >= 0.0);
    assert!(layout.border.y >= 0.0);
    assert!(layout.border.x + layout.border.w <= 1200.0);
    assert!(layout.border.y + layout.border.h <= 800.0);
    // Default modal size is honoured on a large enough window.
    assert!((layout.border.w - PALETTE_WIDTH).abs() < 0.5);
    assert!((layout.border.h - PALETTE_HEIGHT).abs() < 0.5);
    // Query row is non-empty. The redesign drops the `> ` prefix; the
    // label now starts directly with the user's query (or the block
    // cursor when empty) and the search icon stands in for the prompt.
    assert!(!layout.query_label.starts_with("> "));
    // At least one row appears with the default (empty) query — there
    // are many bindable actions.
    assert!(!layout.rows.is_empty());
    assert_eq!(layout.rows.len(), layout.row_labels.len());
    // First row is selected when the palette opens fresh.
    assert_eq!(layout.selected_row, Some(0));
}

#[test]
fn palette_layout_appears_in_glyph_label_list_when_open() {
    // Mirror the property the renderer relies on: when open, action
    // names are part of the label list that glyphon receives.
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    let joined = layout.row_labels.join("\n");
    assert!(joined.contains("New Tab"));
    assert!(joined.contains("Close Tab"));
    // …and hides when closed.
    p.close();
    assert!(PaletteLayout::compute(&mut p, 1200.0, 800.0).is_none());
}

#[test]
fn palette_layout_filters_with_query() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("newtab");
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    let joined = layout.row_labels.join("\n");
    assert!(joined.contains("New Tab"));
    assert!(!joined.contains("Close Tab"));
    // Query label echoes the typed string.
    assert!(layout.query_label.contains("newtab"));
}

#[test]
fn palette_layout_clamps_to_small_window() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 200.0, 160.0).expect("open");
    assert!(layout.border.w <= 200.0);
    assert!(layout.border.h <= 160.0);
}

#[test]
fn palette_layout_scrolls_to_keep_selection_visible() {
    let mut p = CommandPalette::new();
    p.open();
    // Move selection well past the first visible window.
    for _ in 0..20 {
        p.move_selection_down();
    }
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    // The selected row, if surfaced, must be inside the visible window.
    if let Some(sel) = layout.selected_row {
        assert!(sel < layout.rows.len(), "sel={} rows={}", sel, layout.rows.len());
    }
}

#[test]
fn search_bar_label_with_matches() {
    let mut s = SearchState::new();
    s.query = "abc".to_string();
    use sonic_shared::search::MatchRange;
    s.matches = vec![
        MatchRange { row: 0, col_start: 0, col_end: 3 },
        MatchRange { row: 1, col_start: 0, col_end: 3 },
    ];
    s.current = Some(0);
    let label = search_bar_label(&s);
    assert!(label.contains("abc"));
    assert!(label.contains("1/2"));
}

#[test]
fn search_bar_label_with_zero_matches() {
    let mut s = SearchState::new();
    s.query = "nope".to_string();
    let label = search_bar_label(&s);
    assert!(label.contains("nope"));
    assert!(label.contains("0/0"));
}

#[test]
fn search_bar_layout_sits_in_bottom_right() {
    let layout = SearchBarLayout::compute(1200.0, 800.0);
    // Bar is to the right of horizontal center and below the vertical center.
    assert!(layout.bg.x > 600.0);
    assert!(layout.bg.y > 400.0);
    // Doesn't escape the window.
    assert!(layout.bg.x + layout.bg.w <= 1200.0);
    assert!(layout.bg.y + layout.bg.h <= 800.0);
}

#[test]
fn search_bar_layout_clamps_for_narrow_window() {
    let layout = SearchBarLayout::compute(80.0, 60.0);
    assert!(layout.bg.x >= 0.0);
    assert!(layout.bg.y >= 0.0);
    assert!(layout.bg.x + layout.bg.w <= 80.0);
    assert!(layout.bg.y + layout.bg.h <= 60.0);
}

#[test]
fn ime_overlay_is_none_when_not_composing() {
    let ime = ImeState::new();
    assert!(ImePreeditLayout::compute(&ime, 100.0, 100.0, 8.0, 16.0, 1200.0, 800.0).is_none());
}

#[test]
fn ime_overlay_is_some_when_composing_and_hides_after_commit() {
    let mut ime = ImeState::new();
    ime.handle_enabled();
    ime.handle_preedit("にほ", Some((0, 6)));
    let layout = ImePreeditLayout::compute(&ime, 100.0, 100.0, 8.0, 16.0, 1200.0, 800.0)
        .expect("preedit live");
    // Sits below the cursor and inside the window.
    assert!(layout.bg.y >= 100.0 + 16.0 - 0.001);
    assert!(layout.bg.x + layout.bg.w <= 1200.0);
    assert!(layout.bg.y + layout.bg.h <= 800.0);
    // Underline lives at the bottom of the bg rect.
    assert!(layout.underline.y + layout.underline.h <= layout.bg.y + layout.bg.h + 0.001);

    ime.handle_commit("日本");
    assert!(ImePreeditLayout::compute(&ime, 100.0, 100.0, 8.0, 16.0, 1200.0, 800.0).is_none());
}

#[test]
fn ime_overlay_shifts_left_when_cursor_near_right_edge() {
    let mut ime = ImeState::new();
    ime.handle_enabled();
    ime.handle_preedit("hello world", Some((0, 11)));
    let layout =
        ImePreeditLayout::compute(&ime, 1180.0, 100.0, 8.0, 16.0, 1200.0, 800.0).expect("live");
    // Layout was shifted left so it doesn't escape the right edge.
    assert!(layout.bg.x + layout.bg.w <= 1200.0 + 0.001);
}

// ----- Issue #112 Round 1 redesign -----------------------------------------

#[test]
fn palette_modal_width_clamps_to_viewport() {
    let mut p = CommandPalette::new();
    p.open();
    // Big viewport: modal honours the ideal 680px width.
    let big = PaletteLayout::compute(&mut p, 1600.0, 1000.0).expect("open");
    assert!((big.border.w - PALETTE_WIDTH).abs() < 0.5);
    // Tight viewport: modal clamps to `viewport_w - 48`.
    let mut p2 = CommandPalette::new();
    p2.open();
    let tight = PaletteLayout::compute(&mut p2, 500.0, 1000.0).expect("open");
    assert!(tight.border.w <= 500.0 - 48.0 + 0.5);
    assert!(tight.border.w < PALETTE_WIDTH);
    // Modal never escapes the viewport.
    assert!(tight.border.x + tight.border.w <= 500.0 + 0.001);
}

#[test]
fn palette_query_field_height_is_52() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    assert!((layout.query_row.h - PALETTE_QUERY_HEIGHT).abs() < 0.001);
    assert!((PALETTE_QUERY_HEIGHT - 52.0).abs() < 0.001);
    // Search icon is positioned inside the query field at x=16.
    assert!((layout.query_icon.x - layout.query_row.x - 16.0).abs() < 0.001);
    assert!((layout.query_icon.w - 16.0).abs() < 0.001);
    // Placeholder text appears when the query is empty.
    let ph = layout.query_placeholder.as_deref().unwrap_or("");
    assert!(ph.contains("Search commands"));
}

#[test]
fn palette_row_selected_renders_left_accent() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    let sel = layout.selected_row.expect("first row selected on open");
    let row = layout.rows[sel];
    let accent = layout.selected_accent.expect("accent strip on selected row");
    // Accent strip is 3px wide, aligns to the row's left edge, and lives
    // vertically inside the row's bounds.
    assert!((accent.w - PALETTE_ROW_ACCENT_W).abs() < 0.001);
    assert!((accent.x - row.rect.x).abs() < 0.001);
    assert!(accent.y >= row.rect.y - 0.001);
    assert!(accent.y + accent.h <= row.rect.y + row.rect.h + 0.001);
    // Row height matches the redesign spec (40px).
    assert!((row.rect.h - PALETTE_ROW_HEIGHT).abs() < 0.001);
    assert!((PALETTE_ROW_HEIGHT - 40.0).abs() < 0.001);
    // When the palette closes the accent goes away (sanity check that
    // it isn't a sticky field).
    p.close();
    assert!(PaletteLayout::compute(&mut p, 1200.0, 800.0).is_none());
}

#[test]
fn palette_footer_shows_count_and_hint() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    // Footer rect sits at the bottom of the modal background.
    assert!((layout.footer.h - PALETTE_FOOTER_HEIGHT).abs() < 0.001);
    assert!(layout.footer.y + layout.footer.h <= layout.bg.y + layout.bg.h + 0.001);
    // Footer label carries both a command count and the nav hint.
    let label = &layout.footer_label;
    assert!(label.contains("command"), "footer missing count: {label}");
    assert!(label.contains("navigate"), "footer missing nav hint: {label}");
    assert!(label.contains("run"), "footer missing run hint: {label}");
    assert!(label.contains("close"), "footer missing close hint: {label}");
    // Empty-state hint appears when no matches.
    p.set_query("zzzzzzzzzzzz_no_match");
    let layout2 = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    assert!(layout2.empty_label.is_some());
    let hint = layout2.empty_hint.as_deref().unwrap_or("");
    assert!(hint.contains("Try"), "empty hint missing examples: {hint}");
}
