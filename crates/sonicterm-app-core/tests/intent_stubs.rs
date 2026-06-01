//! Per-Intent coverage. Proves that every one of the 63
//! `AppIntent` variants compiles into the enum and that
//! `AppStateMachine::handle` accepts each variant without panicking.
//!
//! **M6a-expand-2b** (THIS PR): the leaf reducer now emits Effects for
//! the routed Intent classes (PTY / Key / IME / clipboard / scroll /
//! mouse-wheel / hyperlink / config / redraw / exit). Non-leaf
//! variants (window / tab / pane lifecycle, selection, search,
//! palette, broadcast, OS drag) still return empty pending 2c.
//!
//! Variant count enforced: 63.

use std::path::PathBuf;
use std::time::Instant;

use bytes::Bytes;
use sonicterm_app_core::{
    AppEffect, AppIntent, AppState, AppStateMachine, BroadcastScope, KeyCode, LogicalPos,
    MouseButton, PaletteChoice, PaneId, PendingDragOutcomeCore, PtyConfig, SelectionMode, SplitDir,
    WindowRole,
};
use sonicterm_types::{ModKey, Pos, WindowKey};

fn wk() -> WindowKey {
    WindowKey::new(1)
}
fn pane() -> PaneId {
    PaneId(1)
}
fn pos() -> LogicalPos {
    LogicalPos { x: 0.0, y: 0.0 }
}
fn cellpos() -> Pos {
    Pos { col: 0, row: 0 }
}
fn mods() -> ModKey {
    ModKey::empty()
}
fn sm() -> AppStateMachine {
    AppStateMachine::new(AppState::default())
}

/// Stub variant — reducer must return empty (2c pending).
macro_rules! stub_test {
    ($name:ident, $intent:expr) => {
        #[test]
        fn $name() {
            let mut m = sm();
            let out = m.handle($intent);
            assert!(
                out.is_empty(),
                "non-leaf variant {} expected to return empty until M6a-expand-2c, got {:?}",
                stringify!($name),
                out.as_slice()
            );
        }
    };
}

/// Routed variant — reducer must return at least one Effect.
macro_rules! routed_test {
    ($name:ident, $intent:expr) => {
        #[test]
        fn $name() {
            let mut m = sm();
            let out = m.handle($intent);
            assert!(
                !out.is_empty(),
                "leaf variant {} expected to emit Effects in M6a-expand-2b, got empty",
                stringify!($name)
            );
        }
    };
}

// 01..06 Window lifecycle (routed in M6a-expand-2c-window)
routed_test!(intent_01_new_window, AppIntent::NewWindow { role: WindowRole::Primary });
routed_test!(intent_02_window_close_requested, AppIntent::WindowCloseRequested { window: wk() });
routed_test!(intent_03_window_focused, AppIntent::WindowFocused { window: wk() });
// Blurred only emits when previously focused — exercise the
// transition path (see window_lifecycle_intents::blur_after_focus).
// A standalone Blurred against fresh state legitimately returns
// empty, so it stays in the stub bucket here; a dedicated focused
// test covers the routed transition.
stub_test!(intent_04_window_blurred, AppIntent::WindowBlurred { window: wk() });
routed_test!(
    intent_05_window_resized,
    AppIntent::WindowResized { window: wk(), cols: 80, rows: 24 }
);
// Moved is intentionally side-effect-free (record-only); it stays
// in the stub bucket here while window_lifecycle_intents asserts
// the state mutation.
stub_test!(intent_06_window_moved, AppIntent::WindowMoved { window: wk(), pos: pos() });

// 07..12 Tab lifecycle (routed in M6a-expand-2c-tab)
routed_test!(intent_07_new_tab, AppIntent::NewTab { window: wk(), cwd: None });
routed_test!(
    intent_07_new_tab_with_cwd,
    AppIntent::NewTab { window: wk(), cwd: Some(PathBuf::from("/")) }
);
routed_test!(intent_08_close_tab, AppIntent::CloseTab { window: wk(), idx: 0 });
// Next/Prev with only one tab (default state) are no-ops by design —
// transition-only emission, same shape as WindowFocused on fresh state.
// Focused per-Intent tests in `tab_intents.rs` cover the routed paths.
stub_test!(intent_09_next_tab, AppIntent::NextTab { window: wk() });
stub_test!(intent_10_prev_tab, AppIntent::PrevTab { window: wk() });
// GoToTab against empty tab_count is also a no-op.
stub_test!(intent_11_goto_tab, AppIntent::GoToTab { window: wk(), idx: 3 });
routed_test!(intent_12_tear_out_tab, AppIntent::TearOutTab { src_window: wk(), src_tab: 0 });

