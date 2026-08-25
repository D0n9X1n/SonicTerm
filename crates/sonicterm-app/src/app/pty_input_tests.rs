use super::*;
use crate::app::spawn_pane::{osc52_clipboard_write_event, MAX_OSC52_CLIPBOARD_BYTES};
use base64::Engine;
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

/// Valid OSC 52 clipboard writes decode exactly before app-thread delivery.
#[test]
fn osc52_clipboard_write_decodes_bounded_utf8() {
    let encoded = base64::engine::general_purpose::STANDARD.encode("Copilot copied text");

    assert_eq!(
        osc52_clipboard_write_event('c', &encoded),
        Some(UserEvent::ClipboardWrite { text: "Copilot copied text".into() })
    );
}

/// Queries, unsupported targets, malformed data, and oversized writes fail closed.
#[test]
fn osc52_clipboard_write_rejects_unsupported_or_unsafe_payloads() {
    assert_eq!(osc52_clipboard_write_event('c', "?"), None);
    assert_eq!(osc52_clipboard_write_event('p', "dGV4dA=="), None);
    assert_eq!(osc52_clipboard_write_event('c', "not base64!"), None);
    assert_eq!(
        osc52_clipboard_write_event(
            'c',
            &"A".repeat((MAX_OSC52_CLIPBOARD_BYTES + 1).div_ceil(3) * 4)
        ),
        None
    );
    let invalid_utf8 = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]);
    assert_eq!(osc52_clipboard_write_event('c', &invalid_utf8), None);
}

/// Event-loop clipboard delivery uses the same system/test write seam as local copy.
#[test]
fn osc52_clipboard_event_writes_on_the_app_thread() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_set_memory_clipboard("old");

    app.handle_clipboard_write("new from OSC 52".into());

    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("new from OSC 52"));
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
