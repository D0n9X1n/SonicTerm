//! Integration tests for the sonicterm-core keymap re-exports.

use sonicterm_core::keymap::*;

#[test]
fn parses_bundled_wezterm_map() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/keymaps/wezterm.toml");
    let km = Keymap::load(&path).expect("load");
    assert_eq!(km.meta.name, "wezterm-default");
    assert!(matches!(km.lookup("super+t"), Some(Action::NewTab)));
    assert!(matches!(km.lookup("super+1"), Some(Action::ActivateTab(0))));
    assert!(matches!(km.lookup("super+shift+h"), Some(Action::FocusPane(Direction::Left))));
}
