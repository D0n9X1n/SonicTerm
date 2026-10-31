//! Pins the native window title: which text it shows (`#N Name`, `#N <tab body>`, `#N
//! SonicTerm`), how a tab's body is taken out of its displayed title, and when the native title
//! is written — only on a change, at most once per interval, with the last change delivered.

use std::time::{Duration, Instant};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::tabs::Tab;

use super::*;
use crate::app::App;

/// A tab whose displayed title is `title` and which carries no custom title.
fn tab(title: &str) -> Tab {
    Tab::new(title)
}

/// An app whose synthetic main window holds one tab per entry of `titles`, the last one active,
/// with its title state cleared so each test counts only its own writes on its own clock.
fn app_with_tabs(titles: &[&str]) -> (App, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    for title in titles {
        app.__test_seed_tab(title);
    }
    let main = app.main_window_id.unwrap();
    app.window_keys.intern(main);
    app.windows.get_mut(&main).unwrap().native_title = NativeTitleState::default();
    (app, main)
}

/// The main window's native title state, for reading what was written.
fn state(app: &App, id: WindowId) -> &NativeTitleState {
    &app.windows[&id].native_title
}

#[test]
fn a_name_wins_then_the_tab_body_then_the_app_name() {
    // The three title sources in priority order; blank text never counts as a source.
    assert_eq!(window_title_text(2, "Work", Some("vim")), "#2 Work");
    assert_eq!(window_title_text(2, "", Some("vim")), "#2 vim");
    assert_eq!(window_title_text(2, "   ", Some("  ")), "#2 SonicTerm");
    assert_eq!(window_title_text(2, "", None), "#2 SonicTerm");
}

#[test]
fn control_characters_are_replaced_and_long_bodies_capped() {
    // A program's OSC title reaches the native title: no NUL (X11 panics) and no other control.
    assert_eq!(window_title_text(1, "", Some("a\u{0}b\u{7}c")), "#1 a\u{FFFD}b\u{FFFD}c");
    let long = "x".repeat(MAX_NATIVE_TITLE_SCALARS + 40);
    let shown = window_title_text(1, "", Some(&long));
    assert_eq!(shown.chars().count(), "#1 ".len() + MAX_NATIVE_TITLE_SCALARS);
}

#[test]
fn the_body_comes_out_of_the_formatter_shape_only() {
    // `#N <icon> body` from the tab formatter yields its body, spaces and all.
    let icon = sonicterm_ui::tab_title::PROGRAM_ICONS[0];
    assert_eq!(tab_display_body(&tab(&format!("#3 {icon} code/sonicterm"))), "code/sonicterm");
    assert_eq!(tab_display_body(&tab(&format!("#1 {icon} my session 2"))), "my session 2");
    // Anything else is the user's or the launcher's own text and is shown whole.
    assert_eq!(tab_display_body(&tab("shell 1")), "shell 1");
    assert_eq!(tab_display_body(&tab("#team notes")), "#team notes");
    assert_eq!(tab_display_body(&tab("#1 工作 notes")), "#1 工作 notes");
}

#[test]
fn a_custom_tab_title_is_shown_verbatim() {
    // A user's tab name is never re-parsed, even when it looks like a formatted title.
    let mut named = tab("#1 x y");
    named.custom_title = Some("#team notes".to_owned());
    assert_eq!(tab_display_body(&named), "#team notes");
}

#[test]
fn an_unnamed_window_follows_its_active_tab() {
    // Switching, renaming the active tab and closing to no tab each change the window title.
    let (mut app, main) = app_with_tabs(&["first", "second"]);
    let now = Instant::now();
    let _ = app.sync_native_title(main, now, false);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 second"));
    app.windows.get_mut(&main).unwrap().tabs.activate(0);
    let _ = app.sync_native_title(main, now + Duration::from_secs(1), false);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 first"));
    app.windows.get_mut(&main).unwrap().tabs.set_active_custom_title("Build");
    let _ = app.sync_native_title(main, now + Duration::from_secs(2), false);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 Build"));
    let window = app.windows.get_mut(&main).unwrap();
    while !window.tabs.is_empty() {
        let id = window.tabs.active().unwrap().id;
        window.tabs.close(id);
    }
    let _ = app.sync_native_title(main, now + Duration::from_secs(3), false);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 SonicTerm"));
}

#[test]
fn a_window_name_beats_the_tab_and_clearing_it_returns_the_tab() {
    // A rename writes at once even inside the interval; clearing the name gives the tab back.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let now = Instant::now();
    let _ = app.sync_native_title(main, now, false);
    app.windows.get_mut(&main).unwrap().custom_window_name = "Work".to_owned();
    let _ = app.sync_native_title(main, now, true);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 Work"));
    app.windows.get_mut(&main).unwrap().custom_window_name.clear();
    let _ = app.sync_native_title(main, now, true);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 shell"));
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 shell"));
}

#[test]
fn unchanged_inputs_write_nothing() {
    // Every event-loop pass checks the title; only a change reaches the native window.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let now = Instant::now();
    for step in 0..5 {
        assert_eq!(app.sync_native_title(main, now + Duration::from_secs(step), false), None);
    }
    assert_eq!(state(&app, main).writes, 1);
}

#[test]
fn rapid_changes_are_throttled_and_the_last_one_lands() {
    // A program retitling every frame makes one write per interval, and the newest title is
    // written on the interval's trailing edge rather than lost.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let start = Instant::now();
    let _ = app.sync_native_title(main, start, false);
    let mut deadline = None;
    for step in 1..=5_u32 {
        app.windows.get_mut(&main).unwrap().tabs.set_active_title(format!("step {step}"));
        deadline =
            app.sync_native_title(main, start + Duration::from_millis(10 * u64::from(step)), false);
    }
    assert_eq!(state(&app, main).writes, 1, "only the first write ran inside the interval");
    assert_eq!(deadline, Some(start + NATIVE_TITLE_MIN_INTERVAL));
    assert_eq!(app.sync_native_title(main, start + NATIVE_TITLE_MIN_INTERVAL, false), None);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 step 5"));
    assert_eq!(state(&app, main).writes, 2);
}

#[test]
fn the_event_loop_pass_reports_the_earliest_pending_title() {
    // `sync_native_titles` covers every window and hands back the wake for a throttled title.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let start = Instant::now();
    assert_eq!(app.sync_native_titles(start), None);
    app.windows.get_mut(&main).unwrap().tabs.set_active_title("vim");
    assert_eq!(app.sync_native_titles(start), Some(start + NATIVE_TITLE_MIN_INTERVAL));
}

#[test]
fn report_and_unnumbered_windows_are_left_alone() {
    // A report window keeps its fixed name, and a window with no number has no numbered title.
    let (mut app, main) = app_with_tabs(&["shell"]);
    app.windows.get_mut(&main).unwrap().role = crate::app::WindowRole::MemoryReport;
    assert_eq!(app.sync_native_title(main, Instant::now(), true), None);
    assert_eq!(state(&app, main).writes, 0);
    let mut unnumbered = App::new(Theme::default(), Config::default(), Keymap::default());
    unnumbered.__test_seed_tab("shell");
    let id = unnumbered.main_window_id.unwrap();
    unnumbered.window_keys.remove(id);
    unnumbered.windows.get_mut(&id).unwrap().native_title = NativeTitleState::default();
    assert_eq!(unnumbered.sync_native_title(id, Instant::now(), true), None);
    assert_eq!(state(&unnumbered, id).writes, 0);
}
