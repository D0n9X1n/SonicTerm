use super::*;
use sonicterm_io::pty::PtyInputError;

#[test]
fn saturated_input_event_retains_rejected_bytes_for_user_notification() {
    let rejected = b"retry me".to_vec();

    let event = pty_input_rejected_event(PtyInputError::QueueFull(rejected.clone()));

    assert!(matches!(
        event,
        UserEvent::PtyInputRejected { bytes, reason }
            if bytes == rejected && reason.contains("queue is full")
    ));
}

#[test]
fn script_draft_rejection_becomes_a_visible_warning() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_synthetic_main();

    app.handle_script_draft_rejected("unsafe script path".to_string());

    assert_eq!(app.__test_main_notification_message(), Some("unsafe script path"));
}