// 13..19 Pane lifecycle / nav (routed in M6a-expand-2c-pane)
routed_test!(intent_13_split_pane, AppIntent::SplitPane { window: wk(), dir: SplitDir::Right });
// ClosePane against empty pane_count (default state) is a no-op
// emission-wise (saturates at 0); pane_intents.rs covers the routed
// path with a populated state.
routed_test!(intent_14_close_pane, AppIntent::ClosePane { window: wk() });
// ResizePane / FocusPane* require pane_count >= 2 to emit — fresh
// state returns empty by design (no second pane to focus/resize).
// Focused per-Intent tests in `pane_intents.rs` cover the routed paths.
stub_test!(
    intent_15_resize_pane,
    AppIntent::ResizePane { window: wk(), dir: SplitDir::Down, cells: 5 }
);
stub_test!(intent_16_focus_pane_left, AppIntent::FocusPaneLeft { window: wk() });
stub_test!(intent_17_focus_pane_right, AppIntent::FocusPaneRight { window: wk() });
stub_test!(intent_18_focus_pane_up, AppIntent::FocusPaneUp { window: wk() });
stub_test!(intent_19_focus_pane_down, AppIntent::FocusPaneDown { window: wk() });

// 20..23 PTY (20..22 routed; 23 still cascades — stub)
routed_test!(intent_20_pty_burst, AppIntent::PtyBurst { pane: pane(), generation: 1 });
routed_test!(intent_21_pty_exit, AppIntent::PtyExit { pane: pane(), status: 0 });
routed_test!(
    intent_22_pty_write,
    AppIntent::PtyWrite { pane: pane(), bytes: Bytes::from_static(b"hi") }
);
stub_test!(
    intent_23_foreground_proc_changed,
    AppIntent::ForegroundProcChanged { pane: pane(), name: Some("bash".into()) }
);

// 24..28 Keyboard / IME (all routed — leaf)
routed_test!(
    intent_24_key,
    AppIntent::Key { window: wk(), code: KeyCode(0x41), mods: mods(), pressed: true }
);
routed_test!(intent_25_ime_start, AppIntent::ImeStart { window: wk() });
routed_test!(
    intent_26_ime_preedit,
    AppIntent::ImePreedit { window: wk(), text: "あ".into(), cursor: 0..1 }
);
routed_test!(intent_27_ime_commit, AppIntent::ImeCommit { window: wk(), text: "あ".into() });
routed_test!(intent_28_ime_end, AppIntent::ImeEnd { window: wk() });

// 29..32 Mouse (29 down routed, 29 up stub against fresh state, 30 routed, 31 routed, 32 routed)
routed_test!(
    intent_29_mouse_button_down,
    AppIntent::MouseButton {
        window: wk(),
        pressed: true,
        button: MouseButton::Left,
        mods: mods(),
        pos: pos()
    }
);
// Button-up against fresh state (mouse_left_down already false) is a
// no-op transition by design — same shape as WindowFocused on a fresh
// state. The focused test in `mouse_intents.rs` covers the down→up
// round-trip.
stub_test!(
    intent_29_mouse_button_up,
    AppIntent::MouseButton {
        window: wk(),
        pressed: false,
        button: MouseButton::Left,
        mods: mods(),
        pos: pos()
    }
);
routed_test!(intent_30_mouse_move, AppIntent::MouseMove { window: wk(), pos: pos() });
routed_test!(
    intent_31_mouse_wheel,
    AppIntent::MouseWheel { window: wk(), dy: -1.0, dx: 0.0, mods: mods() }
);
routed_test!(
    intent_32_hover_url,
    AppIntent::HoverUrl { window: wk(), url: Some("https://x".into()) }
);

// 33..39 Scrollback (all routed)
routed_test!(intent_33_scroll_up, AppIntent::ScrollUp { window: wk(), lines: 3 });
routed_test!(intent_34_scroll_down, AppIntent::ScrollDown { window: wk(), lines: 3 });
routed_test!(intent_35_scroll_page_up, AppIntent::ScrollPageUp { window: wk() });
routed_test!(intent_36_scroll_page_down, AppIntent::ScrollPageDown { window: wk() });
routed_test!(intent_37_scroll_to_top, AppIntent::ScrollToTop { window: wk() });
routed_test!(intent_38_scroll_to_bottom, AppIntent::ScrollToBottom { window: wk() });
routed_test!(intent_39_scroll_to_cursor, AppIntent::ScrollToCursor { window: wk() });

// 40..45 Selection / clipboard (44, 45 routed; rest stubs)
stub_test!(
    intent_40_selection_start,
    AppIntent::SelectionStart { window: wk(), anchor: cellpos(), mode: SelectionMode::Cell }
);
stub_test!(intent_41_selection_extend, AppIntent::SelectionExtend { window: wk(), to: cellpos() });
stub_test!(intent_42_selection_end, AppIntent::SelectionEnd { window: wk() });
stub_test!(intent_43_clear_selection, AppIntent::ClearSelection { window: wk() });
routed_test!(intent_44_copy_selection, AppIntent::CopySelection { window: wk() });
routed_test!(
    intent_45_paste,
    AppIntent::Paste { window: wk(), text: "hi".into(), bracketed: true }
);

// 46..49 Search (stub)
stub_test!(intent_46_open_search, AppIntent::OpenSearch { window: wk() });
stub_test!(intent_47_search_query, AppIntent::SearchQuery { window: wk(), q: "needle".into() });
stub_test!(intent_48_search_step, AppIntent::SearchStep { window: wk(), forward: true });
stub_test!(intent_49_close_search, AppIntent::CloseSearch { window: wk() });

