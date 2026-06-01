//! M6a-expand-2b — leaf routing through `AppStateMachine`.
//!
//! Proves the keystroke / PTY-write boundary path emits a
//! `AppEffect::PtyWrite` via the new reducer. The boundary then
//! resolves the pane id back to the live `PtyHandle`; here we
//! exercise just the Intent→Effect surface (the boundary code path
//! is covered indirectly by the §13 GUI smoke).

use bytes::Bytes;
use sonicterm_app_core::{AppEffect, AppIntent, AppState, AppStateMachine, PaneId};
use sonicterm_types::WindowKey;

fn wk() -> WindowKey {
    WindowKey::new(1)
}

#[test]
fn keystroke_byte_routes_to_pty_write_effect() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::PtyWrite { pane: PaneId(42), bytes: Bytes::from_static(b"a") });
    assert_eq!(out.len(), 1, "PtyWrite is a 1:1 leaf");
    match &out[0] {
        AppEffect::PtyWrite { pane, data } => {
            assert_eq!(*pane, PaneId(42));
            assert_eq!(&data[..], b"a");
        }
        other => panic!("expected PtyWrite effect, got {other:?}"),
    }
}

#[test]
fn ime_commit_routes_through_state_machine() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::ImeCommit { window: wk(), text: "中".into() });
    assert!(out.iter().any(|e| matches!(e, AppEffect::PtyWrite { .. })));
    assert!(out.iter().any(|e| matches!(e, AppEffect::Render { .. })));
}
