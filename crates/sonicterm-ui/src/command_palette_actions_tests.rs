//! Canonical palette actions retain discoverability and default availability.

use super::*;
use crate::i18n::test_translator as translator;
use PaletteEntry::Command;

/// The default catalog excludes parameterized placeholder commands that cannot execute meaningfully.
#[test]
fn palette_defaults_do_not_expose_placeholder_parameter_actions() {
    let actions = palette_actions();
    assert!(!actions.iter().any(|action| matches!(action, Action::ApplyTheme(_))));
    assert!(actions.iter().any(|action| matches!(action, Action::OpenCommandPalette)));
    assert!(actions.iter().any(|action| matches!(action, Action::UpdateTabColor)));
    assert!(actions.iter().any(|action| matches!(action, Action::MoveTabToNewWindow)));
    assert!(actions.iter().any(|action| {
        matches!(action, Action::ResizePane { dir: Direction::Left, amount: 5 })
    }));
    assert!(covers_every_variant_kind());
}

/// Tear-out remains discoverable without inventing a default shortcut.
#[test]
fn move_tab_to_new_window_is_searchable_and_unbound_by_default() {
    let mut palette = CommandPalette::new();
    palette.set_keymap(&Keymap::default(), &translator("en"));
    palette.set_query("detach");

    let visible = palette.visible();
    let index = visible
        .iter()
        .position(|action| matches!(action, Command(Action::MoveTabToNewWindow)))
        .expect("detach should find Move Tab to New Window");
    assert_eq!(palette.label_for_visible_index(index), Some("Move Tab to New Window"));
    assert_eq!(palette.shortcut_hint_for_visible_index(index), None);
}

/// The weight commands exist so `weight_scale` is reachable without editing
/// `sonicterm.toml`. Searching the words a user would actually type has to
/// surface them, or the feature is invisible.
#[test]
fn font_weight_commands_are_searchable_by_bolder_and_thinner() {
    for (query, expected, label) in [
        ("bolder", Action::IncreaseFontWeight, "Increase Font Weight (Bolder)"),
        ("thinner", Action::DecreaseFontWeight, "Decrease Font Weight (Thinner)"),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_keymap(&Keymap::default(), &translator("en"));
        palette.set_query(query);

        let visible = palette.visible();
        let index = visible
            .iter()
            .position(|action| **action == Command(expected.clone()))
            .unwrap_or_else(|| panic!("{query:?} should find {label}"));
        assert_eq!(palette.label_for_visible_index(index), Some(label));
    }
}

/// Alternate wordings people reach for, so the commands are not findable only
/// by their exact label.
#[test]
fn font_weight_commands_are_searchable_by_synonyms() {
    for (query, expected) in [
        ("heavier", Action::IncreaseFontWeight),
        ("thicker", Action::IncreaseFontWeight),
        ("bolder font", Action::IncreaseFontWeight),
        ("bold font", Action::IncreaseFontWeight),
        ("lighter", Action::DecreaseFontWeight),
        ("slimmer", Action::DecreaseFontWeight),
        ("thinner font", Action::DecreaseFontWeight),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_keymap(&Keymap::default(), &translator("en"));
        palette.set_query(query);
        assert!(
            palette.visible().iter().any(|action| **action == Command(expected.clone())),
            "{query:?} should surface {expected:?}"
        );
    }
}

/// Reset font weight remains a discoverable canonical action.
#[test]
fn reset_font_weight_is_in_the_palette() {
    let mut palette = CommandPalette::new();
    palette.set_keymap(&Keymap::default(), &translator("en"));
    palette.set_query("reset font weight");
    let visible = palette.visible();
    let index = visible
        .iter()
        .position(|action| matches!(action, Command(Action::ResetFontWeight)))
        .expect("reset font weight should be searchable");
    assert_eq!(palette.label_for_visible_index(index), Some("Reset Font Weight to Config"));
}
