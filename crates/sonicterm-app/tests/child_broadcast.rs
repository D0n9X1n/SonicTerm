//! Regression tests for broadcast input across torn-out child windows.

use std::collections::BTreeSet;

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, BroadcastScope, Keymap},
    theme::Theme,
};

fn test_app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

fn toggle_broadcast(scope: BroadcastScope) -> Action {
    Action::ToggleBroadcast { scope }
}

#[test]
fn child_window_can_be_broadcast_source_without_frontmost_cache() {
    let mut app = test_app();
    let main = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_child_split_active_right(child), "child split should succeed");
    let source = app.__test_child_active_pane(child).expect("child active pane");
    let child_panes = app.__test_child_pane_ids(child).expect("child panes");
    let sibling = child_panes.iter().copied().find(|id| *id != source).expect("split sibling");

    assert!(app.run_action_for_window(&toggle_broadcast(BroadcastScope::Tab), child));

    assert_eq!(app.__test_broadcast_source(), Some(source));
    assert_eq!(app.__test_broadcast_receivers(), BTreeSet::from([sibling]));
    assert!(
        !app.__test_broadcast_receivers().contains(&main),
        "tab-scoped child broadcast must not leak to the main window"
    );
}

#[test]
fn all_tabs_broadcast_receivers_span_main_and_child_windows() {
    let mut app = test_app();
    let main_a = app.__test_seed_tab("main-a");
    let main_b = app.__test_seed_tab("main-b");
    let child_a = app.__test_seed_child_window(&["child-a", "child-a-2"]);
    let child_b = app.__test_seed_child_window(&["child-b"]);
    let source = app.__test_child_active_pane(child_a).expect("child source pane");
    let child_a_panes = app.__test_child_pane_ids(child_a).expect("child-a panes");
    let child_b_pane = app.__test_child_active_pane(child_b).expect("child-b pane");

    assert!(app.run_action_for_window(&toggle_broadcast(BroadcastScope::AllTabs), child_a));

    let receivers = app.__test_broadcast_receivers();
    assert!(!receivers.contains(&source), "source pane never receives its own broadcast");
    assert!(receivers.contains(&main_a), "main active tab pane should receive all-tabs broadcast");
    assert!(
        receivers.contains(&main_b),
        "main inactive tab pane should receive all-tabs broadcast"
    );
    assert!(
        receivers.contains(&child_b_pane),
        "sibling child window should receive all-tabs broadcast"
    );
    for pane in child_a_panes.into_iter().filter(|id| *id != source) {
        assert!(receivers.contains(&pane), "other tabs in the source child should receive");
    }
}

#[test]
fn child_source_write_fans_out_to_receivers_in_every_window() {
    let mut app = test_app();
    let main = app.__test_seed_tab("main");
    let child_a = app.__test_seed_child_window(&["child-a", "child-a-2"]);
    let child_b = app.__test_seed_child_window(&["child-b"]);
    let source = app.__test_child_active_pane(child_a).expect("child source pane");
    let child_b_pane = app.__test_child_active_pane(child_b).expect("child-b pane");
    app.__test_enable_pty_write_log();

    assert!(app.run_action_for_window(&toggle_broadcast(BroadcastScope::AllTabs), child_a));
    app.__test_write_to_pane_with_broadcast(source, b"ping".to_vec());

    let writes = app.__test_pty_write_log();
    let written_panes: BTreeSet<u64> = writes.iter().map(|(pane, _)| *pane).collect();
    assert!(written_panes.contains(&source), "source pane write should be preserved");
    assert!(written_panes.contains(&main), "main pane should receive child-origin broadcast");
    assert!(
        written_panes.contains(&child_b_pane),
        "other child windows should receive child-origin broadcast"
    );
    assert_eq!(writes.iter().filter(|(_, bytes)| bytes == b"ping").count(), writes.len());
}

#[test]
fn child_render_flags_mark_broadcast_receivers() {
    let mut app = test_app();
    let source = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_child_split_active_right(child), "child split should succeed");
    let child_panes = app.__test_child_pane_ids(child).expect("child panes");

    assert!(app.run_action(&toggle_broadcast(BroadcastScope::AllTabs)));
    assert_eq!(app.__test_broadcast_source(), Some(source));

    let flags = app.__test_child_broadcast_render_flags(child).expect("child render flags");
    assert_eq!(flags.len(), child_panes.len());
    for pane in child_panes {
        assert!(
            flags.iter().any(|(id, is_receiver)| *id == pane && *is_receiver),
            "child pane {pane} should be passed to the renderer as a broadcast receiver"
        );
    }
}
