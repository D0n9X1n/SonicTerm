//! Focused per-Intent tests for the M6a-expand-2c-misc batch:
//! ForegroundProcChanged, Selection*, Search*, Palette*, OsDragOutcome,
//! SetBroadcastScope, and the TearOutTab cascade.
//!
//! Routing call sites land in:
//!   - sonicterm-app/src/app/overlays.rs       (palette + search)
//!   - sonicterm-app/src/app/tear_out.rs       (TearOutTab cascade)
//!   - sonicterm-app/src/app/os_drag.rs        (OsDragOutcome)
//!   - sonicterm-app/src/app/config_apply.rs   (ApplyTheme; routed in 2b)

use sonicterm_app_core::{
    AppEffect, AppIntent, AppState, AppStateMachine, BroadcastScope, PaletteChoice, PaneId,
    PendingDragOutcomeCore, RedrawReason, SelectionMode,
};
use sonicterm_types::{Pos, WindowKey};

fn wk() -> WindowKey {
    WindowKey::new(1)
}
fn sm() -> AppStateMachine {
    AppStateMachine::new(AppState::default())
}
fn anchor() -> Pos {
    Pos { col: 0, row: 0 }
}

// ── ForegroundProcChanged ────────────────────────────────────────────

#[test]
fn fg_proc_changed_emits_title_or_tab_on_first_set() {
    let mut m = sm();
    let out =
        m.handle(AppIntent::ForegroundProcChanged { pane: PaneId(1), name: Some("vim".into()) });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::Render { reason, .. } => assert_eq!(*reason, RedrawReason::TitleOrTab),
        other => panic!("expected Render(TitleOrTab), got {other:?}"),
    }
}

#[test]
fn fg_proc_changed_dedupes_repeated_same_name() {
    let mut m = sm();
    let _ =
        m.handle(AppIntent::ForegroundProcChanged { pane: PaneId(1), name: Some("vim".into()) });
    let out =
        m.handle(AppIntent::ForegroundProcChanged { pane: PaneId(1), name: Some("vim".into()) });
    assert!(out.is_empty(), "repeated same name must be a no-op");
}

// ── Selection ────────────────────────────────────────────────────────

#[test]
fn selection_start_extend_end_emits_render_each() {
    let mut m = sm();
    let s1 = m.handle(AppIntent::SelectionStart {
        window: wk(),
        anchor: anchor(),
        mode: SelectionMode::Cell,
    });
    assert_eq!(s1.len(), 1);
    let s2 = m.handle(AppIntent::SelectionExtend { window: wk(), to: anchor() });
    assert_eq!(s2.len(), 1);
    let s3 = m.handle(AppIntent::SelectionEnd { window: wk() });
    assert_eq!(s3.len(), 1);
    // After End, Extend is a no-op again.
    let s4 = m.handle(AppIntent::SelectionExtend { window: wk(), to: anchor() });
    assert!(s4.is_empty());
}

#[test]
fn clear_selection_after_start_emits_and_resets() {
    let mut m = sm();
    let _ = m.handle(AppIntent::SelectionStart {
        window: wk(),
        anchor: anchor(),
        mode: SelectionMode::Word,
    });
    let out = m.handle(AppIntent::ClearSelection { window: wk() });
    assert_eq!(out.len(), 1);
    // Second Clear is a no-op.
    let out2 = m.handle(AppIntent::ClearSelection { window: wk() });
    assert!(out2.is_empty());
}

// ── Search overlay ───────────────────────────────────────────────────

#[test]
fn open_search_then_query_then_close_round_trip() {
    let mut m = sm();
    let open = m.handle(AppIntent::OpenSearch { window: wk() });
    assert_eq!(open.len(), 1);
    assert!(matches!(open[0], AppEffect::Render { reason: RedrawReason::Overlay, .. }));
    let q = m.handle(AppIntent::SearchQuery { window: wk(), q: "x".into() });
    assert_eq!(q.len(), 1);
    let step = m.handle(AppIntent::SearchStep { window: wk(), forward: true });
    assert_eq!(step.len(), 1);
    let close = m.handle(AppIntent::CloseSearch { window: wk() });
    assert_eq!(close.len(), 1);
    // After close, Query is a no-op again.
    let q2 = m.handle(AppIntent::SearchQuery { window: wk(), q: "y".into() });
    assert!(q2.is_empty());
}

#[test]
fn open_search_when_already_open_dedupes() {
    let mut m = sm();
    let _ = m.handle(AppIntent::OpenSearch { window: wk() });
    let out = m.handle(AppIntent::OpenSearch { window: wk() });
    assert!(out.is_empty(), "double-open must dedupe via transition-guard");
}

// ── Command palette ──────────────────────────────────────────────────

#[test]
fn palette_toggle_filter_submit_round_trip() {
    let mut m = sm();
    let t1 = m.handle(AppIntent::ToggleCommandPalette { window: wk() });
    assert_eq!(t1.len(), 1);
    let f = m.handle(AppIntent::PaletteFilter { window: wk(), filter: "tab".into() });
    assert_eq!(f.len(), 1);
    let s = m.handle(AppIntent::PaletteStep { window: wk(), delta: 1 });
    assert_eq!(s.len(), 1);
    let submit = m.handle(AppIntent::PaletteSubmit {
        window: wk(),
        choice: PaletteChoice { id: "new_tab".into() },
    });
    assert_eq!(submit.len(), 1);
    // After Submit the palette is closed; Filter is a no-op now.
    let f2 = m.handle(AppIntent::PaletteFilter { window: wk(), filter: "x".into() });
    assert!(f2.is_empty());
}

