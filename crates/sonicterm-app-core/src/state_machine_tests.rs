//! Behavior tests for the `AppStateMachine` dispatch boundary.
//!
//! Where `reducer_tests.rs` reads the reducer's *raw* Effect batch,
//! these drive Intents through the public `handle` path and assert the
//! spec §6 class-sort contract, the cascade-bound `drain_pending`
//! draining/sorting, and that state mutations are observable through
//! `state()`.

use super::*;

use std::time::Instant;

use sonicterm_types::WindowKey;

use crate::effect::{EffectClass, LogLevel};
use crate::intent::RedrawReason;
use crate::supporting::{MenuModel, PaneId, WindowRole};

fn wk(id: u64) -> WindowKey {
    WindowKey::new(id)
}

// ── effect-class ordering through the dispatch boundary ─────────────

#[test]
fn handle_sorts_child_exit_cascade_pty_before_window_op() {
    // The reducer pushes ChildExitPropagate (WindowOp, class 4) *before*
    // PtyClose (PtyWrite, class 0). `handle` must reorder so the shell
    // sees the shell-side close first (spec §6: PtyWrite < WindowOp).
    let mut sm = AppStateMachine::new(AppState::default());
    let out = sm.handle(AppIntent::PtyExit { pane: PaneId(1), status: 0 });
    assert_eq!(out.len(), 2);
    assert!(
        matches!(&out[0], AppEffect::PtyClose { .. }),
        "PtyClose (class 0) must sort ahead of ChildExitPropagate: {out:?}",
    );
    assert!(matches!(&out[1], AppEffect::ChildExitPropagate { .. }));
    // Classes must be non-decreasing.
    assert!(out[0].effect_class() <= out[1].effect_class());
}

#[test]
fn handle_ime_commit_orders_pty_write_before_render() {
    // ImeCommit pushes PtyWrite (class 0) then Render (class 1); already
    // ascending, so the stable sort preserves it. Asserts the contract
    // holds end-to-end.
    let mut sm = AppStateMachine::new(AppState::default());
    let out = sm.handle(AppIntent::ImeCommit { window: wk(1), text: "a".to_string() });
    assert_eq!(out.len(), 2);
    assert!(matches!(&out[0], AppEffect::PtyWrite { .. }));
    assert!(matches!(&out[1], AppEffect::Render { .. }));
}

#[test]
fn handle_window_resized_orders_render_before_window_op() {
    let mut sm = AppStateMachine::new(AppState::default());
    let out = sm.handle(AppIntent::WindowResized { window: wk(1), cols: 80, rows: 24 });
    assert_eq!(out.len(), 2);
    // Render (class 1) then WindowResize (WindowOp, class 4).
    assert_eq!(out[0].effect_class(), EffectClass::Render);
    assert_eq!(out[1].effect_class(), EffectClass::WindowOp);
}

#[test]
fn effect_class_order_is_canonical_across_all_seven_classes() {
    // Sanity on the ordering key itself: one representative per class,
    // deliberately scrambled, must sort into the spec §6 sequence.
    let mut batch = [
        AppEffect::LogEvent { level: LogLevel::Info, target: "t", msg: String::new() },
        AppEffect::MenubarUpdate(MenuModel::default()),
        AppEffect::Quit, // WindowOp
        AppEffect::ClipboardSet { text: String::new() },
        AppEffect::OsDragEnd { src_window: wk(1), committed: false },
        AppEffect::Render { window: wk(1), reason: RedrawReason::Vsync },
        AppEffect::PtyClose { pane: PaneId(0) },
    ];
    batch.sort_by_key(AppEffect::effect_class);
    let classes: Vec<EffectClass> = batch.iter().map(AppEffect::effect_class).collect();
    assert_eq!(
        classes,
        vec![
            EffectClass::PtyWrite,
            EffectClass::Render,
            EffectClass::OsDrag,
            EffectClass::Clipboard,
            EffectClass::WindowOp,
            EffectClass::MenubarUpdate,
            EffectClass::Log,
        ],
    );
}

// ── state observability through handle ──────────────────────────────

#[test]
fn handle_threads_state_across_calls() {
    let mut sm = AppStateMachine::new(AppState::default());
    let _ = sm.handle(AppIntent::NewWindow { role: WindowRole::Primary });
    let _ = sm.handle(AppIntent::NewWindow { role: WindowRole::Primary });
    assert_eq!(sm.state().live_window_count, 2, "counts accumulate in the owned state");

    let _ = sm.handle(AppIntent::WindowFocused { window: wk(3) });
    assert_eq!(sm.state().focused_window, Some(wk(3)));
}

#[test]
fn handle_returns_empty_for_record_only_intent() {
    let mut sm = AppStateMachine::new(AppState::default());
    let out = sm.handle(AppIntent::Tick { now: Instant::now() });
    assert!(out.is_empty(), "clock-only Tick yields no effects: {out:?}");
}

// ── drain_pending: sorted output + bounded cascade ──────────────────

#[test]
fn drain_pending_starts_empty() {
    let mut sm = AppStateMachine::new(AppState::default());
    assert!(sm.drain_pending().is_empty());
}

#[test]
fn drain_pending_sorts_by_class_and_empties_queue() {
    let mut sm = AppStateMachine::new(AppState::default());
    // Seed the internal queue out of class order. `pending` is private
    // but reachable from this descendant test module.
    sm.pending.push(AppEffect::Quit); // WindowOp (4)
    sm.pending.push(AppEffect::Render { window: wk(1), reason: RedrawReason::Vsync }); // 1
    sm.pending.push(AppEffect::PtyClose { pane: PaneId(0) }); // 0

    let drained = sm.drain_pending();
    let classes: Vec<EffectClass> = drained.iter().map(AppEffect::effect_class).collect();
    assert_eq!(
        classes,
        vec![EffectClass::PtyWrite, EffectClass::Render, EffectClass::WindowOp],
        "drain_pending must return a class-sorted batch",
    );
    // Draining a second time yields nothing (queue emptied).
    assert!(sm.drain_pending().is_empty(), "queue must be empty after a drain");
}

#[test]
fn drain_pending_at_max_cascade_depth_does_not_panic() {
    // Exactly MAX_CASCADE_DEPTH items: depth reaches the bound but never
    // exceeds it, so the drain completes normally.
    let mut sm = AppStateMachine::new(AppState::default());
    for _ in 0..MAX_CASCADE_DEPTH {
        sm.pending.push(AppEffect::Quit);
    }
    let drained = sm.drain_pending();
    assert_eq!(drained.len(), MAX_CASCADE_DEPTH);
}

#[test]
fn release_overflow_uses_the_common_class_sort() {
    fn has_clear_then_break(source: &str) -> bool {
        source
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair[0].trim() == "self.pending.clear();" && pair[1].trim() == "break;")
    }

    const SOURCE: &str = include_str!("state_machine.rs");
    assert!(!SOURCE.contains("return out;"));
    assert!(has_clear_then_break(SOURCE));
    assert!(has_clear_then_break("self.pending.clear();\r\n    break;\r\n"));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "MAX_CASCADE_DEPTH")]
fn drain_pending_over_cascade_bound_panics_in_debug() {
    // One past the bound: the debug guard trips. Release builds instead
    // log + truncate (not exercised here — that path is non-panicking).
    let mut sm = AppStateMachine::new(AppState::default());
    for _ in 0..(MAX_CASCADE_DEPTH + 1) {
        sm.pending.push(AppEffect::Quit);
    }
    let _ = sm.drain_pending();
}
