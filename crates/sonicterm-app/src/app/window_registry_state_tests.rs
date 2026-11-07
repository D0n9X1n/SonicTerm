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
        app.registry.pane_mut(pane).expect("an owned pane").agent_owned = true;
        assert!(app.registry.windows[&window].panes[&pane].agent_owned);
    }
    assert!(app.registry.pane(u64::MAX).is_none());
    assert!(app.registry.window_of_pane(u64::MAX).is_none());
}
