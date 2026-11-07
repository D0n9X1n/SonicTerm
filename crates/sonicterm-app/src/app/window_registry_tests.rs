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
        assert!(app.mcp_resolve_pane(entry.pane_id).is_some(), "{context}: a listed pane resolves");
        assert_eq!(
            app.mcp_pane_metadata(entry.pane_id),
            entry.metadata,
            "{context}: list and lookup agree for pane {}",
            entry.pane_id
        );
    }
}

#[test]
fn registry_invariants_hold_across_lifecycle_sequences() {
    use sonicterm_ui::pane::Rect;
    let mut seed: u64 = 0x1800_0004;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound.max(1)
    };
    // How many of each operation actually ran, so a sequence that refused every transfer or resize
    // cannot pass on the setup alone.
    let (mut transfers, mut resizes) = (0_usize, 0_usize);
    let geometry = |width_px: f32| Rect::new(0.0, 0.0, width_px, 240.0);
    for round in 0..12 {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        // Every window gets explicit test geometry, so transfers have a destination size and a
        // resize really changes the panes' grids, as a renderer would provide.
        assert!(app.__test_set_main_pane_viewport(geometry(800.0), 10.0, 10.0));
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
                    let child = app.__test_seed_child_window(&["c1", "c2"]);
                    assert!(app.__test_set_child_pane_viewport(child, geometry(800.0), 10.0, 10.0));
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
                    let source_tabs = tabs(source, &app);
                    if source_tabs > 1 {
                        let target_tabs = tabs(target, &app);
                        let index = next(source_tabs as u64) as usize;
                        app.transfer_tab(source, index, target, 0).unwrap_or_else(|error| {
                            panic!("{context}: transfer refused: {error:?}")
                        });
                        assert_eq!(
                            tabs(source, &app),
                            source_tabs - 1,
                            "{context}: source lost it"
                        );
                        assert_eq!(tabs(target, &app), target_tabs + 1, "{context}: target got it");
                        transfers += 1;
                    }
                }
                5 if !children.is_empty() => {
                    let child = children[next(children.len() as u64) as usize];
                    app.__test_invoke_close_active_pane_or_tab_in_child(child);
                }
                _ => {
                    // Alternate the main viewport's width, so every resize changes the visible grids.
                    let current = app.test_viewport_override.map_or(800.0, |(outer, _, _)| outer.w);
                    let width_px = if current == 800.0 { 600.0 } else { 800.0 };
                    let before = app.compute_active_pane_rects();
                    assert!(app.__test_set_main_pane_viewport(geometry(width_px), 10.0, 10.0));
                    app.__test_resize_visible_panes();
                    // Each visible pane's grid now fits its own layout rect in the new viewport.
                    let rects = app.compute_active_pane_rects();
                    assert_ne!(rects, before, "{context}: the layout changed with the viewport");
                    for (pane, rect) in rects {
                        let cols = app.__test_pane_grid_size(pane).expect("a main pane").0;
                        assert_eq!(
                            u32::from(cols),
                            (rect.w / 10.0).floor() as u32,
                            "{context}: pane {pane}'s grid follows the resize"
                        );
                    }
                    resizes += 1;
                }
            }
            // The main window survives every operation here, so the invariants are always checked.
            check(&app, &context);
        }
    }
    assert!(transfers >= 10, "only {transfers} transfers ran");
    assert!(resizes >= 10, "only {resizes} resizes ran");
}
