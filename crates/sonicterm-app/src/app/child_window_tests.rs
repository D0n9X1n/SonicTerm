use super::*;
use sonicterm_ui::pane::Rect;

#[test]
fn child_split_after_zoom_keeps_active_parser_visible_in_every_direction() {
    // Both real split directions must unzoom the child without changing the main window's topology.
    for nested in [false, true] {
        for direction in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            let main_pane = app.__test_seed_tab("main");
            let child_id = app.__test_seed_child_window(&["child"]);
            let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
            assert!(app.__test_set_child_pane_viewport(child_id, outer, 10.0, 10.0));
            resize_visible_panes_in_child(app.windows.get_mut(&child_id).expect("child window"));
            if nested {
                assert!(app.split_active_pane_in_child(child_id, Direction::Right));
            }
            let child = app.windows.get(&child_id).expect("child window");
            let tab = &child.tab_states[child.tabs.active_index()];
            let previous_active = tab.active_pane;
            let mut expected = tab.tree.clone();
            assert!(app.toggle_active_pane_zoom_in_child(child_id));
            assert_eq!(
                App::compute_pane_rects_for(app.windows.get(&child_id).expect("child window")),
                [(previous_active, outer)],
            );
            for pane in app.windows.get(&child_id).expect("child window").panes.values() {
                pane.parser.lock().grid_mut().clear_dirty();
            }

            assert!(app.split_active_pane_in_child(child_id, direction));

            let child = app.windows.get(&child_id).expect("child window");
            let tab = &child.tab_states[child.tabs.active_index()];
            let active = tab.active_pane;
            assert_ne!(active, previous_active);
            assert!(expected.split(previous_active, direction, active));
            let rects = App::compute_pane_rects_for(child);
            assert_eq!(rects, expected.layout(outer), "nested={nested}, {direction:?}");
            assert_eq!(tab.tree.zoomed_pane_id(), None);
            assert_eq!(rects.iter().filter(|(id, _)| *id == active).count(), 1);
            let guards: Vec<_> = rects
                .iter()
                .map(|(id, rect)| {
                    let pane = child.panes.get(id).expect("visible pane is live");
                    (*id, pane.parser.try_lock().expect("coherent parser guard"), *rect)
                })
                .collect();
            assert!(guards.iter().any(|(id, _, _)| *id == active));
            for (id, parser, rect) in &guards {
                let grid = parser.grid();
                assert_eq!(
                    (grid.cols, grid.rows),
                    ((rect.w / 10.0) as u16, (rect.h / 10.0) as u16)
                );
                if *id == active || *id == previous_active {
                    assert!(grid.dirty_rows().count() > 0, "split participants must redraw");
                }
            }
            let main = app.main().expect("main window");
            assert_eq!(main.tab_states[0].tree.leaves(), [main_pane]);
            assert_eq!(main.tab_states[0].active_pane, main_pane);
            assert_eq!(main.panes.len(), 1);
            assert_eq!(main.tab_states[0].tree.zoomed_pane_id(), None);
        }
    }
}

#[test]
fn child_split_refusal_preserves_zoom_focus_and_live_panes() {
    // A missing live pane and a non-leaf focus both refuse without mutating the child's zoomed tree.
    for missing_live_pane in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let child_id = app.__test_seed_child_window(&["child"]);
        let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
        assert!(app.__test_set_child_pane_viewport(child_id, outer, 10.0, 10.0));
        assert!(app.toggle_active_pane_zoom_in_child(child_id));
        let child = app.windows.get_mut(&child_id).expect("child window");
        let pane = child.tab_states[0].active_pane;
        if missing_live_pane {
            child.panes.remove(&pane);
        } else {
            child.tab_states[0].active_pane = u64::MAX;
        }
        let active = child.tab_states[0].active_pane;
        let mut panes_before: Vec<_> = child.panes.keys().copied().collect();
        panes_before.sort_unstable();
        let layout_before = child.tab_states[0].tree.layout(outer);

        assert!(!app.split_active_pane_in_child(child_id, Direction::Down));

        let child = app.windows.get(&child_id).expect("child window");
        let tab = &child.tab_states[0];
        let mut panes_after: Vec<_> = child.panes.keys().copied().collect();
        panes_after.sort_unstable();
        assert_eq!(panes_after, panes_before);
        assert_eq!(tab.active_pane, active);
        assert_eq!(tab.tree.leaves(), [pane]);
        assert_eq!(tab.tree.zoomed_pane_id(), Some(pane));
        assert_eq!(tab.tree.layout(outer), layout_before);
    }
}

#[test]
fn refused_child_split_actions_never_fall_through_to_main() {
    // A live child consumes a refused split in both dispatch routes without changing either window.
    for explicit_window in [false, true] {
        for action in [Action::SplitRight, Action::SplitDown] {
            for missing_live_pane in [false, true] {
                let shell =
                    std::env::current_exe().expect("test executable").join("unavailable-shell");
                let config = Config {
                    terminal: sonicterm_cfg::config::TerminalConfig {
                        shell: Some(shell.to_string_lossy().into_owned()),
                        ..Default::default()
                    },
                    ..Config::default()
                };
                let mut app = App::new(Theme::default(), config, Keymap::default());
                let main_pane = app.__test_seed_tab("main");
                let child_id = app.__test_seed_child_window(&["child"]);
                assert!(app.toggle_active_pane_zoom_in_child(child_id));
                let child = app.windows.get_mut(&child_id).expect("child window");
                let original = child.tab_states[0].active_pane;
                if missing_live_pane {
                    child.panes.remove(&original);
                } else {
                    child.tab_states[0].active_pane = u64::MAX;
                }
                let child_active = child.tab_states[0].active_pane;
                let pane_count = child.panes.len();
                app.frontmost_window = Some(child_id);

                if explicit_window {
                    assert!(app.run_action_for_window(&action, child_id));
                } else {
                    assert!(app.run_action(&action));
                }

                let main = app.main().expect("main window");
                assert_eq!(main.tab_states[0].tree.leaves(), [main_pane]);
                assert_eq!(main.tab_states[0].active_pane, main_pane);
                assert_eq!(main.panes.len(), 1);
                assert_eq!(app.frontmost_window, Some(child_id));
                let child = app.windows.get(&child_id).expect("child window");
                assert_eq!(child.tab_states[0].tree.leaves(), [original]);
                assert_eq!(child.tab_states[0].tree.zoomed_pane_id(), Some(original));
                assert_eq!(child.tab_states[0].active_pane, child_active);
                assert_eq!(child.panes.len(), pane_count);
            }
        }
    }
}

#[test]
fn child_split_without_window_or_tab_leaves_topology_empty() {
    // Child mutations must refuse missing destinations without installing unowned panes.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child_id = app.__test_seed_child_window(&[]);
    assert!(!app.split_active_pane_in_child(child_id, Direction::Right));
    let child = app.windows.get(&child_id).expect("empty child window");
    assert!(child.tab_states.is_empty());
    assert!(child.panes.is_empty());

    app.windows.remove(&child_id);
    assert!(!app.split_active_pane_in_child(child_id, Direction::Right));
    assert!(!app.windows.contains_key(&child_id));
}
