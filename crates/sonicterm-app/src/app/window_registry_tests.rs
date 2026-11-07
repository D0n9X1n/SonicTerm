//! Pins the window registry's invariants across seeded sequences of real lifecycle operations
//! (child windows, tabs, splits, transfers between windows, pane and tab closes, resizes): every
//! pane is owned by exactly one window and reachable from exactly one tab tree, the main window id
//! names a live window, every live window has a key and keys are unique, and the MCP list and
//! one-pane lookup agree. These hold on the current layout so the owner move is checked by them.

use std::collections::{HashMap, HashSet};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

use crate::app::App;

/// Every registry invariant, checked after each operation; `context` names the step.
fn check(app: &App, context: &str) {
    let main = app.registry.main_window_id.expect("a main window");
    assert!(app.registry.windows.contains_key(&main), "{context}: the main id names a live window");
    let mut owner: HashMap<u64, winit::window::WindowId> = HashMap::new();
    let mut keys = HashSet::new();
    for (id, window) in &app.registry.windows {
        let key = app.window_key(*id).expect("every live window has a key");
        assert!(keys.insert(key), "{context}: window keys are unique");
        for pane_id in window.panes.keys() {
            let previous = owner.insert(*pane_id, *id);
            assert!(previous.is_none(), "{context}: pane {pane_id} is owned by two windows");
        }
        let mut in_trees = HashSet::new();
        for tab in &window.tab_states {
            tab.tree.for_each_leaf(&mut |leaf| {
                assert!(in_trees.insert(leaf), "{context}: pane {leaf} sits in two tab trees");
            });
            assert!(
                tab.tree.contains_pane(tab.active_pane),
                "{context}: a tab's active pane is its own"
            );
        }
        for pane_id in window.panes.keys() {
            assert!(
                in_trees.contains(pane_id),
                "{context}: pane {pane_id} is in no tab of its window"
            );
        }
        assert_eq!(
            window.tabs.len(),
            window.tab_states.len(),
            "{context}: tabs and tab states agree"
        );
    }
    for entry in app.mcp_pane_entries() {
        let resolved = app.mcp_resolve_pane(entry.pane_id).expect("a listed pane resolves");
        assert_eq!(
            (resolved.window_index, resolved.tab_index),
            (entry.window_index, entry.tab_index),
            "{context}: list and lookup agree for pane {}",
            entry.pane_id
        );
    }
}

#[test]
fn registry_invariants_hold_across_lifecycle_sequences() {
    let mut seed: u64 = 0x1800_0004;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound.max(1)
    };
    for round in 0..12 {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        check(&app, "seeded");
        for step in 0..25 {
            let children: Vec<_> = app
                .registry
                .windows
                .keys()
                .copied()
                .filter(|id| Some(*id) != app.registry.main_window_id)
                .collect();
            let context = format!("round {round} step {step}");
            match next(7) {
                0 => {
                    app.__test_seed_tab("tab");
                }
                1 => {
                    app.__test_seed_child_window(&["c1", "c2"]);
                }
                2 => app.__test_split_active_right(),
                3 if !children.is_empty() => {
                    let child = children[next(children.len() as u64) as usize];
                    app.__test_child_split_active_right(child);
                }
                4 if !children.is_empty() => {
                    // A tab moves from a child to main, or from main to a child, as a drop does.
                    let child = children[next(children.len() as u64) as usize];
                    let (source, target) =
                        if next(2) == 0 { (Some(child), None) } else { (None, Some(child)) };
                    let tabs = |id: Option<winit::window::WindowId>, app: &App| {
                        id.or(app.registry.main_window_id)
                            .and_then(|id| app.registry.windows.get(&id))
                            .map_or(0, |window| window.tabs.len())
                    };
                    if tabs(source, &app) > 1 {
                        let _ = app.transfer_tab(
                            source,
                            next(tabs(source, &app) as u64) as usize,
                            target,
                            0,
                        );
                    }
                }
                5 if !children.is_empty() => {
                    let child = children[next(children.len() as u64) as usize];
                    app.__test_invoke_close_active_pane_or_tab_in_child(child);
                }
                _ => app.__test_resize_visible_panes(),
            }
            if app
                .registry
                .main_window_id
                .is_some_and(|main| app.registry.windows.contains_key(&main))
            {
                check(&app, &context);
            }
        }
    }
}
