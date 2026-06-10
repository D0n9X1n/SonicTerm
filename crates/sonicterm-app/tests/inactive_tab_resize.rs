//! Regression tests for #671: inactive tabs must pick up the current window
//! size when they become active after a resize.
//!
//! The production resize paths size only the active tab's panes. That is fine
//! for performance, but tab activation must lazily resize the newly-active tab
//! before it renders or receives more PTY input; otherwise a multi-tab window
//! shows stale wrapping/layout after a resize.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::pane::Rect;

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

#[test]
fn main_inactive_tab_resizes_when_activated_after_window_resize() {
    let mut app = app();
    let first = app.__test_seed_tab("first");
    let second = app.__test_seed_tab("second");

    assert!(app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
    assert!(app.__test_invoke_activate_main_tab(0));
    assert_eq!(app.__test_pane_grid_size(first), Some((80, 24)));
    assert_eq!(app.__test_pane_grid_size(second), Some((80, 24)));

    // Simulate a window resize while tab 0 is active. Production resizes only
    // the active tab here; tab 1 remains stale until activation.
    assert!(app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 400.0, 240.0), 10.0, 10.0));
    app.__test_resize_visible_panes();
    assert_eq!(app.__test_pane_grid_size(first), Some((40, 24)));
    assert_eq!(app.__test_pane_grid_size(second), Some((80, 24)), "inactive tab starts stale");

    assert!(app.__test_invoke_activate_main_tab(1));
    assert_eq!(
        app.__test_pane_grid_size(second),
        Some((40, 24)),
        "activating an inactive tab must resize it to the current viewport"
    );
}

#[test]
fn child_inactive_tab_resizes_when_activated_after_window_resize() {
    let mut app = app();
    let id = app.__test_seed_child_window(&["first", "second"]);

    assert!(app.__test_set_child_pane_viewport(id, Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
    assert!(app.__test_invoke_activate_tab_in_child(id, 0));
    let first = app.__test_child_active_pane(id).expect("first tab pane");
    assert!(app.__test_invoke_activate_tab_in_child(id, 1));
    let second = app.__test_child_active_pane(id).expect("second tab pane");
    assert_ne!(first, second);
    assert!(app.__test_invoke_activate_tab_in_child(id, 0));
    assert_eq!(app.__test_child_pane_grid_size(id, first), Some((80, 24)));
    assert_eq!(app.__test_child_pane_grid_size(id, second), Some((80, 24)));

    // Simulate child window resize while tab 0 is active.
    assert!(app.__test_set_child_pane_viewport(id, Rect::new(0.0, 0.0, 400.0, 240.0), 10.0, 10.0));
    app.__test_invoke_activate_tab_in_child(id, 0);
    assert_eq!(app.__test_child_pane_grid_size(id, first), Some((40, 24)));
    assert_eq!(app.__test_child_pane_grid_size(id, second), Some((80, 24)), "inactive child tab starts stale");

    assert!(app.__test_invoke_activate_tab_in_child(id, 1));
    assert_eq!(
        app.__test_child_pane_grid_size(id, second),
        Some((40, 24)),
        "activating an inactive child tab must resize it to the current viewport"
    );
}
