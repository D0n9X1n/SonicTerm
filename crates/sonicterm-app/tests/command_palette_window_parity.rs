//! Command palette routing parity tests.
//!
//! The visual compactness of the palette is computed in `sonicterm-ui`, but
//! the app decides which window receives the shared palette overlay. These
//! tests pin that the same compact palette can attach to either the main window
//! or a torn-out child window, so main/child cannot drift.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Action, keymap::Keymap, theme::Theme};
use winit::event::Ime;

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

#[test]
fn command_palette_opens_attached_to_main_when_main_is_frontmost() {
    let mut app = app();
    app.__test_seed_tab("main");
    // In headless tests there is no real winit main window, so leave
    // frontmost unset; production treats `None` as the safe main-window
    // fallback and attaches the palette to main.
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_palette_open());
    assert_eq!(
        app.__test_palette_attached_window(),
        None,
        "main-frontmost palette should render on main (None attachment)"
    );
}

#[test]
fn command_palette_opens_attached_to_child_when_child_is_frontmost() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));

    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_palette_open());
    assert_eq!(
        app.__test_palette_attached_window(),
        Some(child),
        "child-frontmost palette should render on that child window"
    );
}

#[test]
fn command_palette_accepts_ime_commit_text_on_main_attachment() {
    let mut app = app();
    app.__test_seed_tab("main");
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_command_palette_handle_ime(&Ime::Commit("重命名".into())));

    assert_eq!(app.__test_palette_query(), "重命名");
    assert_eq!(app.__test_palette_cursor(), "重命名".len());
}

#[test]
fn command_palette_accepts_ime_commit_text_on_child_attachment() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_command_palette_handle_ime(&Ime::Commit("设置".into())));

    assert_eq!(app.__test_palette_attached_window(), Some(child));
    assert_eq!(app.__test_palette_query(), "设置");
}
