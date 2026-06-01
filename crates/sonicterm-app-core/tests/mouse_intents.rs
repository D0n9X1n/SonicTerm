//! Focused per-Intent tests for the mouse reducer arms introduced
//! in M6a-expand-2c-mouse.
//!
//! Mirrors the shape of `pane_intents.rs` / `tab_intents.rs`: each
//! Intent gets a state-mutation + Effect-shape check against FINAL
//! spec §3.
//!
//! The reducer tracks `last_mouse_pos` + `mouse_left_down`; the
//! boundary's `WindowState.{cursor_pos, mouse_down, selection,
//! drag_session}` remain source-of-truth for the actual hit-tests
//! (tab drag, selection extend, scrollbar drag, OSC8 hover). These
//! tests verify the observability + dedupe surface only.

use sonicterm_app_core::{
    AppEffect, AppIntent, AppState, AppStateMachine, LogicalPos, MouseButton, RedrawReason,
};
use sonicterm_types::{ModKey, WindowKey};

fn wk(n: u64) -> WindowKey {
    WindowKey::new(n)
}

fn sm() -> AppStateMachine {
    AppStateMachine::new(AppState::default())
}

fn lp(x: f64, y: f64) -> LogicalPos {
    LogicalPos { x, y }
}

fn assert_render(effect: &AppEffect, want_window: WindowKey, want_reason: RedrawReason) {
    match effect {
        AppEffect::Render { window, reason } => {
            assert_eq!(*window, want_window);
            assert_eq!(*reason, want_reason);
        }
        other => panic!("expected Render({want_reason:?}), got {other:?}"),
    }
}

#[test]
fn mouse_left_down_then_up_emits_selection_transitions() {
    let mut m = sm();
    // Press → transition (false → true), emits Render(Selection).
    let out = m.handle(AppIntent::MouseButton {
        window: wk(1),
        pressed: true,
        button: MouseButton::Left,
        mods: ModKey::empty(),
        pos: lp(10.0, 20.0),
    });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Selection);
    assert!(m.state().mouse_left_down);
    assert_eq!(m.state().last_mouse_pos, Some(lp(10.0, 20.0)));

    // Release → transition (true → false), emits Render(Selection).
    let out = m.handle(AppIntent::MouseButton {
        window: wk(1),
        pressed: false,
        button: MouseButton::Left,
        mods: ModKey::empty(),
        pos: lp(15.0, 25.0),
    });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Selection);
    assert!(!m.state().mouse_left_down);
    assert_eq!(m.state().last_mouse_pos, Some(lp(15.0, 25.0)));
}

#[test]
fn mouse_left_repeated_press_no_transition_no_emit() {
    let mut m = sm();
    let _ = m.handle(AppIntent::MouseButton {
        window: wk(1),
        pressed: true,
        button: MouseButton::Left,
        mods: ModKey::empty(),
        pos: lp(0.0, 0.0),
    });
    // Second press while already-down: no transition, no Effect.
    let out = m.handle(AppIntent::MouseButton {
        window: wk(1),
        pressed: true,
        button: MouseButton::Left,
        mods: ModKey::empty(),
        pos: lp(5.0, 5.0),
    });
    assert!(out.is_empty(), "repeated press should be no-op, got {:?}", out.as_slice());
    // Position still tracked.
    assert_eq!(m.state().last_mouse_pos, Some(lp(5.0, 5.0)));
}

#[test]
fn mouse_right_button_emits_user_input() {
    let mut m = sm();
    // Right click — not selection; emits Render(UserInput) for
    // paste / context affordance repaint.
    let out = m.handle(AppIntent::MouseButton {
        window: wk(1),
        pressed: true,
        button: MouseButton::Right,
        mods: ModKey::empty(),
        pos: lp(40.0, 50.0),
    });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::UserInput);
    // Right button does NOT touch mouse_left_down.
    assert!(!m.state().mouse_left_down);
}

#[test]
fn mouse_move_emits_render_hover_on_position_change() {
    let mut m = sm();
    let out = m.handle(AppIntent::MouseMove { window: wk(1), pos: lp(1.0, 2.0) });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Hover);
    assert_eq!(m.state().last_mouse_pos, Some(lp(1.0, 2.0)));
}

#[test]
fn mouse_move_coalesces_repeated_position() {
    let mut m = sm();
    let _ = m.handle(AppIntent::MouseMove { window: wk(1), pos: lp(3.0, 4.0) });
    // Identical follow-up — coalesced to no Effect.
    let out = m.handle(AppIntent::MouseMove { window: wk(1), pos: lp(3.0, 4.0) });
    assert!(out.is_empty(), "duplicate MouseMove should coalesce, got {:?}", out.as_slice());

    // A genuinely different position emits again.
    let out = m.handle(AppIntent::MouseMove { window: wk(1), pos: lp(3.0, 5.0) });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Hover);
}

#[test]
fn mouse_button_updates_last_pos_even_when_no_transition() {
    let mut m = sm();
    // Fresh state: mouse_left_down=false. Button-up=false → no transition,
    // no Effect emitted, but position still recorded.
    let out = m.handle(AppIntent::MouseButton {
        window: wk(1),
        pressed: false,
        button: MouseButton::Left,
        mods: ModKey::empty(),
        pos: lp(7.0, 8.0),
    });
    assert!(out.is_empty());
    assert_eq!(m.state().last_mouse_pos, Some(lp(7.0, 8.0)));
    assert!(!m.state().mouse_left_down);
}
