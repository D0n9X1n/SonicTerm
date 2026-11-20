//! OSC 99 occasions and ids: when a notification shows, and when it replaces the one on screen.

use std::time::{Duration, Instant};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_vt::vt::NotifyOccasion;

use super::*;

/// A headless App with one child window of two tabs, `front` and the active `back`, focused.
fn occasion_app() -> (App, WindowId, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["front", "back"]);
    let panes: Vec<u64> =
        app.registry.windows[&child].tab_states.iter().map(|tab| tab.active_pane).collect();
    app.registry.frontmost_window = Some(child);
    (app, child, panes[0], panes[1])
}

/// Notify `pane_id` with `body` under `occasion` and report whether a toast opened.
fn shows(app: &mut App, pane_id: u64, body: &str, occasion: NotifyOccasion) -> bool {
    app.program.toast = None;
    app.handle_program_notice(pane_id, (None, body), None, occasion, 0, Instant::now());
    app.program.toast.is_some()
}

/// `unfocused` shows only while its pane is not the active pane of the focused window: the
/// focused active pane suppresses it, a background tab or an unfocused window shows it.
#[test]
fn unfocused_waits_until_the_pane_is_out_of_focus() {
    let (mut app, _, front, back) = occasion_app();
    assert!(!shows(&mut app, back, "x", NotifyOccasion::Unfocused), "the focused active pane");
    assert!(shows(&mut app, front, "x", NotifyOccasion::Unfocused), "a background tab");
    app.registry.frontmost_window = None;
    assert!(shows(&mut app, back, "x", NotifyOccasion::Unfocused), "the window lost focus");
}

/// `invisible` shows only while its pane is not drawn: the active tab's pane is drawn, even in
/// an unfocused window; a background tab or a covered window is not.
#[test]
fn invisible_waits_until_the_pane_is_not_drawn() {
    let (mut app, child, front, back) = occasion_app();
    app.registry.frontmost_window = None;
    assert!(!shows(&mut app, back, "x", NotifyOccasion::Invisible), "drawn though unfocused");
    assert!(shows(&mut app, front, "x", NotifyOccasion::Invisible), "a background tab");
    app.registry.windows.get_mut(&child).unwrap().redraw.native_occluded = true;
    assert!(shows(&mut app, back, "x", NotifyOccasion::Invisible), "a covered window");
}

/// A notification with the shown toast's pane and id replaces its text instead of counting, and
/// another pane's notification with the same id does not touch it.
#[test]
fn the_same_pane_and_id_replaces_the_toast() {
    let (mut app, _, front, back) = occasion_app();
    let start = Instant::now();
    let job = || Some("job".to_owned());
    app.handle_program_notice(
        front,
        (Some("Build"), "running"),
        job(),
        NotifyOccasion::Always,
        0,
        start,
    );
    app.handle_program_notice(
        front,
        (Some("Build"), "done"),
        job(),
        NotifyOccasion::Always,
        0,
        start + Duration::from_millis(10),
    );
    let toast = app.program.toast.clone().expect("a toast");
    assert_eq!((toast.body.as_str(), toast.more), ("done", 0), "replaced, not counted");
    app.handle_program_notice(
        back,
        (None, "elsewhere"),
        job(),
        NotifyOccasion::Always,
        0,
        start + Duration::from_millis(20),
    );
    let toast = app.program.toast.clone().expect("a toast");
    assert_eq!(
        (toast.pane_id, toast.body.as_str(), toast.more),
        (front, "done", 1),
        "another pane only counts"
    );
}
