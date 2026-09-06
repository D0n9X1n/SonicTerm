use super::*;
use crate::app::spawn_pane::{osc52_clipboard_write_event, MAX_OSC52_CLIPBOARD_BYTES};
use base64::Engine;
use sonicterm_io::pty::{PtyInputDiagnostics, PtyInputError, PtyWriterPhase};

fn input_observation() -> PtyInputDiagnostics {
    PtyInputDiagnostics {
        queued_messages: 4,
        queued_bytes: 48,
        queue_capacity: 4,
        writer_phase: PtyWriterPhase::Writing,
        in_flight_bytes: 12,
        completed_messages: 8,
        in_flight_millis: Some(30),
    }
}

#[test]
fn rejection_event_debug_does_not_expose_terminal_input() {
    // Rejection diagnostics must not carry user text into derived event debugging.
    let private_input = b"private-shell-input".to_vec();
    let event = pty_input_rejected_event(
        7,
        PtyInputSource::Paste,
        PtyInputError::QueueFull(private_input.clone()),
        input_observation(),
    );
    let diagnostic = format!("{event:?}");
    assert!(!diagnostic.contains("private-shell-input"));
    assert!(!diagnostic.contains(&format!("{private_input:?}")));
}

#[test]
fn rejection_event_preserves_identity_and_observations_without_payload() {
    // Identical bytes from different producers retain their typed origin and affected pane.
    for source in [
        PtyInputSource::Keyboard,
        PtyInputSource::Paste,
        PtyInputSource::FileDrop,
        PtyInputSource::Ime,
        PtyInputSource::PointerButton,
        PtyInputSource::PointerMotion,
        PtyInputSource::Wheel,
        PtyInputSource::FocusReport,
        PtyInputSource::TerminalReply,
        PtyInputSource::ScriptDraft,
        PtyInputSource::StateMachine,
    ] {
        let event = pty_input_rejected_event(
            7,
            source,
            PtyInputError::QueueFull(b"same bytes".to_vec()),
            input_observation(),
        );
        assert!(
            matches!(event, UserEvent::PtyInputRejected { pane_id: 7, source: actual, rejected_bytes: 10, reason, diagnostics }
            if actual == source && reason.contains("queue is full") && diagnostics == input_observation())
        );
    }
}

#[derive(Clone, Default)]
struct InputDiagnosticLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for InputDiagnosticLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_input_warning(action: impl FnOnce()) -> String {
    let log = InputDiagnosticLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, action);
    let output = log.0.lock().clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn rejected_input_remains_observable_when_event_delivery_fails() {
    // A closed event loop must not swallow the final overload evidence or reveal its rejected payload.
    let private_input = b"private-shell-input".to_vec();
    let log = capture_input_warning(|| {
        App::deliver_pty_input_rejection(
            Some(Err),
            pty_input_rejected_event(
                7,
                PtyInputSource::TerminalReply,
                PtyInputError::QueueFull(private_input.clone()),
                input_observation(),
            ),
        );
    });
    for field in [
        "pane_id=7",
        "source=TerminalReply",
        "rejected_bytes=19",
        "writer_phase=Writing",
        "queued_messages=4",
        "observation=\"concurrent\"",
    ] {
        assert!(log.contains(field), "missing {field} in rejection warning: {log}");
    }
    assert!(!log.contains("private-shell-input"));
    assert!(!log.contains(&format!("{private_input:?}")));
    assert_eq!(log.matches("terminal input was not queued").count(), 1);
}

#[test]
fn delivered_rejections_are_logged_once_by_the_event_loop() {
    // Successful delivery leaves logging and current-window attribution to the event-loop handler.
    let mut delivered = None;
    let log = capture_input_warning(|| {
        App::deliver_pty_input_rejection(
            Some(|event| {
                delivered = Some(event);
                Ok(())
            }),
            pty_input_rejected_event(
                7,
                PtyInputSource::Keyboard,
                PtyInputError::QueueFull(vec![b'x']),
                input_observation(),
            ),
        );
    });
    assert!(log.is_empty());
    assert!(matches!(delivered, Some(UserEvent::PtyInputRejected { pane_id: 7, .. })));
}

