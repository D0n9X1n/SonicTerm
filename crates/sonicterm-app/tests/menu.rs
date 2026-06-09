//! Guard tests for the native menu blueprint.
//!
//! These pin two user-facing decisions that are easy to silently regress:
//!   1. Cmd+Q must close the active pane/tab (NOT terminate the whole app).
//!      The macOS convention is Cmd+Q = quit; SonicTerm deliberately remaps
//!      it to `CloseActivePaneOrTab` so power users don't lose every tab on
//!      a fat-finger. A refactor that restores `System("terminate:")` here
//!      would be a real behavior regression.
//!   2. The blueprint's top-level submenu set + ordering (the Window menu is
//!      injected by the macOS layer via `setWindowsMenu:`, so it must NOT
//!      appear in the shared blueprint, or AppKit would get two).

use sonicterm_app::menu::{blueprint, Binding, Item, KeyMods};
use sonicterm_cfg::keymap::Action;

fn find_item<'a>(title: &str) -> Option<Item> {
    blueprint()
        .iter()
        .flat_map(|sm| sm.items.iter())
        .find(|it| it.title == title)
        .cloned()
}

#[test]
fn cmd_q_closes_tab_not_terminates_app() {
    let close = find_item("Close Tab").expect("'Close Tab' item must exist in the blueprint");
    assert_eq!(close.key, "q", "Close Tab must be bound to the 'q' key");
    assert_eq!(close.mods, KeyMods::Cmd, "Close Tab must use the Cmd modifier");
    match close.binding {
        Binding::Action(Action::CloseActivePaneOrTab) => {}
        other => panic!(
            "Cmd+Q must map to CloseActivePaneOrTab (close current tab), got {other:?} — \
             a regression to terminate: would quit the whole app"
        ),
    }
    // Belt-and-suspenders: no menu item anywhere may bind Cmd+Q to a
    // terminate selector.
    let terminates_on_q = blueprint().iter().flat_map(|sm| sm.items.iter()).any(|it| {
        it.key == "q"
            && it.mods == KeyMods::Cmd
            && matches!(&it.binding, Binding::System(s) if s.contains("terminate"))
    });
    assert!(!terminates_on_q, "no Cmd+Q item may bind to terminate: — that would quit the app");
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
fn close_actions_are_consistent() {
    // Both Cmd+W ("Close") and Cmd+Q ("Close Tab") route through the same
    // close action so the two shortcuts behave identically.
    let close_w = find_item("Close").expect("'Close' (Cmd+W) item exists");
    assert_eq!(close_w.key, "w");
    assert!(matches!(close_w.binding, Binding::Action(Action::CloseActivePaneOrTab)));
}
