//! Guard tests for the native menu blueprint.
//!
//! These pin two user-facing decisions that are easy to silently regress:
//!   1. The macOS Quit item must map to `QuitApp` and carry NO ⌘Q key
//!      equivalent. ⌘Q is hold-to-quit, handled on winit's keyboard path so
//!      the hold can be measured; if the NSMenu item owned a ⌘Q key
//!      equivalent, AppKit would consume the chord and quit immediately,
//!      defeating the guard. No blueprint item may bind ⌘Q at all.
//!   2. The blueprint's top-level submenu set + ordering (the Window menu is
//!      injected by the macOS layer via `setWindowsMenu:`, so it must NOT
//!      appear in the shared blueprint, or AppKit would get two).

use sonicterm_app::menu::{blueprint, Binding, Item, KeyMods};
use sonicterm_cfg::keymap::Action;

fn find_item<'a>(title: &str) -> Option<Item> {
    blueprint().iter().flat_map(|sm| sm.items.iter()).find(|it| it.title == title).cloned()
}

#[test]
fn quit_item_maps_to_quit_app_without_cmd_q_key_equivalent() {
    let quit =
        find_item("Quit SonicTerm").expect("'Quit SonicTerm' item must exist in the blueprint");
    assert_eq!(
        quit.key, "",
        "Quit must carry no key equivalent — ⌘Q is hold-to-quit on the keymap"
    );
    assert_eq!(quit.mods, KeyMods::None, "Quit must not register a modifier chord");
    match quit.binding {
        Binding::Action(Action::QuitApp) => {}
        other => panic!("Quit must map to Action::QuitApp, got {other:?}"),
    }
    // No menu item anywhere may bind ⌘Q: the chord is owned by the keymap so
    // the hold-to-quit guard can measure the press. An NSMenu key equivalent
    // (whether Action or terminate:) would let AppKit consume it first.
    let binds_cmd_q = blueprint()
        .iter()
        .flat_map(|sm| sm.items.iter())
        .any(|it| it.key == "q" && it.mods == KeyMods::Cmd);
    assert!(
        !binds_cmd_q,
        "no menu item may bind ⌘Q — it is handled by the hold-to-quit keymap path"
    );
}

#[test]
fn blueprint_has_expected_top_level_menus_without_window() {
    let titles: Vec<&str> = blueprint().iter().map(|sm| sm.title).collect();
    assert_eq!(
        titles,
        vec!["SonicTerm", "Shell", "Edit", "View", "Help"],
        "blueprint submenu set/order changed"
    );
    // The Window menu is added by the macOS layer (setWindowsMenu:), so it
    // must NOT be in the shared blueprint — otherwise AppKit would receive
    // two Window menus.
    assert!(
        !titles.contains(&"Window"),
        "Window menu must be injected by the mac layer, not the shared blueprint"
    );
}

#[test]
fn close_action_is_on_cmd_w() {
    // Cmd+W ("Close") closes the active pane/tab. This is now the sole
    // close-current chord (⌘Q became app-quit), so pin it explicitly.
    let close_w = find_item("Close").expect("'Close' (Cmd+W) item exists");
    assert_eq!(close_w.key, "w");
    assert_eq!(close_w.mods, KeyMods::Cmd);
    assert!(matches!(close_w.binding, Binding::Action(Action::CloseActivePaneOrTab)));
}