#[test]
fn palette_toggle_twice_returns_to_closed() {
    let mut m = sm();
    let _ = m.handle(AppIntent::ToggleCommandPalette { window: wk() });
    let _ = m.handle(AppIntent::ToggleCommandPalette { window: wk() });
    assert!(!m.state().palette_open);
}

// ── OS drag outcome ──────────────────────────────────────────────────

#[test]
fn os_drag_outcome_emits_drag_end() {
    let mut m = sm();
    let out = m.handle(AppIntent::OsDragOutcome(PendingDragOutcomeCore {
        src_window: wk(),
        committed: true,
    }));
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::OsDragEnd { src_window, committed } => {
            assert_eq!(*src_window, wk());
            assert!(*committed);
        }
        other => panic!("expected OsDragEnd, got {other:?}"),
    }
}

// ── Broadcast scope ──────────────────────────────────────────────────

#[test]
fn set_broadcast_scope_emits_title_or_tab_on_change() {
    let mut m = sm();
    let out = m.handle(AppIntent::SetBroadcastScope { scope: BroadcastScope::CurrentTab });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::Render { reason, .. } => assert_eq!(*reason, RedrawReason::TitleOrTab),
        other => panic!("expected Render(TitleOrTab), got {other:?}"),
    }
    // No-op set dedupes.
    let out2 = m.handle(AppIntent::SetBroadcastScope { scope: BroadcastScope::CurrentTab });
    assert!(out2.is_empty());
}

// ── TearOutTab cascade ───────────────────────────────────────────────

#[test]
fn tear_out_tab_emits_tab_removed_and_window_open_cascade() {
    let mut m = sm();
    // Seed: two tabs, active = 1.
    let _ = m.handle(AppIntent::NewTab { window: wk(), cwd: None });
    let _ = m.handle(AppIntent::NewTab { window: wk(), cwd: None });
    assert_eq!(m.state().tab_count, 2);

    let out = m.handle(AppIntent::TearOutTab { src_window: wk(), src_tab: 1 });
    // Render(TabRemoved) (class Render=1) + WindowOpen (class WindowOp=4),
    // sorted by class.
    assert_eq!(out.len(), 2, "tear-out emits both halves of the cascade");
    assert!(matches!(out[0], AppEffect::Render { reason: RedrawReason::TabRemoved, .. }));
    assert!(matches!(out[1], AppEffect::WindowOpen { .. }));

    assert_eq!(m.state().tab_count, 1, "source window loses one tab");
    assert_eq!(m.state().live_window_count, 1, "destination window now alive");
}

// ── FilesDropped + Tick stay record-only (sanity) ───────────────────

#[test]
fn files_dropped_is_record_only() {
    let mut m = sm();
    let out = m.handle(AppIntent::FilesDropped { window: wk(), paths: vec![] });
    assert!(out.is_empty());
}

#[test]
fn tick_is_clock_only() {
    let mut m = sm();
    let out = m.handle(AppIntent::Tick { now: std::time::Instant::now() });
    assert!(out.is_empty());
}

// ── Additional dedupe / state-tracking sanity ────────────────────────

#[test]
fn fg_proc_changed_clear_to_none_after_set_emits() {
    let mut m = sm();
    let _ =
        m.handle(AppIntent::ForegroundProcChanged { pane: PaneId(1), name: Some("vim".into()) });
    let out = m.handle(AppIntent::ForegroundProcChanged { pane: PaneId(1), name: None });
    assert_eq!(out.len(), 1, "transitioning back to None is a real change");
}

#[test]
fn selection_active_flag_tracks_state() {
    let mut m = sm();
    assert!(!m.state().selection_active);
    let _ = m.handle(AppIntent::SelectionStart {
        window: wk(),
        anchor: anchor(),
        mode: SelectionMode::Cell,
    });
    assert!(m.state().selection_active);
    let _ = m.handle(AppIntent::SelectionEnd { window: wk() });
    assert!(!m.state().selection_active);
}

#[test]
fn search_and_palette_open_flags_track_state() {
    let mut m = sm();
    let _ = m.handle(AppIntent::OpenSearch { window: wk() });
    assert!(m.state().search_open);
    let _ = m.handle(AppIntent::CloseSearch { window: wk() });
    assert!(!m.state().search_open);

    let _ = m.handle(AppIntent::ToggleCommandPalette { window: wk() });
    assert!(m.state().palette_open);
    let _ = m.handle(AppIntent::PaletteSubmit {
        window: wk(),
        choice: PaletteChoice { id: "x".into() },
    });
    assert!(!m.state().palette_open);
}

#[test]
fn broadcast_scope_state_tracks_observed_value() {
    let mut m = sm();
    let scope = BroadcastScope::Custom(vec![PaneId(2), PaneId(3)]);
    let _ = m.handle(AppIntent::SetBroadcastScope { scope: scope.clone() });
    assert_eq!(m.state().broadcast_scope, scope);
}
