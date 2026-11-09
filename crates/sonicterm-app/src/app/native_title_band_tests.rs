//! The applied-title snapshot the status line's window segment reads: its provenance follows the
//! inputs even when the text does not change, and it moves only when the native title is written.

use std::time::{Duration, Instant};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

use super::*;
use crate::app::status_line::window_title_input;
use crate::app::App;

/// An app whose main window holds one tab titled `title`, with its title state cleared.
fn app_with_tab(title: &str) -> (App, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab(title);
    let main = app.registry.main_window_id.unwrap();
    app.registry.window_keys.intern(main);
    app.registry.windows.get_mut(&main).unwrap().native_title = NativeTitleState::default();
    (app, main)
}

/// Naming the window the same text its tab shows writes nothing new, yet the snapshot now marks the
/// text as the user's name; clearing the name marks it a tab title again, so `hide_sensitive`
/// redacts it. The window segment's input carries the same provenance.
#[test]
fn provenance_follows_the_inputs_when_the_text_is_equal() {
    let (mut app, main) = app_with_tab("Work");
    let now = Instant::now();
    let _ = app.sync_native_title(main, now, false);
    let input = window_title_input(&app.registry.windows[&main]).expect("a numbered window");
    assert_eq!((input.shown.as_str(), input.custom), ("Work", false));
    let writes = app.registry.windows[&main].native_title.writes;

    app.registry.windows.get_mut(&main).unwrap().custom_window_name = "Work".to_owned();
    let _ = app.sync_native_title(main, now, true);
    assert_eq!(app.registry.windows[&main].native_title.writes, writes, "same text, no write");
    assert!(window_title_input(&app.registry.windows[&main]).unwrap().custom, "now a name");

    app.registry.windows.get_mut(&main).unwrap().custom_window_name.clear();
    let _ = app.sync_native_title(main, now, true);
    assert_eq!(app.registry.windows[&main].native_title.writes, writes, "still no write");
    assert!(!window_title_input(&app.registry.windows[&main]).unwrap().custom, "a tab title again");
}

/// A program's rapid retitles reach the window segment no faster than the native title: the
/// snapshot keeps the written text until the throttle's trailing edge writes the newest one.
#[test]
fn the_snapshot_moves_only_when_the_title_is_written() {
    let (mut app, main) = app_with_tab("shell");
    let start = Instant::now();
    let _ = app.sync_native_title(main, start, false);
    app.registry.windows.get_mut(&main).unwrap().tabs.set_active_title("step 1");
    let _ = app.sync_native_title(main, start + Duration::from_millis(10), false);
    let shown = |app: &App| window_title_input(&app.registry.windows[&main]).unwrap().shown;
    assert_eq!(shown(&app), "shell", "a throttled retitle is not shown yet");
    let _ = app.sync_native_title(main, start + NATIVE_TITLE_MIN_INTERVAL, false);
    assert_eq!(shown(&app), "step 1", "the trailing edge shows it");
}