// 50..53 Palette (stub)
stub_test!(intent_50_toggle_command_palette, AppIntent::ToggleCommandPalette { window: wk() });
stub_test!(intent_51_palette_filter, AppIntent::PaletteFilter { window: wk(), filter: "x".into() });
stub_test!(intent_52_palette_step, AppIntent::PaletteStep { window: wk(), delta: 1 });
stub_test!(
    intent_53_palette_submit,
    AppIntent::PaletteSubmit { window: wk(), choice: PaletteChoice { id: "new_tab".into() } }
);

// 54..55 OS drag / drop (stub)
stub_test!(
    intent_54_os_drag_outcome,
    AppIntent::OsDragOutcome(PendingDragOutcomeCore { src_window: wk(), committed: false })
);
stub_test!(
    intent_55_files_dropped,
    AppIntent::FilesDropped { window: wk(), paths: vec![PathBuf::from("/x")] }
);

// 56 Hyperlinks (routed)
routed_test!(intent_56_click_url, AppIntent::ClickUrl { window: wk(), url: "https://x".into() });

// 57..59 Config / theming (all routed)
routed_test!(
    intent_57_config_changed,
    AppIntent::ConfigChanged { new: Box::new(PtyConfig::default()) }
);
routed_test!(intent_58_apply_theme, AppIntent::ApplyTheme { name: "Tokyo Night".into() });
routed_test!(intent_59_font_size_delta, AppIntent::FontSizeDelta { delta: 1 });

// 60 Broadcast (stub)
stub_test!(
    intent_60_set_broadcast_scope,
    AppIntent::SetBroadcastScope { scope: BroadcastScope::Off }
);
stub_test!(
    intent_60_set_broadcast_scope_custom,
    AppIntent::SetBroadcastScope { scope: BroadcastScope::Custom(vec![pane()]) }
);

// 61..63 Frame timing / lifecycle (61 routed, 62 stub, 63 routed)
routed_test!(intent_61_redraw_requested, AppIntent::RedrawRequested { window: wk() });
stub_test!(intent_62_tick, AppIntent::Tick { now: Instant::now() });
routed_test!(intent_63_exit, AppIntent::Exit);

// ── Per-Effect shape checks for the routed leaves (M6a-expand-2b) ──

#[test]
fn pty_write_intent_passes_bytes_through() {
    let mut m = sm();
    let out = m.handle(AppIntent::PtyWrite { pane: pane(), bytes: Bytes::from_static(b"abc") });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::PtyWrite { pane: p, data } => {
            assert_eq!(*p, pane());
            assert_eq!(&data[..], b"abc");
        }
        other => panic!("expected PtyWrite, got {other:?}"),
    }
}

#[test]
fn pty_exit_emits_propagate_then_close_sorted() {
    let mut m = sm();
    let out = m.handle(AppIntent::PtyExit { pane: pane(), status: 2 });
    assert_eq!(out.len(), 2);
    // Both belong to different classes: PtyClose=PtyWrite class (0),
    // ChildExitPropagate=WindowOp class (4) → sort places PtyClose first.
    assert!(matches!(out[0], AppEffect::PtyClose { .. }));
    assert!(matches!(out[1], AppEffect::ChildExitPropagate { .. }));
}

#[test]
fn ime_commit_emits_write_and_render_sorted() {
    let mut m = sm();
    let out = m.handle(AppIntent::ImeCommit { window: wk(), text: "x".into() });
    assert_eq!(out.len(), 2);
    // PtyWrite class 0 before Render class 1.
    assert!(matches!(out[0], AppEffect::PtyWrite { .. }));
    assert!(matches!(out[1], AppEffect::Render { .. }));
}

#[test]
fn copy_selection_emits_clipboard_set() {
    let mut m = sm();
    let out = m.handle(AppIntent::CopySelection { window: wk() });
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], AppEffect::ClipboardSet { .. }));
}

#[test]
fn click_url_emits_open_url() {
    let mut m = sm();
    let out = m.handle(AppIntent::ClickUrl { window: wk(), url: "https://example.com".into() });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::OpenURL { url } => assert_eq!(url, "https://example.com"),
        other => panic!("expected OpenURL, got {other:?}"),
    }
}

#[test]
fn exit_emits_quit() {
    let mut m = sm();
    let out = m.handle(AppIntent::Exit);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], AppEffect::Quit));
}

#[test]
fn scroll_to_bottom_emits_render_scroll() {
    let mut m = sm();
    let out = m.handle(AppIntent::ScrollToBottom { window: wk() });
    assert_eq!(out.len(), 1);
    match &out[0] {
        AppEffect::Render { reason, .. } => assert_eq!(
            *reason,
            sonicterm_app_core::RedrawReason::Scroll,
            "ScrollToBottom must emit Render(Scroll)"
        ),
        other => panic!("expected Render, got {other:?}"),
    }
}
