//! Tests for the state-overlay layout module (`sonic_shared::overlays`).
//!
//! These are pure-data tests: no wgpu device is required. They cover the
//! contract that the renderer relies on — that overlays appear when the
//! state machine is open and disappear when it is closed.

use sonic_shared::command_palette::CommandPalette;
use sonic_shared::ime::ImeState;
use sonic_shared::overlays::{
    search_bar_label, ImePreeditLayout, PaletteLayout, SearchBarLayout, PALETTE_HEIGHT,
    PALETTE_WIDTH,
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
    // Query row is non-empty and starts with the prompt prefix.
    assert!(layout.query_label.starts_with("> "));
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
