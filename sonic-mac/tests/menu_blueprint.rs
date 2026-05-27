//! Tests for the macOS native menubar blueprint + dispatch.
//!
//! These run on macOS only — the `menubar` module is `cfg(target_os = "macos")`.

#![cfg(target_os = "macos")]

use sonic_core::keymap::Action;
use sonic_mac::menubar::{blueprint, dispatch_tag, Binding, Item, KeyMods, MenuEntry};

/// The serialization needs to match: clicking the "New Tab" menu item
/// must end up queueing `Action::NewTab` on the menubar bridge.
#[test]
fn menu_dispatch_new_tab_calls_action_new_tab() {
    // Drain any pre-existing queue residue first.
    let _ = sonic_shared::menubar_bridge::__test_drain();

    // Re-register the New Tab item and grab its tag through the same
    // path the AppKit installer uses: walk the blueprint, find the
    // entry, and call `dispatch_tag`. We can't call `install()`
    // (needs main-thread AppKit), so we re-register manually.
    let entry = MenuEntry::Act(Action::NewTab);
    let tag = sonic_mac::menubar::__test_register(entry);

    let _ = dispatch_tag(tag);
    // Note: dispatch_tag returns the result of push_action, which is
    // `false` in tests because no EventLoopProxy is installed — but
    // the action IS queued. We assert on the queue directly.

    let drained = sonic_shared::menubar_bridge::__test_drain();
    assert_eq!(drained.len(), 1, "exactly one action should be queued");
    assert!(
        matches!(drained[0], Action::NewTab),
        "queued action should be NewTab, got {:?}",
        drained[0]
    );
}

/// Counts and key bindings for every item the spec documents. If the
/// blueprint diverges from the README/spec, this test catches it.
#[test]
fn menu_lists_all_documented_items() {
    let bp = blueprint();

    // 5 top-level submenus, in order.
    let titles: Vec<&str> = bp.iter().map(|s| s.title).collect();
    assert_eq!(titles, vec!["Sonic", "Shell", "Edit", "View", "Help"], "submenu order/titles");

    let find = |menu: &str, title: &str| -> Item {
        bp.iter()
            .find(|s| s.title == menu)
            .unwrap_or_else(|| panic!("missing submenu {menu}"))
            .items
            .iter()
            .find(|i| i.title == title)
            .cloned()
            .unwrap_or_else(|| panic!("missing item {menu} → {title}"))
    };

    // ---- Shell ----
    let new_tab = find("Shell", "New Tab");
    assert_eq!(new_tab.key, "t");
    assert_eq!(new_tab.mods, KeyMods::Cmd);
    assert!(matches!(new_tab.binding, Binding::Action(Action::NewTab)));

    let new_win = find("Shell", "New Window");
    assert!(matches!(new_win.binding, Binding::Action(Action::NewWindow)));
    assert_eq!(new_win.mods, KeyMods::Cmd);

    let split_r = find("Shell", "Split Right");
    assert!(matches!(split_r.binding, Binding::Action(Action::SplitRight)));
    assert_eq!(split_r.mods, KeyMods::Cmd);

    let split_d = find("Shell", "Split Down");
    assert!(matches!(split_d.binding, Binding::Action(Action::SplitDown)));
    assert_eq!(split_d.mods, KeyMods::CmdShift);

    let close_tab = find("Shell", "Close Tab");
    assert!(matches!(close_tab.binding, Binding::Action(Action::CloseTab)));
    assert_eq!(close_tab.mods, KeyMods::Cmd);

    let close_pane = find("Shell", "Close Pane");
    assert!(matches!(close_pane.binding, Binding::Action(Action::ClosePane)));
    assert_eq!(close_pane.mods, KeyMods::CmdShift);

    // ---- Edit ----
    assert!(matches!(find("Edit", "Copy").binding, Binding::Action(Action::CopyToClipboard)));
    assert!(matches!(find("Edit", "Paste").binding, Binding::Action(Action::PasteFromClipboard)));
    assert!(matches!(find("Edit", "Find…").binding, Binding::Action(Action::OpenSearch)));
    assert!(matches!(
        find("Edit", "Command Palette").binding,
        Binding::Action(Action::OpenCommandPalette)
    ));

    // ---- View ----
    assert!(matches!(
        find("View", "Toggle Tab Bar").binding,
        Binding::Action(Action::ToggleTabBar)
    ));
    assert!(matches!(find("View", "Reset Zoom").binding, Binding::Action(Action::ResetFontSize)));

    // ---- Help ----
    let help_url = find("Help", "Sonic Help");
    match &help_url.binding {
        Binding::Url(u) => assert_eq!(*u, "https://github.com/D0n9X1n/sonic"),
        other => panic!("expected URL binding, got {other:?}"),
    }
    let issue_url = find("Help", "Report Issue");
    match &issue_url.binding {
        Binding::Url(u) => assert_eq!(*u, "https://github.com/D0n9X1n/sonic/issues/new"),
        other => panic!("expected URL binding, got {other:?}"),
    }

    // ---- Sonic (existing menu preserved) ----
    assert!(bp
        .iter()
        .find(|s| s.title == "Sonic")
        .unwrap()
        .items
        .iter()
        .any(|i| i.title == "About Sonic"));
    assert!(bp
        .iter()
        .find(|s| s.title == "Sonic")
        .unwrap()
        .items
        .iter()
        .any(|i| i.title == "Preferences…"));
    assert!(bp
        .iter()
        .find(|s| s.title == "Sonic")
        .unwrap()
        .items
        .iter()
        .any(|i| i.title == "Quit Sonic"));
}
