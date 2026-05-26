//! Integration tests for the command palette state.

use sonic_core::keymap::{Action, Direction, ScrollAction};
use sonic_shared::command_palette::{action_display_name, all_actions, CommandPalette};

#[test]
fn starts_closed_and_empty_query() {
    let p = CommandPalette::new();
    assert!(!p.is_open());
    assert_eq!(p.query(), "");
    // Even when closed the full universe is available — refilter on open
    // is the canonical reset.
    assert!(!p.is_empty());
}

#[test]
fn open_clears_query_and_resets_selection() {
    let mut p = CommandPalette::new();
    p.set_query("zzznevermatches");
    assert!(p.is_empty());
    p.open();
    assert!(p.is_open());
    assert_eq!(p.query(), "");
    assert_eq!(p.selected(), 0);
    assert_eq!(p.len(), all_actions().len());
}

#[test]
fn set_query_ne_filters_to_actions_containing_subsequence_ne() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("ne");
    let visible: Vec<String> = p.visible().iter().map(|a| action_display_name(a)).collect();
    // NewTab, NewWindow, NextTab — every action whose lowercased name
    // contains the subsequence "ne".
    assert!(visible.iter().any(|n| n == "NewTab"), "visible = {visible:?}");
    assert!(visible.iter().any(|n| n == "NextTab"), "visible = {visible:?}");
    assert!(visible.iter().any(|n| n == "NewWindow"), "visible = {visible:?}");
    // Sanity: a totally unrelated action should be filtered out.
    assert!(!visible.iter().any(|n| n == "CopyToClipboard"));
}

#[test]
fn input_char_appends_and_refilters_incrementally() {
    let mut p = CommandPalette::new();
    p.open();
    p.input_char('n');
    p.input_char('e');
    p.input_char('w');
    assert_eq!(p.query(), "new");
    let visible: Vec<String> = p.visible().iter().map(|a| action_display_name(a)).collect();
    assert!(visible.iter().any(|n| n == "NewTab"));
    assert!(visible.iter().any(|n| n == "NewWindow"));
    // "CopyToClipboard" does not contain n,e,w as a subsequence.
    assert!(!visible.iter().any(|n| n == "CopyToClipboard"));
}

#[test]
fn backspace_widens_filter() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("newt");
    let narrow = p.len();
    p.backspace();
    assert_eq!(p.query(), "new");
    assert!(p.len() >= narrow);
}

#[test]
fn enter_returns_currently_selected_action() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("ne");
    // "current" gives us what Enter would dispatch.
    let current = p.current().cloned().expect("at least one match for 'ne'");
    // Must be one of the actions whose display name contains "ne" as a
    // subsequence — specifically the first hit in canonical order.
    let name = action_display_name(&current);
    assert!(name.to_lowercase().contains('n'));
    assert!(matches!(current, Action::NewTab | Action::NewWindow | Action::NextTab));
}

#[test]
fn esc_closes_and_clears() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("ne");
    p.close();
    assert!(!p.is_open());
    assert_eq!(p.query(), "");
    assert_eq!(p.selected(), 0);
}

#[test]
fn move_selection_wraps_around_bounds() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query(""); // full list
    let n = p.len();
    assert!(n >= 3);
    assert_eq!(p.selected(), 0);
    p.move_selection_up(); // wraps to last
    assert_eq!(p.selected(), n - 1);
    p.move_selection_down(); // wraps back to first
    assert_eq!(p.selected(), 0);
    for _ in 0..n {
        p.move_selection_down();
    }
    assert_eq!(p.selected(), 0, "full loop returns to start");
}

#[test]
fn move_selection_on_empty_is_noop() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("zzzzznevermatchesanything");
    assert!(p.is_empty());
    p.move_selection_down();
    p.move_selection_up();
    assert_eq!(p.selected(), 0);
    assert!(p.current().is_none());
}

#[test]
fn toggle_flips_open_state() {
    let mut p = CommandPalette::new();
    assert!(!p.is_open());
    assert!(p.toggle());
    assert!(p.is_open());
    assert!(!p.toggle());
    assert!(!p.is_open());
}

#[test]
fn all_actions_covers_every_variant_kind() {
    // Spot check: every coarse-grained Action category appears at least
    // once in the palette universe.
    let all = all_actions();
    assert!(all.contains(&Action::NewTab));
    assert!(all.contains(&Action::OpenCommandPalette));
    assert!(all.contains(&Action::OpenSearch));
    assert!(all.contains(&Action::OpenPreferences));
    assert!(all.contains(&Action::Scroll(ScrollAction::ToTop)));
    assert!(all.contains(&Action::FocusPane(Direction::Left)));
}