#[test]
fn rejection_notification_follows_the_pane_not_the_frontmost_window() {
    // A queued rejection follows a transferred pane and never appears on a surviving unrelated window.
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let pane_id = app.__test_child_active_pane(child).unwrap();
    app.__test_set_frontmost_window(app.__test_main_window_id());
    app.handle_pty_input_rejected(
        pane_id,
        PtyInputSource::Wheel,
        12,
        "queue full".into(),
        input_observation(),
    );
    assert!(app.__test_child_notification_message(child).unwrap().contains("Wheel"));
    assert!(app.__test_main_notification_message().is_none());
    assert!(app.transfer_tab(Some(child), 0, None, 1).is_ok());
    app.handle_pty_input_rejected(
        pane_id,
        PtyInputSource::PointerMotion,
        12,
        "queue full".into(),
        input_observation(),
    );
    assert!(app.__test_main_notification_message().unwrap().contains("PointerMotion"));
    app.main_mut().unwrap().notification = None;
    app.handle_pty_input_rejected(
        u64::MAX,
        PtyInputSource::Keyboard,
        1,
        "disconnected".into(),
        input_observation(),
    );
    assert!(app.__test_main_notification_message().is_none());
}

/// Terminal key ownership includes only panes whose PTY accepted the press.
#[test]
fn terminal_key_dispatch_excludes_attempted_but_undelivered_panes() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane_id = app.__test_seed_tab("missing-pty");

    let delivered =
        app.dispatch_terminal_key_writes(vec![(pane_id, b"a".to_vec()), (u64::MAX, b"b".to_vec())]);

    assert!(delivered.is_empty());
}

/// Accepted Windows PTY input fixes one foreground-process probe deadline.
#[cfg(windows)]
#[test]
fn accepted_windows_pty_input_arms_foreground_probe() {
    let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
        "cmd.exe",
        &["/D".into(), "/Q".into()],
        80,
        24,
    )
    .expect("spawn interactive Windows PTY");
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane_id = app.__test_seed_tab("cmd");
    app.main_mut()
        .expect("synthetic main window")
        .panes
        .get_mut(&pane_id)
        .expect("synthetic pane")
        .pty = Some(pty);
    let effect = sonicterm_app_core::AppEffect::PtyWrite {
        pane: sonicterm_app_core::PaneId(pane_id),
        data: bytes::Bytes::from_static(b"ver\r"),
    };
    let before = std::time::Instant::now();

    app.dispatch_pty_write_effect(&effect, PtyInputSource::Keyboard);

    let wake = app.foreground_probe_wake.expect("accepted input arms a probe");
    assert!(wake.fixed);
    assert!(wake.due >= before + FOREGROUND_PROCESS_TTL);
    assert!(wake.due <= std::time::Instant::now() + FOREGROUND_PROCESS_TTL);
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

/// The documented OSC 52 cap accepts its exact byte count and rejects the next byte.
#[test]
fn osc52_clipboard_write_enforces_actual_decoded_boundary() {
    let exact = vec![b'x'; MAX_OSC52_CLIPBOARD_BYTES];
    let exact_encoded = base64::engine::general_purpose::STANDARD.encode(&exact);
    let accepted = osc52_clipboard_write_event('c', &exact_encoded);
    assert!(
        matches!(accepted, Some(UserEvent::ClipboardWrite { text }) if text.len() == exact.len())
    );

    let oversized = vec![b'x'; MAX_OSC52_CLIPBOARD_BYTES + 1];
    let oversized_encoded = base64::engine::general_purpose::STANDARD.encode(oversized);
    assert_eq!(osc52_clipboard_write_event('c', &oversized_encoded), None);
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

/// Windows reasserts an OSC 52 write after a failing native helper, but never over a newer copy.
#[cfg(target_os = "windows")]
#[test]
fn windows_osc52_reassertion_is_bounded_and_supersedable() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_set_memory_clipboard("old");

    app.handle_clipboard_write("Copilot selection".into());
    let due = app
        .pending_osc52_reassert
        .as_ref()
        .expect("a successful Windows OSC 52 write schedules one reassertion")
        .due;

    app.test_clipboard_text = Some("old".into());
    app.reassert_osc52_clipboard_if_due(due);
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("Copilot selection"));
    assert!(app.pending_osc52_reassert.is_none(), "the retry is one-shot");

    app.handle_clipboard_write("stale OSC 52".into());
    app.test_clipboard_text = Some("newer external copy".into());
    app.reassert_osc52_clipboard_if_due(due + std::time::Duration::from_secs(1));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("newer external copy"));

    app.handle_clipboard_write("another stale OSC 52".into());
    assert!(app.set_clipboard_text("newer local copy".into()));
    app.reassert_osc52_clipboard_if_due(due + std::time::Duration::from_secs(2));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("newer local copy"));

    app.pending_osc52_reassert = Some(PendingOsc52Reassert {
        text: "must not replace unreadable content".into(),
        previous_text: None,
        due,
    });
    app.reassert_osc52_clipboard_if_due(due);
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("newer local copy"));
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
