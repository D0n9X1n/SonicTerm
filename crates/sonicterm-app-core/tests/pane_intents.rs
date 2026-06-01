//! Focused per-Intent tests for the pane-lifecycle reducer arms
//! introduced in M6a-expand-2c-pane.
//!
//! Mirrors the shape of `tab_intents.rs`: each Intent gets a state-
//! mutation + Effect-shape check against the FINAL spec §3 mapping.
//!
//! The reducer tracks a flat `pane_count` + `focused_pane_idx` pair;
//! the boundary's pane-tree in `sonicterm-app::app::WindowState
//! .tab_states[..].tree` remains source-of-truth for actual geometry.
//! These tests verify the observability surface only.

use sonicterm_app_core::{AppEffect, AppIntent, AppState, AppStateMachine, RedrawReason, SplitDir};
use sonicterm_types::WindowKey;

fn wk(n: u64) -> WindowKey {
    WindowKey::new(n)
}

fn sm() -> AppStateMachine {
    AppStateMachine::new(AppState::default())
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
fn split_pane_bumps_count_and_focuses_new_leaf() {
    let mut m = sm();
    let out = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Right });
    assert_eq!(out.len(), 1, "got {:?}", out.as_slice());
    assert_render(&out[0], wk(1), RedrawReason::Layout);
    assert_eq!(m.state().pane_count, 1);
    assert_eq!(m.state().focused_pane_idx, Some(0));

    // Second split — count 2, focus shifts to new leaf at idx 1.
    let out = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Down });
    assert_eq!(out.len(), 1);
    assert_eq!(m.state().pane_count, 2);
    assert_eq!(m.state().focused_pane_idx, Some(1));
}

#[test]
fn close_pane_decrements_and_clamps_focus() {
    let mut m = sm();
    for _ in 0..3 {
        let _ = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Right });
    }
    assert_eq!(m.state().pane_count, 3);
    assert_eq!(m.state().focused_pane_idx, Some(2));

    // Close — count drops to 2, focus clamps to last valid (1).
    let out = m.handle(AppIntent::ClosePane { window: wk(1) });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Layout);
    assert_eq!(m.state().pane_count, 2);
    assert_eq!(m.state().focused_pane_idx, Some(1));

    // Close remaining two — count 0, focus cleared.
    let _ = m.handle(AppIntent::ClosePane { window: wk(1) });
    let _ = m.handle(AppIntent::ClosePane { window: wk(1) });
    assert_eq!(m.state().pane_count, 0);
    assert_eq!(m.state().focused_pane_idx, None);
}

#[test]
fn close_pane_on_empty_state_saturates() {
    let mut m = sm();
    // Default state has pane_count = 0; ClosePane must saturate and
    // still emit Render(Layout) so the boundary can re-paint after
    // its own tree mutation.
    let out = m.handle(AppIntent::ClosePane { window: wk(1) });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Layout);
    assert_eq!(m.state().pane_count, 0);
}

#[test]
fn resize_pane_emits_layout_only_when_multi_pane() {
    let mut m = sm();
    // Single pane — Resize is a no-op (no second pane to grow against).
    let _ = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Right });
    let out = m.handle(AppIntent::ResizePane { window: wk(1), dir: SplitDir::Right, cells: 5 });
    assert!(out.is_empty(), "single-pane Resize should be no-op, got {:?}", out.as_slice());

    // Add a second pane — Resize now emits Render(Layout).
    let _ = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Down });
    let out = m.handle(AppIntent::ResizePane { window: wk(1), dir: SplitDir::Up, cells: 3 });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::Layout);
    // Resize doesn't touch count/focus.
    assert_eq!(m.state().pane_count, 2);
    assert_eq!(m.state().focused_pane_idx, Some(1));
}

#[test]
fn focus_pane_directions_emit_focus_when_multi_pane() {
    let mut m = sm();
    // No panes — FocusPane* is a no-op.
    let out = m.handle(AppIntent::FocusPaneLeft { window: wk(1) });
    assert!(out.is_empty());

    // Two panes — each direction emits Render(Focus).
    for _ in 0..2 {
        let _ = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Right });
    }
    for intent in [
        AppIntent::FocusPaneLeft { window: wk(1) },
        AppIntent::FocusPaneRight { window: wk(1) },
        AppIntent::FocusPaneUp { window: wk(1) },
        AppIntent::FocusPaneDown { window: wk(1) },
    ] {
        let out = m.handle(intent);
        assert_eq!(out.len(), 1);
        assert_render(&out[0], wk(1), RedrawReason::Focus);
    }
}

#[test]
fn focus_pane_single_pane_is_noop() {
    let mut m = sm();
    let _ = m.handle(AppIntent::SplitPane { window: wk(1), dir: SplitDir::Right });
    // Only one pane — directional focus emits nothing.
    let out = m.handle(AppIntent::FocusPaneRight { window: wk(1) });
    assert!(out.is_empty(), "single-pane FocusPaneRight should be no-op, got {:?}", out.as_slice());
    assert_eq!(m.state().focused_pane_idx, Some(0));
}
