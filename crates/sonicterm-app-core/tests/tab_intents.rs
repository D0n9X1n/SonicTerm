//! Focused per-Intent tests for the tab-lifecycle reducer arms
//! introduced in M6a-expand-2c-tab.
//!
//! For each Intent we assert (a) the Effect batch shape (class-sorted,
//! correct variant + payload), and (b) the `AppState` mutation —
//! exactly what the FINAL spec §3 mapping promises and what the
//! boundary in `sonicterm-app::app::*` relies on.

use sonicterm_app_core::{AppEffect, AppIntent, AppState, AppStateMachine, RedrawReason};
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
fn new_tab_bumps_count_and_emits_tab_added() {
    let mut m = sm();
    let out = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    assert_eq!(out.len(), 1, "got {:?}", out.as_slice());
    assert_render(&out[0], wk(1), RedrawReason::TabAdded);
    assert_eq!(m.state().tab_count, 1);
    assert_eq!(m.state().active_tab_idx, Some(0));

    // Second tab — count 2, active 1.
    let out = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    assert_eq!(out.len(), 1);
    assert_eq!(m.state().tab_count, 2);
    assert_eq!(m.state().active_tab_idx, Some(1));
}

#[test]
fn close_tab_decrements_count_and_emits_tab_removed() {
    let mut m = sm();
    let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    assert_eq!(m.state().tab_count, 3);
    assert_eq!(m.state().active_tab_idx, Some(2));

    // Close the active (last) tab — active shifts down to 1.
    let out = m.handle(AppIntent::CloseTab { window: wk(1), idx: 2 });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::TabRemoved);
    assert_eq!(m.state().tab_count, 2);
    assert_eq!(m.state().active_tab_idx, Some(1));

    // Close index 0 — active was at 1, shifts down to 0.
    let _ = m.handle(AppIntent::CloseTab { window: wk(1), idx: 0 });
    assert_eq!(m.state().tab_count, 1);
    assert_eq!(m.state().active_tab_idx, Some(0));
}

#[test]
fn next_tab_wraps_and_emits_tab_switch() {
    let mut m = sm();
    for _ in 0..3 {
        let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    }
    // Active is 2 after three NewTab calls.
    assert_eq!(m.state().active_tab_idx, Some(2));

    let out = m.handle(AppIntent::NextTab { window: wk(1) });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::TabSwitch);
    assert_eq!(m.state().active_tab_idx, Some(0), "next from last wraps to 0");

    let _ = m.handle(AppIntent::NextTab { window: wk(1) });
    assert_eq!(m.state().active_tab_idx, Some(1));
}

#[test]
fn prev_tab_wraps_and_emits_tab_switch() {
    let mut m = sm();
    for _ in 0..3 {
        let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    }
    assert_eq!(m.state().active_tab_idx, Some(2));

    let out = m.handle(AppIntent::PrevTab { window: wk(1) });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::TabSwitch);
    assert_eq!(m.state().active_tab_idx, Some(1));

    // From 0, prev wraps to last (2).
    let _ = m.handle(AppIntent::PrevTab { window: wk(1) });
    let _ = m.handle(AppIntent::PrevTab { window: wk(1) });
    assert_eq!(m.state().active_tab_idx, Some(2));
}

#[test]
fn next_prev_with_single_tab_is_noop() {
    let mut m = sm();
    let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    // Only one tab — Next/Prev are no-ops (no Render).
    let out = m.handle(AppIntent::NextTab { window: wk(1) });
    assert!(out.is_empty(), "single-tab NextTab should be no-op, got {:?}", out.as_slice());
    let out = m.handle(AppIntent::PrevTab { window: wk(1) });
    assert!(out.is_empty(), "single-tab PrevTab should be no-op, got {:?}", out.as_slice());
    assert_eq!(m.state().active_tab_idx, Some(0));
}

#[test]
fn goto_tab_emits_on_transition_and_clamps_oor() {
    let mut m = sm();
    for _ in 0..3 {
        let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    }
    // Active is 2 — going to 0 is a real transition.
    let out = m.handle(AppIntent::GoToTab { window: wk(1), idx: 0 });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::TabSwitch);
    assert_eq!(m.state().active_tab_idx, Some(0));

    // Going to the same index again — no-op.
    let out = m.handle(AppIntent::GoToTab { window: wk(1), idx: 0 });
    assert!(out.is_empty(), "no-op GoToTab should emit nothing, got {:?}", out.as_slice());

    // Out-of-range clamps to last valid (2) — emits transition.
    let out = m.handle(AppIntent::GoToTab { window: wk(1), idx: 99 });
    assert_eq!(out.len(), 1);
    assert_render(&out[0], wk(1), RedrawReason::TabSwitch);
    assert_eq!(m.state().active_tab_idx, Some(2));
}

#[test]
fn tear_out_tab_decrements_source_count_and_emits_removed() {
    let mut m = sm();
    for _ in 0..2 {
        let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    }
    assert_eq!(m.state().tab_count, 2);
    assert_eq!(m.state().active_tab_idx, Some(1));

    let out = m.handle(AppIntent::TearOutTab { src_window: wk(1), src_tab: 1 });
    // 2c-misc: cascade now emits Render(TabRemoved) + WindowOpen
    // (the destination window for the torn-out tab).
    assert_eq!(out.len(), 2);
    assert_render(&out[0], wk(1), RedrawReason::TabRemoved);
    assert!(matches!(out[1], sonicterm_app_core::AppEffect::WindowOpen { .. }));
    assert_eq!(m.state().tab_count, 1);
    // Active was 1 (the torn-out one); reducer clamps down to 0.
    assert_eq!(m.state().active_tab_idx, Some(0));
}

#[test]
fn close_tab_below_active_shifts_active_down() {
    let mut m = sm();
    for _ in 0..3 {
        let _ = m.handle(AppIntent::NewTab { window: wk(1), cwd: None });
    }
    assert_eq!(m.state().active_tab_idx, Some(2));
    // Close idx 0 (below active 2) — active shifts to 1.
    let _ = m.handle(AppIntent::CloseTab { window: wk(1), idx: 0 });
    assert_eq!(m.state().active_tab_idx, Some(1));
    assert_eq!(m.state().tab_count, 2);
}
