//! Regression tests for #680: READONLY mode is scoped to the dispatching
//! window, not global app state.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Action, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

#[test]
fn main_readonly_does_not_mark_child_windows_readonly() {
    let mut app = app();
    let main = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.__test_child_active_pane(child).expect("child pane");
    assert_ne!(main, child_pane);

    assert!(app.run_action_for_window(&Action::EnterCopyMode, app.__test_main_window_id().unwrap()));

    assert!(app.__test_main_read_only());
    assert_eq!(app.__test_child_read_only(child), Some(false));
}

#[test]
fn child_readonly_does_not_mark_main_or_sibling_child_readonly() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child_a = app.__test_seed_child_window(&["child-a"]);
    let child_b = app.__test_seed_child_window(&["child-b"]);

    assert!(app.run_action_for_window(&Action::EnterCopyMode, child_a));

    assert!(!app.__test_main_read_only());
    assert_eq!(app.__test_child_read_only(child_a), Some(true));
    assert_eq!(app.__test_child_read_only(child_b), Some(false));
}
