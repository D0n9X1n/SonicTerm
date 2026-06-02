//! Shared blueprint shape parity — runs on every platform.
//!
//! Lives in `sonicterm-shared/tests/` so that any platform crate
//! (sonicterm-mac, sonicterm-windows) implementing [`PlatformMenu`] can rely
//! on the same canonical structure being present at compile/link time.

use sonicterm_app::menu::{blueprint, Binding, KeyMods, Sender};
use sonicterm_cfg::keymap::Action;

#[test]
fn blueprint_has_five_top_level_submenus_in_order() {
    let bp = blueprint();
    let titles: Vec<&str> = bp.iter().map(|s| s.title).collect();
    assert_eq!(titles, vec!["SonicTerm", "Shell", "Edit", "View", "Help"]);
}

#[test]
fn blueprint_action_bindings_round_trip_through_sender() {
    let _ = sonicterm_app::menubar_bridge::__test_drain();
    // Find the New Tab item and confirm its binding decodes the way
    // platform code will read it.
    let bp = blueprint();
    let shell = bp.iter().find(|s| s.title == "Shell").expect("Shell submenu");
    let new_tab = shell.items.iter().find(|i| i.title == "New Tab").expect("New Tab item");
    assert_eq!(new_tab.key, "t");
    assert_eq!(new_tab.mods, KeyMods::Cmd);
    let action = match &new_tab.binding {
        Binding::Action(a) => a.clone(),
        other => panic!("expected Action binding, got {other:?}"),
    };
    assert!(matches!(action, Action::NewTab));

    // Sender::push must not panic when no event loop is running (the
    // headless test environment). Return value is irrelevant here.
    let _ = Sender::new().push(action);
}

#[test]
fn blueprint_help_items_are_https_urls() {
    let bp = blueprint();
    let help = bp.iter().find(|s| s.title == "Help").expect("Help submenu");
    for it in &help.items {
        match &it.binding {
            Binding::Url(u) => assert!(
                u.starts_with("https://"),
                "Help URL must be https for OSC 8 / NSWorkspace safety: {u}"
            ),
            other => panic!("Help item {:?} not a URL binding: {other:?}", it.title),
        }
    }
}

#[test]
fn blueprint_separators_have_empty_titles() {
    let bp = blueprint();
    for sm in &bp {
        for it in &sm.items {
            if matches!(it.binding, Binding::Separator) {
                assert_eq!(it.title, "", "separator with non-empty title in {}", sm.title);
                assert_eq!(it.key, "");
                assert_eq!(it.mods, KeyMods::None);
            }
        }
    }
}
