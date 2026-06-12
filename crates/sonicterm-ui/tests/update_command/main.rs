use sonicterm_cfg::keymap::Action;
use sonicterm_ui::command_label::{label, search_haystack, variant_kind};
use sonicterm_ui::command_palette::palette_actions;

#[test]
fn check_for_updates_is_a_command_palette_action() {
    assert!(palette_actions().iter().any(|action| matches!(action, Action::CheckForUpdates)));
    assert_eq!(variant_kind(&Action::CheckForUpdates), "CheckForUpdates");
    assert_eq!(label(&Action::CheckForUpdates), "Check for Updates");
    assert!(search_haystack(&Action::CheckForUpdates).contains("release"));
}
