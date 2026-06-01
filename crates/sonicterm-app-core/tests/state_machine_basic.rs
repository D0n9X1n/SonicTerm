//! Smoke tests covering `AppStateMachine::new` / `handle` / `state`
//! / `drain_pending` API surface (M6a-expand-2a).

use sonicterm_app_core::{AppEffect, AppIntent, AppState, AppStateMachine};

#[test]
fn new_wraps_initial_state() {
    let sm = AppStateMachine::new(AppState::builder().with_grid(80, 24).build());
    assert_eq!(sm.state().cols, 80);
    assert_eq!(sm.state().rows, 24);
}

#[test]
fn handle_returns_empty_for_stub_reducer() {
    let mut sm = AppStateMachine::new(AppState::default());
    let out = sm.handle(AppIntent::Exit);
    assert!(out.is_empty(), "stub reducer must return empty Effect batch");
}

#[test]
fn drain_pending_empty_by_default() {
    let mut sm = AppStateMachine::new(AppState::default());
    let drained: Vec<AppEffect> = sm.drain_pending();
    assert!(drained.is_empty());
}

#[test]
fn state_accessor_is_read_only() {
    let sm = AppStateMachine::new(AppState::builder().with_grid(120, 40).build());
    // Compile-time: `state()` returns `&AppState`, not `&mut AppState`.
    let s: &AppState = sm.state();
    assert_eq!(s.cols, 120);
}
