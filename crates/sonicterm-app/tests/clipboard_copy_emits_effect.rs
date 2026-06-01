//! M6a-expand-2b — clipboard intent routes through the state machine.

use sonicterm_app_core::{AppEffect, AppIntent, AppState, AppStateMachine};
use sonicterm_types::WindowKey;

#[test]
fn copy_selection_intent_emits_clipboard_set_effect() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::CopySelection { window: WindowKey::new(1) });
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], AppEffect::ClipboardSet { .. }));
}

#[test]
fn paste_intent_emits_pty_write_effect() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::Paste {
        window: WindowKey::new(1),
        text: "hello".into(),
        bracketed: false,
    });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::PtyWrite { data, .. } => assert_eq!(&data[..], b"hello"),
        other => panic!("expected PtyWrite, got {other:?}"),
    }
}
