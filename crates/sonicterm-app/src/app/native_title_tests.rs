//! Pins the native window title: which text it shows (`#N Name`, `#N <tab body>`, `#N
//! SonicTerm`), how a tab's body is taken out of its displayed title, and when the native title
//! is written — only on a change, at most once per interval, with the last change delivered.

use std::time::{Duration, Instant};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::tabs::Tab;

use super::*;
use crate::app::native_title_font::title_font_draws;
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

#[test]
fn switching_tabs_retitles_the_window_at_once_even_inside_the_interval() {
    // A tab switch is the user's own action: it is written immediately, never held behind the
    // write interval a program's rapid retitling is subject to.
    let (mut app, main) = app_with_tabs(&["first", "second", "third"]);
    let now = Instant::now();
    let _ = app.sync_native_title(main, now, false);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 third"));
    for (index, expected) in [(0, "#1 first"), (1, "#1 second"), (2, "#1 third")] {
        assert!(app.__test_invoke_activate_main_tab(index));
        assert_eq!(app.sync_native_titles(now + Duration::from_millis(1)), None, "nothing waits");
        assert_eq!(state(&app, main).applied.as_deref(), Some(expected));
    }
}

#[test]
fn dragging_a_tab_out_retitles_the_source_window_after_its_next_tab() {
    // The window a tab leaves takes its new active tab's title at once; the window the tab
    // joins takes that tab's title.
    let (mut app, main) = app_with_tabs(&["left", "dragged"]);
    let child = app.__test_seed_child_window(&["other"]);
    let now = Instant::now();
    let _ = app.sync_native_titles(now);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 dragged"));
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((sonicterm_ui::pane::Rect { x: 0.0, y: 0.0, w: 800.0, h: 500.0 }, 10.0, 20.0));
    app.transfer_tab(None, 1, Some(child), 1).unwrap();
    assert_eq!(app.sync_native_titles(now + Duration::from_millis(1)), None);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 left"));
    assert_eq!(state(&app, child).applied.as_deref(), Some("#2 dragged"));
}

#[test]
fn the_formatter_parser_handles_its_edge_cases() {
    // Multi-digit positions and trailing spaces parse; an icon with no separator after it is not
    // the formatter's shape and is shown whole.
    let icon = sonicterm_ui::tab_title::PROGRAM_ICONS[3];
    assert_eq!(tab_display_body(&tab(&format!("#128 {icon} logs  "))), "logs  ");
    assert_eq!(tab_display_body(&tab(&format!("#12 {icon} "))), "");
    let no_separator = format!("#12 {icon}");
    assert_eq!(tab_display_body(&tab(&no_separator)), no_separator, "not the exact shape");
    assert_eq!(tab_display_body(&tab(&format!("#2 {icon}logs"))), format!("#2 {icon}logs"));
    // A blank formatted body names nothing, so the window falls back to the application name;
    // a body that is only a control is shown as U+FFFD, never discarded.
    let title_of = |raw: &str| window_title_text(1, "", Some(tab_display_body(&tab(raw))));
    assert_eq!(title_of(&format!("#12 {icon} ")), "#1 SonicTerm");
    assert_eq!(title_of(&format!("#12 {icon} \t")), "#1 \u{FFFD}");
}

#[test]
fn controls_are_replaced_before_spaces_are_trimmed() {
    // A tab or newline at either end is shown as U+FFFD, never silently trimmed away; spaces trim.
    assert_eq!(window_title_text(1, "", Some("work\t")), "#1 work\u{FFFD}");
    assert_eq!(window_title_text(1, "", Some("  work  ")), "#1 work");
    assert_eq!(window_title_text(1, "", Some(" \n ")), "#1 \u{FFFD}");
}

#[test]
fn a_launch_title_beats_the_tab_like_a_rename() {
    // `-T TITLE` names the main window; that name wins over the active tab until it is cleared.
    let (mut app, main) = app_with_tabs(&["shell"]);
    app.set_main_window_name("Build box");
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 Build box"));
    app.windows.get_mut(&main).unwrap().tabs.set_active_title("vim");
    let _ = app.sync_native_titles(Instant::now() + Duration::from_secs(1));
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 Build box"));
}

#[test]
fn each_window_keeps_its_own_interval() {
    // One window's throttled retitle neither delays nor is delayed by another window's write.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let child = app.__test_seed_child_window(&["other"]);
    let start = Instant::now();
    let _ = app.sync_native_titles(start);
    app.windows.get_mut(&main).unwrap().tabs.set_active_title("main retitled");
    let main_due = app.sync_native_titles(start + Duration::from_millis(100));
    assert_eq!(main_due, Some(start + NATIVE_TITLE_MIN_INTERVAL));
    let child_at = start + Duration::from_millis(400);
    app.windows.get_mut(&child).unwrap().tabs.set_active_title("child retitled");
    assert_eq!(app.sync_native_titles(child_at), None, "both are now due or free");
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 main retitled"));
    assert_eq!(state(&app, child).applied.as_deref(), Some("#2 child retitled"));
}

