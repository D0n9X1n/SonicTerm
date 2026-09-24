use super::*;

#[test]
fn keyboard_quit_confirmation_stays_on_its_source_window() {
    // The warning follows the key's owner without retargeting focus or allowing repeats to confirm quit.
    for source_is_child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["child"]);
        let (source, other) = if source_is_child { (child, main) } else { (main, child) };
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();

        assert!(app.on_quit_chord_pressed(source, false));
        assert_eq!(
            app.windows[&source].notification.as_ref().map(|bubble| bubble.message.as_str()),
            Some(super::super::quit_hold::QUIT_CONFIRM_PROMPT)
        );
        assert!(app.windows[&other].notification.is_none());
        assert_eq!(app.frontmost_window, Some(other));
        assert!(!app.pending_exit);
        assert!(app.on_quit_chord_pressed(source, true));
        assert!(!app.pending_exit);
        assert!(app.on_quit_chord_pressed(source, false));
        assert!(app.pending_exit);
        assert!(app.__test_drain_pty_writes().is_empty());
    }
}

#[test]
fn stale_keyboard_quit_cannot_arm_a_live_window() {
    // A removed source must not contribute the first press or place a warning on the frontmost window.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["removed"]);
    app.windows.remove(&child);
    app.frontmost_window = Some(main);

    assert!(!app.on_quit_chord_pressed(child, false));
    assert!(app.windows[&main].notification.is_none());
    assert!(!app.pending_exit);
    assert!(app.on_quit_chord_pressed(main, false));
    assert!(!app.pending_exit);
    assert!(app.windows[&main].notification.is_some());
}
