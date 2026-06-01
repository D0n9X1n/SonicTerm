//! Focused per-Intent tests for the window-lifecycle reducer arms
//! introduced in M6a-expand-2c-window.
//!
//! For each Intent we assert (a) the Effect batch shape (class-sorted,
//! correct variant + payload), and (b) the `AppState` mutation —
//! exactly what the FINAL spec §3 mapping promises and what the
//! boundary in `sonicterm-app::app::*` relies on.

use sonicterm_app_core::{
    AppEffect, AppIntent, AppState, AppStateMachine, LogicalPos, RedrawReason, WindowRole,
};
use sonicterm_types::WindowKey;

fn wk(n: u64) -> WindowKey {
    WindowKey::new(n)
}

fn sm() -> AppStateMachine {
    AppStateMachine::new(AppState::default())
}

#[test]
fn new_window_emits_window_open_and_bumps_count() {
    let mut m = sm();
    assert_eq!(m.state().live_window_count, 0);
    let out = m.handle(AppIntent::NewWindow { role: WindowRole::Primary });
    assert_eq!(out.len(), 1, "got {:?}", out.as_slice());
    match &out[0] {
        AppEffect::WindowOpen { role, initial_size } => {
            assert_eq!(*role, WindowRole::Primary);
            assert!(initial_size.is_none());
        }
        other => panic!("expected WindowOpen, got {other:?}"),
    }
    assert_eq!(m.state().live_window_count, 1);
}

#[test]
fn window_close_requested_emits_close_and_decrements() {
    let mut m = sm();
    let _ = m.handle(AppIntent::NewWindow { role: WindowRole::Primary });
    let _ = m.handle(AppIntent::NewWindow { role: WindowRole::Primary });
    assert_eq!(m.state().live_window_count, 2);
    let out = m.handle(AppIntent::WindowCloseRequested { window: wk(1) });
    // Two windows alive → close the first, no cascading Quit.
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], AppEffect::WindowClose { .. }));
    assert_eq!(m.state().live_window_count, 1);
}

#[test]
fn window_close_requested_last_window_cascades_quit() {
    let mut m = sm();
    let _ = m.handle(AppIntent::NewWindow { role: WindowRole::Primary });
    let out = m.handle(AppIntent::WindowCloseRequested { window: wk(1) });
    // Sorted: WindowClose (WindowOp=4) + Quit (WindowOp=4) — stable
    // sort keeps push order.
    assert_eq!(out.len(), 2);
    assert!(matches!(out[0], AppEffect::WindowClose { .. }));
    assert!(matches!(out[1], AppEffect::Quit));
    assert_eq!(m.state().live_window_count, 0);
}

#[test]
fn window_focused_transition_emits_render_focus() {
    let mut m = sm();
    let out = m.handle(AppIntent::WindowFocused { window: wk(7) });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::Render { window, reason } => {
            assert_eq!(*window, wk(7));
            assert_eq!(*reason, RedrawReason::Focus);
        }
        other => panic!("expected Render(Focus), got {other:?}"),
    }
    assert_eq!(m.state().focused_window, Some(wk(7)));
    // Idempotent re-focus on the already-focused window emits nothing.
    let out2 = m.handle(AppIntent::WindowFocused { window: wk(7) });
    assert!(out2.is_empty(), "duplicate focus should be a no-op, got {:?}", out2.as_slice());
}

#[test]
fn window_blurred_after_focus_transitions_and_emits_render() {
    let mut m = sm();
    let _ = m.handle(AppIntent::WindowFocused { window: wk(2) });
    let out = m.handle(AppIntent::WindowBlurred { window: wk(2) });
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], AppEffect::Render { reason: RedrawReason::Focus, .. }));
    assert_eq!(m.state().focused_window, None);
}

#[test]
fn window_blurred_when_not_focused_is_noop() {
    let mut m = sm();
    // No prior Focused — blur is a no-op (spurious OS event).
    let out = m.handle(AppIntent::WindowBlurred { window: wk(2) });
    assert!(out.is_empty());
    // Blur for a different window than the focused one is also a no-op.
    let _ = m.handle(AppIntent::WindowFocused { window: wk(1) });
    let out2 = m.handle(AppIntent::WindowBlurred { window: wk(99) });
    assert!(out2.is_empty());
    assert_eq!(m.state().focused_window, Some(wk(1)));
}

#[test]
fn window_resized_mutates_grid_and_emits_render_plus_resize() {
    let mut m = sm();
    let out = m.handle(AppIntent::WindowResized { window: wk(1), cols: 120, rows: 40 });
    assert_eq!(out.len(), 2);
    // Sorted: Render(class 1) before WindowResize(class 4).
    match &out[0] {
        AppEffect::Render { window, reason } => {
            assert_eq!(*window, wk(1));
            assert_eq!(*reason, RedrawReason::Resize);
        }
        other => panic!("expected Render(Resize) first, got {other:?}"),
    }
    match &out[1] {
        AppEffect::WindowResize { window, size } => {
            assert_eq!(*window, wk(1));
            assert!((size.width - 120.0).abs() < f64::EPSILON);
            assert!((size.height - 40.0).abs() < f64::EPSILON);
        }
        other => panic!("expected WindowResize second, got {other:?}"),
    }
    assert_eq!(m.state().cols, 120);
    assert_eq!(m.state().rows, 40);
}

#[test]
fn window_moved_records_pos_without_effects() {
    let mut m = sm();
    let pos = LogicalPos { x: 100.5, y: -3.0 };
    let out = m.handle(AppIntent::WindowMoved { window: wk(1), pos });
    assert!(out.is_empty(), "Moved is record-only; got {:?}", out.as_slice());
    assert_eq!(m.state().last_window_pos, Some(pos));
}