#[test]
fn a_waiting_title_is_not_composed_again_before_its_deadline() {
    // Passes before the deadline only report it; the pending title is written once, when due.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let start = Instant::now();
    let _ = app.sync_native_title(main, start, false);
    app.windows.get_mut(&main).unwrap().tabs.set_active_title("vim");
    let due = app.sync_native_title(main, start + Duration::from_millis(10), false);
    for step in 2..10 {
        let at = start + Duration::from_millis(10 * step);
        assert_eq!(app.sync_native_title(main, at, false), due);
    }
    assert_eq!(state(&app, main).writes, 1);
    let _ = app.sync_native_title(main, due.unwrap(), false);
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 vim"));
    assert_eq!(state(&app, main).writes, 2);
}

#[test]
fn a_waiting_title_arms_a_wake_that_delivers_it_without_a_frame() {
    // A throttled retitle arms a `NativeTitle` wake at its deadline; servicing that wake requests
    // no frame, and the about-to-wait pass the wake causes writes the title.
    let (mut app, main) = app_with_tabs(&["shell"]);
    let start = Instant::now();
    assert!(app.native_title_wake(start).is_none());
    app.windows.get_mut(&main).unwrap().tabs.set_active_title("vim");
    let wake = app.native_title_wake(start + Duration::from_millis(5)).expect("a wake is armed");
    assert_eq!(wake.cause, crate::app::redraw::DueCause::NativeTitle);
    assert_eq!(wake.deadline, start + NATIVE_TITLE_MIN_INTERVAL);
    app.redraw_due = vec![wake];
    app.service_redraw_due(wake.deadline);
    assert!(!app.windows[&main].redraw.request_in_flight, "a title wake is never a frame");
    assert!(app.native_title_wake(wake.deadline).is_none());
    assert_eq!(state(&app, main).applied.as_deref(), Some("#1 vim"));
}

#[test]
fn about_to_wait_arms_the_title_wake_before_building_accessibility() {
    // The pass that arms the wake runs before the accessibility tree reads the native title.
    let source = include_str!("event_loop_wait.rs");
    let wake = source.find("self.native_title_wake(now)").expect("wake armed each pass");
    let accessibility = source.find("self.flush_accessibility(now)").expect("accessibility");
    assert!(wake < accessibility);
}

/// A title font with no private-use glyph but the Apple logo, as the macOS title font has.
fn apple_logo_only(character: char) -> bool {
    character == '\u{F8FF}'
}

#[test]
fn icons_the_title_font_cannot_draw_are_left_out() {
    // A Nerd Font icon would draw as an empty box in the platform's title font, so it is dropped
    // and the spaces it leaves collapse into one.
    let shown =
        window_title_text_with(1, "", Some("\u{F489} build \u{E0B0} main"), apple_logo_only);
    assert_eq!(shown, "#1 build main");
}

#[test]
fn a_part_made_only_of_icons_falls_through_to_the_next_source() {
    // An icon-only body shows the application name; an icon-only window name shows the tab.
    assert_eq!(
        window_title_text_with(1, "", Some("\u{F489} \u{E0B0}"), apple_logo_only),
        "#1 SonicTerm"
    );
    assert_eq!(window_title_text_with(1, "\u{F489}", Some("vim"), apple_logo_only), "#1 vim");
}

#[test]
fn only_private_use_scalars_the_font_lacks_are_dropped() {
    // Every private-use plane boundary is checked, a glyph the font maps (U+F8FF) stays, and
    // the scalars just outside private use (U+D7FF, U+F900) and CJK and emoji text survive.
    let none = |_: char| false;
    for private_use in
        ['\u{E000}', '\u{F8FF}', '\u{F0000}', '\u{FFFFD}', '\u{100000}', '\u{10FFFD}']
    {
        assert_eq!(window_title_text_with(1, "", Some(&format!("a{private_use}b")), none), "#1 ab");
    }
    assert_eq!(window_title_text_with(1, "", Some("a\u{F8FF}b"), apple_logo_only), "#1 a\u{F8FF}b");
    let kept = "\u{D7FF}\u{F900} 日本語 🚀 👩\u{200D}💻 ✔\u{FE0F}";
    assert_eq!(window_title_text_with(1, "", Some(kept), none), format!("#1 {kept}"));
}

#[test]
fn controls_next_to_an_icon_still_show_as_replacement_characters() {
    // Controls are replaced before icons are filtered, so a tab beside an icon is never lost.
    assert_eq!(
        window_title_text_with(1, "", Some("\u{F489}\twork"), apple_logo_only),
        "#1 \u{FFFD}work"
    );
}

#[test]
fn the_length_cap_counts_only_the_scalars_that_remain() {
    // Leading icons must not use up the cap and cut the words a user can read.
    let icons = "\u{F489}".repeat(MAX_NATIVE_TITLE_SCALARS);
    let words = "x".repeat(MAX_NATIVE_TITLE_SCALARS);
    let shown = window_title_text_with(1, "", Some(&format!("{icons}{words}")), apple_logo_only);
    assert_eq!(shown, format!("#1 {words}"));
}

#[test]
fn the_platform_title_font_keeps_ordinary_text_and_drops_a_nerd_icon() {
    // The real oracle on this host: text outside private use is never filtered (the platform's
    // font cascade draws CJK the title font lacks), and a Nerd Font icon no title font carries is.
    assert!(title_font_draws('a'));
    assert!(title_font_draws('日'));
    assert!(!title_font_draws('\u{F489}'));
    assert!(!title_font_draws('\u{F0001}'));
}
