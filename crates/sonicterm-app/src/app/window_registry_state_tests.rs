//! Pins the registry's own pane lookups: a pane is found, mutably and by its window, in whichever
//! window owns it, main or child, and an unknown id finds nothing.

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

use crate::app::App;

/// Each lookup answers for a main pane and a child pane, and agrees with the window's own map.
#[test]
fn the_registry_finds_each_pane_in_its_owning_window() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.registry.windows[&child].tab_states[0].active_pane;
    let main = app.registry.main_window_id.unwrap();
    for (pane, window) in [(main_pane, main), (child_pane, child)] {
        assert_eq!(app.registry.window_of_pane(pane), Some(window));
        let found = app.registry.pane(pane).expect("an owned pane") as *const _;
        assert_eq!(found, &app.registry.windows[&window].panes[&pane] as *const _);
        app.registry.pane_mut(pane).expect("an owned pane").creator =
            crate::app::mcp::agents::PaneOwner::agent_if(true);
        assert!(app.registry.windows[&window].panes[&pane].creator.is_agent());
    }
    assert!(app.registry.pane(u64::MAX).is_none());
    assert!(app.registry.window_of_pane(u64::MAX).is_none());
}

/// A window holding a pane another window already owns is refused in debug builds: one pane, one
/// window. `duplicate_pane` names it in every build.
#[test]
fn inserting_a_window_that_shares_a_pane_is_caught() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert_eq!(app.registry.duplicate_pane(), None);
    // Move a copy of the main pane's entry into the child, as a transfer that forgot to remove
    // it from its source would.
    let mut removed = app.registry.remove_window(child).expect("the child window");
    let shared = app.__test_seed_tab("shared");
    let main = app.registry.main_window_id.unwrap();
    let pane = app.registry.windows.get_mut(&main).unwrap().panes.remove(&shared).unwrap();
    removed.panes.insert(main_pane, pane);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.registry.insert_window(child, removed);
    }));
    assert_eq!(app.registry.duplicate_pane(), Some(main_pane));
    if cfg!(debug_assertions) {
        assert!(outcome.is_err(), "debug builds refuse a pane two windows own");
    }
    // Drop the copy so the App's teardown sees each pane once.
    std::mem::forget(app.registry.windows.get_mut(&child).unwrap().panes.remove(&main_pane));
}

/// `remove_window` hands back the window it dropped, panes included, and the map no longer has it.
#[test]
fn removing_a_window_returns_its_state() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["one", "two"]);
    let pane_ids: Vec<u64> = app.registry.windows[&child].panes.keys().copied().collect();
    let removed = app.registry.remove_window(child).expect("the child window");
    assert!(!app.registry.windows.contains_key(&child));
    assert_eq!(removed.panes.len(), pane_ids.len());
    assert!(pane_ids.iter().all(|pane| app.registry.window_of_pane(*pane).is_none()));
    assert!(app.registry.remove_window(child).is_none(), "a second removal finds nothing");
}

/// `locate_pane` agrees with a brute-force search of every window and tab after tabs move between
/// windows, so callers can rely on it instead of walking the map themselves.
#[test]
fn locate_pane_matches_a_brute_force_search_across_transfers() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main-0");
    app.__test_seed_tab("main-1");
    let left = app.__test_seed_child_window(&["l0", "l1", "l2"]);
    let right = app.__test_seed_child_window(&["r0"]);
    let main = app.registry.main_window_id.unwrap();
    let windows = [main, left, right];
    // Test windows have no renderer; a pane viewport gives `transfer_tab` the geometry it needs.
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 500.0);
    assert!(app.__test_set_main_pane_viewport(outer, 10.0, 20.0));
    for child in [left, right] {
        assert!(app.__test_set_child_pane_viewport(child, outer, 10.0, 20.0));
    }
    // A fixed sequence of moves: each takes the first tab of one window to the end of another,
    // skipping a source that would be left without tabs.
    for step in 0..12_usize {
        let source = windows[step % 3];
        let target = windows[(step * 7 + 1) % 3];
        if source == target || app.registry.windows[&source].tab_states.len() < 2 {
            continue;
        }
        let target_len = app.registry.windows[&target].tab_states.len();
        app.transfer_tab(Some(source), 0, Some(target), target_len).expect("the move");
        assert_eq!(app.registry.duplicate_pane(), None, "step {step}");
        for (window_id, window) in &app.registry.windows {
            for (tab_index, tab) in window.tab_states.iter().enumerate() {
                for pane in tab.tree.leaves() {
                    let slot = app.registry.locate_pane(pane).expect("a live pane");
                    assert_eq!((slot.window_id, slot.tab_index), (*window_id, tab_index));
                }
            }
        }
    }
    assert!(app.registry.locate_pane(u64::MAX).is_none());
}

/// The window map exposes no way to add or drop a window: only the registry's checked
/// `insert_window`/`remove_window` reach the private `insert`/`remove`.
#[test]
fn the_window_map_keeps_insert_and_remove_private() {
    let source = include_str!("window_registry_state.rs").replace("\r\n", "\n");
    let map_impl = source.split("impl WindowMap {").nth(1).expect("the WindowMap impl");
    let map_impl = map_impl.split("\n}\n").next().expect("its end");
    for method in ["insert", "remove"] {
        assert!(map_impl.contains(&format!("    fn {method}(")), "{method} is defined privately");
        assert!(
            !map_impl.contains(&format!("pub(in crate::app) fn {method}(")),
            "{method} is not public"
        );
    }
    // Outside the registry, no code reaches the map's insert or remove.
    let app_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let mut offenders = Vec::new();
    for entry in walk(&app_dir) {
        let text = std::fs::read_to_string(&entry).expect("readable source");
        if text.contains(".windows.insert(") || text.contains(".windows.remove(") {
            offenders.push(entry.display().to_string());
        }
    }
    let own = app_dir.join("window_registry_state.rs").display().to_string();
    offenders.retain(|path| *path != own && !path.ends_with("window_registry_state_tests.rs"));
    assert!(offenders.is_empty(), "{offenders:#?}");
}

/// Every `.rs` file below `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("readable directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}
