//! M6a-expand-2b — leaf keymap actions route through `AppStateMachine`.
//!
//! Phase-2b cap: only Exit / scroll-to-bottom / hyperlink-open are
//! routed via `App::dispatch_intent` directly. Cmd+T (NewTab) and
//! Cmd+D (SplitPane) remain on the legacy direct path until
//! 2c lifts the pane-tree state into `AppState`. This test pins the
//! `Exit` keystroke as the canonical leaf-routed action: driving the
//! `AppIntent::Exit` produces `AppEffect::Quit`, which the boundary
//! translates into `App.pending_exit = true`.

use sonicterm_app_core::{AppEffect, AppIntent, AppState, AppStateMachine};

#[test]
fn exit_intent_routes_to_quit_effect() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::Exit);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], AppEffect::Quit));
}

#[test]
fn redraw_requested_routes_to_render_effect() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::RedrawRequested { window: sonicterm_types::WindowKey::new(7) });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::Render { window, .. } => assert_eq!(window.raw(), 7),
        other => panic!("expected Render, got {other:?}"),
    }
}

#[test]
fn click_url_routes_to_open_url_effect() {
    let mut m = AppStateMachine::new(AppState::default());
    let out = m.handle(AppIntent::ClickUrl {
        window: sonicterm_types::WindowKey::new(1),
        url: "https://example.com/path".into(),
    });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::OpenURL { url } => assert_eq!(url, "https://example.com/path"),
        other => panic!("expected OpenURL, got {other:?}"),
    }
}
