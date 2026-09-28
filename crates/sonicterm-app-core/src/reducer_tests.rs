//! Behavior tests for the per-Intent reducer arms (`reduce_leaf`).
//!
//! These exercise the reducer's *decisions*: which Effects an Intent
//! emits, the transition guards that suppress no-op renders, the
//! saturating count arithmetic, tab wrap/clamp, pane focus clamping,
//! mouse-move coalescing, and dedup surfaces. They read the raw
//! (pre-sort) Effect batch straight from `reduce_leaf` so the
//! push-order the arms produce stays observable; the class-sort
//! contract is covered separately in `state_machine_tests.rs`.

use super::*;

use bytes::Bytes;
use sonicterm_types::{ModKey, Pos, WindowKey};

use crate::intent::SelectionMode;
use crate::supporting::{
    BroadcastScope, KeyCode, LogicalPos, MouseButton, PaletteChoice, PaneId,
    PendingDragOutcomeCore, SplitDir, WindowRole,
};

// ── helpers ─────────────────────────────────────────────────────────

/// Run a single Intent through the leaf reducer and collect the raw,
/// pre-sort Effect batch.
fn run(state: &mut AppState, intent: AppIntent) -> SmallVec<[AppEffect; 4]> {
    let mut out: SmallVec<[AppEffect; 4]> = SmallVec::new();
    reduce_leaf(state, intent, &mut out);
    out
}

fn window_key(id: u64) -> WindowKey {
    WindowKey::new(id)
}

fn logical_pos(pixel_x: f64, pixel_y: f64) -> LogicalPos {
    LogicalPos { pixel_x, pixel_y }
}

/// Assert the effect is a `Render` for `window` with `reason`.
#[track_caller]
fn assert_render(effect: &AppEffect, window: WindowKey, reason: RedrawReason) {
    match effect {
        AppEffect::Render { window: actual_window, reason: actual_reason } => {
            assert_eq!(*actual_window, window, "render window mismatch");
            assert_eq!(*actual_reason, reason, "render reason mismatch");
        }
        other => panic!("expected Render, got {other:?}"),
    }
}

/// The single Effect in a one-element batch, or panic.
#[track_caller]
fn only(out: &SmallVec<[AppEffect; 4]>) -> &AppEffect {
    assert_eq!(out.len(), 1, "expected exactly one effect, got {out:?}");
    &out[0]
}

// ── PTY leaf ────────────────────────────────────────────────────────

#[test]
fn pty_write_passes_bytes_through_unchanged() {
    let mut state = AppState::default();
    let out =
        run(&mut state, AppIntent::PtyWrite { pane: PaneId(7), bytes: Bytes::from_static(b"abc") });
    match only(&out) {
        AppEffect::PtyWrite { pane, data } => {
            assert_eq!(*pane, PaneId(7));
            assert_eq!(data, &Bytes::from_static(b"abc"));
        }
        other => panic!("expected PtyWrite, got {other:?}"),
    }
}

#[test]
fn pty_burst_renders_sentinel_window_with_burst_reason() {
    let mut state = AppState::default();
    let out = run(&mut state, AppIntent::PtyBurst { pane: PaneId(1), generation: 9 });
    assert_render(only(&out), WindowKey::new(0), RedrawReason::PtyBurst);
}

#[test]
fn pty_exit_cascades_child_exit_propagate_then_pty_close() {
    // child-exit / PTY cascade: one Intent, two Effects, in the arm's
    // push order (propagate before close) so the boundary tears the
    // child down after it has forwarded the status upward.
    let mut state = AppState::default();
    let out = run(&mut state, AppIntent::PtyExit { pane: PaneId(3), status: 137 });
    assert_eq!(out.len(), 2, "child-exit must emit exactly two effects: {out:?}");
    match &out[0] {
        AppEffect::ChildExitPropagate { pane, status } => {
            assert_eq!(*pane, PaneId(3));
            assert_eq!(*status, 137);
        }
        other => panic!("expected ChildExitPropagate first, got {other:?}"),
    }
    match &out[1] {
        AppEffect::PtyClose { pane } => assert_eq!(*pane, PaneId(3)),
        other => panic!("expected PtyClose second, got {other:?}"),
    }
}

// ── Keyboard / IME leaf ─────────────────────────────────────────────

#[test]
fn key_press_renders_user_input_but_release_is_silent() {
    let mut state = AppState::default();
    let down = run(
        &mut state,
        AppIntent::Key {
            window: window_key(2),
            code: KeyCode(65),
            mods: ModKey::empty(),
            pressed: true,
        },
    );
    assert_render(only(&down), window_key(2), RedrawReason::UserInput);

    let up = run(
        &mut state,
        AppIntent::Key {
            window: window_key(2),
            code: KeyCode(65),
            mods: ModKey::empty(),
            pressed: false,
        },
    );
    assert!(up.is_empty(), "key release must not emit an effect: {up:?}");
}

#[test]
fn ime_commit_writes_text_bytes_then_renders_ime() {
    let mut state = AppState::default();
    let out =
        run(&mut state, AppIntent::ImeCommit { window: window_key(4), text: "áç".to_string() });
    assert_eq!(out.len(), 2);
    match &out[0] {
        AppEffect::PtyWrite { pane, data } => {
            assert_eq!(*pane, PaneId(0), "commit targets focused-pane sentinel");
            assert_eq!(data, &Bytes::from("áç".to_string().into_bytes()));
        }
        other => panic!("expected PtyWrite, got {other:?}"),
    }
    assert_render(&out[1], window_key(4), RedrawReason::Ime);
}

#[test]
fn ime_preedit_start_end_all_render_ime() {
    let mut state = AppState::default();
    for intent in [
        AppIntent::ImePreedit { window: window_key(1), text: "x".to_string(), cursor: 0..1 },
        AppIntent::ImeStart { window: window_key(1) },
        AppIntent::ImeEnd { window: window_key(1) },
    ] {
        let out = run(&mut state, intent);
        assert_render(only(&out), window_key(1), RedrawReason::Ime);
    }
}

// ── Clipboard leaf ──────────────────────────────────────────────────

#[test]
fn copy_selection_emits_empty_clipboard_sentinel() {
    let mut state = AppState::default();
    let out = run(&mut state, AppIntent::CopySelection { window: window_key(1) });
    match only(&out) {
        AppEffect::ClipboardSet { text } => assert!(text.is_empty()),
        other => panic!("expected ClipboardSet, got {other:?}"),
    }
}

#[test]
fn paste_writes_text_bytes_to_focused_pane_sentinel() {
    let mut state = AppState::default();
    let out = run(
        &mut state,
        AppIntent::Paste { window: window_key(1), text: "hi".to_string(), bracketed: true },
    );
    match only(&out) {
        AppEffect::PtyWrite { pane, data } => {
            assert_eq!(*pane, PaneId(0));
            assert_eq!(data, &Bytes::from_static(b"hi"));
        }
        other => panic!("expected PtyWrite, got {other:?}"),
    }
}

// ── Scroll / wheel leaf ─────────────────────────────────────────────

#[test]
fn every_scroll_variant_renders_scroll() {
    let mut state = AppState::default();
    let variants = [
        AppIntent::ScrollUp { window: window_key(1), lines: 3 },
        AppIntent::ScrollDown { window: window_key(1), lines: 3 },
        AppIntent::ScrollPageUp { window: window_key(1) },
        AppIntent::ScrollPageDown { window: window_key(1) },
        AppIntent::ScrollToTop { window: window_key(1) },
        AppIntent::ScrollToBottom { window: window_key(1) },
        AppIntent::ScrollToCursor { window: window_key(1) },
        AppIntent::MouseWheel {
            window: window_key(1),
            delta_y: 1.0,
            delta_x: 0.0,
            mods: ModKey::empty(),
        },
    ];
    for intent in variants {
        let out = run(&mut state, intent);
        assert_render(only(&out), window_key(1), RedrawReason::Scroll);
    }
}

// ── Hyperlinks leaf ─────────────────────────────────────────────────

#[test]
fn click_url_opens_url_and_hover_renders_hover() {
    let mut state = AppState::default();
    let click = run(
        &mut state,
        AppIntent::ClickUrl { window: window_key(1), url: "https://x.example".to_string() },
    );
    match only(&click) {
        AppEffect::OpenURL { url } => assert_eq!(url, "https://x.example"),
        other => panic!("expected OpenURL, got {other:?}"),
    }
    let hover = run(&mut state, AppIntent::HoverUrl { window: window_key(1), url: None });
    assert_render(only(&hover), window_key(1), RedrawReason::Hover);
}

// ── Config / theming / frame-timing leaf ────────────────────────────

#[test]
fn config_theme_font_all_render_config_reload_on_sentinel() {
    let mut state = AppState::default();
    for intent in [
        AppIntent::FontSizeDelta { delta: 1 },
        AppIntent::ApplyTheme { name: "dark".to_string() },
        AppIntent::ConfigChanged { new: Box::default() },
    ] {
        let out = run(&mut state, intent);
        assert_render(only(&out), WindowKey::new(0), RedrawReason::ConfigReload);
    }
}

#[test]
fn redraw_requested_renders_vsync_and_exit_quits() {
    let mut state = AppState::default();
    let redraw = run(&mut state, AppIntent::RedrawRequested { window: window_key(5) });
    assert_render(only(&redraw), window_key(5), RedrawReason::Vsync);

    let exit = run(&mut state, AppIntent::Exit);
    assert!(matches!(only(&exit), AppEffect::Quit));
}

#[test]
fn files_dropped_and_tick_are_record_only_no_effects() {
    let mut state = AppState::default();
    let dropped = run(&mut state, AppIntent::FilesDropped { window: window_key(1), paths: vec![] });
    assert!(dropped.is_empty());
    let tick = run(&mut state, AppIntent::Tick { now: std::time::Instant::now() });
    assert!(tick.is_empty());
}

// ── Window lifecycle ────────────────────────────────────────────────

#[test]
fn new_window_increments_count_and_opens_with_role() {
    let mut state = AppState::default();
    let out = run(&mut state, AppIntent::NewWindow { role: WindowRole::Primary });
    assert_eq!(state.live_window_count, 1);
    match only(&out) {
        AppEffect::WindowOpen { role, initial_size } => {
            assert_eq!(*role, WindowRole::Primary);
            assert!(initial_size.is_none());
        }
        other => panic!("expected WindowOpen, got {other:?}"),
    }
}

#[test]
fn window_focused_emits_focus_only_on_actual_transition() {
    let mut state = AppState::default();
    // First focus: transition None -> Some(1) emits a render.
    let first = run(&mut state, AppIntent::WindowFocused { window: window_key(1) });
    assert_render(only(&first), window_key(1), RedrawReason::Focus);
    assert_eq!(state.focused_window, Some(window_key(1)));

    // Re-focus the same window: no transition, no effect (dedup guard).
    let repeat = run(&mut state, AppIntent::WindowFocused { window: window_key(1) });
    assert!(repeat.is_empty(), "re-focus of same window must be a no-op: {repeat:?}");

    // Focus a different window: transition emits a render.
    let switch = run(&mut state, AppIntent::WindowFocused { window: window_key(2) });
    assert_render(only(&switch), window_key(2), RedrawReason::Focus);
    assert_eq!(state.focused_window, Some(window_key(2)));
}

#[test]
fn window_blurred_only_clears_matching_focus() {
    let mut state = AppState { focused_window: Some(window_key(1)), ..Default::default() };

    // Blur a non-focused window: no-op.
    let other = run(&mut state, AppIntent::WindowBlurred { window: window_key(2) });
    assert!(other.is_empty(), "blur of non-focused window is a no-op: {other:?}");
    assert_eq!(state.focused_window, Some(window_key(1)));

    // Blur the focused window: clears + renders focus.
    let matched = run(&mut state, AppIntent::WindowBlurred { window: window_key(1) });
    assert_render(only(&matched), window_key(1), RedrawReason::Focus);
    assert_eq!(state.focused_window, None);
}

#[test]
fn window_resized_updates_grid_and_emits_render_then_resize() {
    let mut state = AppState::default();
    let out =
        run(&mut state, AppIntent::WindowResized { window: window_key(1), cols: 100, rows: 40 });
    assert_eq!(state.cols, 100);
    assert_eq!(state.rows, 40);
    assert_eq!(out.len(), 2);
    assert_render(&out[0], window_key(1), RedrawReason::Resize);
    match &out[1] {
        AppEffect::WindowResize { window, size } => {
            assert_eq!(*window, window_key(1));
            assert_eq!(size.width, 100.0);
            assert_eq!(size.height, 40.0);
        }
        other => panic!("expected WindowResize, got {other:?}"),
    }
}

#[test]
fn window_moved_records_position_without_effects() {
    let mut state = AppState::default();
    let out = run(
        &mut state,
        AppIntent::WindowMoved { window: window_key(1), pos: logical_pos(12.0, 34.0) },
    );
    assert!(out.is_empty(), "move is record-only: {out:?}");
    assert_eq!(state.last_window_pos, Some(logical_pos(12.0, 34.0)));
}

#[test]
fn closing_last_window_cascades_quit() {
    let mut state = AppState {
        live_window_count: 1,
        focused_window: Some(window_key(1)),
        ..Default::default()
    };
    let out = run(&mut state, AppIntent::WindowCloseRequested { window: window_key(1) });
    assert_eq!(state.live_window_count, 0);
    assert_eq!(state.focused_window, None, "closing focused window clears focus");
    assert_eq!(out.len(), 2, "last-window close = WindowClose + Quit: {out:?}");
    assert!(matches!(&out[0], AppEffect::WindowClose { window } if *window == window_key(1)));
    assert!(matches!(&out[1], AppEffect::Quit));
}

#[test]
fn closing_non_last_window_does_not_quit() {
    // A different window (9) holds focus while we close window 1.
    let mut state = AppState {
        live_window_count: 3,
        focused_window: Some(window_key(9)),
        ..Default::default()
    };
    let out = run(&mut state, AppIntent::WindowCloseRequested { window: window_key(1) });
    assert_eq!(state.live_window_count, 2);
    assert_eq!(state.focused_window, Some(window_key(9)), "non-focused close leaves focus intact");
    assert_eq!(out.len(), 1);
    assert!(matches!(only(&out), AppEffect::WindowClose { window } if *window == window_key(1)));
}

#[test]
fn window_close_count_saturates_at_zero_and_still_quits() {
    // Double-fire safety: count already 0, a stray close must not wrap.
    let mut state = AppState { live_window_count: 0, ..Default::default() };
    let out = run(&mut state, AppIntent::WindowCloseRequested { window: window_key(1) });
    assert_eq!(state.live_window_count, 0, "saturating_sub must not wrap to u32::MAX");
    assert_eq!(out.len(), 2);
    assert!(matches!(&out[1], AppEffect::Quit));
}

// ── Tab lifecycle ───────────────────────────────────────────────────

#[test]
fn new_tab_increments_count_and_activates_new_tab() {
    let mut state = AppState::default();
    let out = run(&mut state, AppIntent::NewTab { window: window_key(1), cwd: None });
    assert_eq!(state.tab_count, 1);
    assert_eq!(state.active_tab_idx, Some(0));
    assert_render(only(&out), window_key(1), RedrawReason::TabAdded);

    let out2 = run(&mut state, AppIntent::NewTab { window: window_key(1), cwd: None });
    assert_eq!(state.tab_count, 2);
    assert_eq!(state.active_tab_idx, Some(1), "newest tab becomes active");
    assert_render(only(&out2), window_key(1), RedrawReason::TabAdded);
}

#[test]
fn close_active_tab_steps_active_back_one() {
    let mut state = AppState { tab_count: 3, active_tab_idx: Some(2), ..Default::default() };
    let out = run(&mut state, AppIntent::CloseTab { window: window_key(1), idx: 2 });
    assert_eq!(state.tab_count, 2);
    assert_eq!(state.active_tab_idx, Some(1));
    assert_render(only(&out), window_key(1), RedrawReason::TabRemoved);
}

#[test]
fn close_tab_below_active_shifts_active_index_down() {
    let mut state = AppState { tab_count: 4, active_tab_idx: Some(3), ..Default::default() };
    // Closing a lower index shifts the active tab down by one.
    let _ = run(&mut state, AppIntent::CloseTab { window: window_key(1), idx: 1 });
    assert_eq!(state.tab_count, 3);
    assert_eq!(state.active_tab_idx, Some(2));
}

#[test]
fn close_tab_above_active_leaves_active_index() {
    let mut state = AppState { tab_count: 4, active_tab_idx: Some(1), ..Default::default() };
    let _ = run(&mut state, AppIntent::CloseTab { window: window_key(1), idx: 3 });
    assert_eq!(state.tab_count, 3);
    assert_eq!(state.active_tab_idx, Some(1), "closing a higher tab must not move active");
}

#[test]
fn close_final_tab_clears_active_index() {
    let mut state = AppState { tab_count: 1, active_tab_idx: Some(0), ..Default::default() };
    let _ = run(&mut state, AppIntent::CloseTab { window: window_key(1), idx: 0 });
    assert_eq!(state.tab_count, 0);
    assert_eq!(state.active_tab_idx, None, "no tabs left -> active cleared");
}

#[test]
fn close_tab_count_saturates_at_zero() {
    let mut state = AppState { tab_count: 0, ..Default::default() };
    let _ = run(&mut state, AppIntent::CloseTab { window: window_key(1), idx: 0 });
    assert_eq!(state.tab_count, 0, "tab_count must not wrap below zero");
}

#[test]
fn next_tab_wraps_around_the_end() {
    let mut state = AppState { tab_count: 3, active_tab_idx: Some(2), ..Default::default() };
    let out = run(&mut state, AppIntent::NextTab { window: window_key(1) });
    assert_eq!(state.active_tab_idx, Some(0), "next past last wraps to first");
    assert_render(only(&out), window_key(1), RedrawReason::TabSwitch);
}

#[test]
fn prev_tab_wraps_around_the_start() {
    let mut state = AppState { tab_count: 3, active_tab_idx: Some(0), ..Default::default() };
    let out = run(&mut state, AppIntent::PrevTab { window: window_key(1) });
    assert_eq!(state.active_tab_idx, Some(2), "prev before first wraps to last");
    assert_render(only(&out), window_key(1), RedrawReason::TabSwitch);
}

#[test]
fn next_prev_tab_single_tab_seeds_active_without_render() {
    // With exactly one tab and no active index yet, next/prev seed
    // active=0 but emit no TabSwitch (there is nowhere to switch).
    let mut state = AppState { tab_count: 1, active_tab_idx: None, ..Default::default() };
    let out = run(&mut state, AppIntent::NextTab { window: window_key(1) });
    assert_eq!(state.active_tab_idx, Some(0));
    assert!(out.is_empty(), "single-tab NextTab must not render: {out:?}");

    state.active_tab_idx = None;
    let out2 = run(&mut state, AppIntent::PrevTab { window: window_key(1) });
    assert_eq!(state.active_tab_idx, Some(0));
    assert!(out2.is_empty());
}

#[test]
fn next_tab_with_zero_tabs_is_noop() {
    let mut state = AppState { tab_count: 0, ..Default::default() };
    let out = run(&mut state, AppIntent::NextTab { window: window_key(1) });
    assert!(out.is_empty());
    assert_eq!(state.active_tab_idx, None);
}

#[test]
fn goto_tab_clamps_out_of_range_index() {
    let mut state = AppState { tab_count: 3, active_tab_idx: Some(0), ..Default::default() };
    // idx 9 clamps to last valid (2) and switches.
    let out = run(&mut state, AppIntent::GoToTab { window: window_key(1), idx: 9 });
    assert_eq!(state.active_tab_idx, Some(2));
    assert_render(only(&out), window_key(1), RedrawReason::TabSwitch);
}

#[test]
fn goto_current_tab_is_deduped_noop() {
    let mut state = AppState { tab_count: 3, active_tab_idx: Some(1), ..Default::default() };
    let out = run(&mut state, AppIntent::GoToTab { window: window_key(1), idx: 1 });
    assert!(out.is_empty(), "goto current tab must not re-render: {out:?}");
    assert_eq!(state.active_tab_idx, Some(1));
}

#[test]
fn goto_tab_with_zero_tabs_is_noop() {
    let mut state = AppState { tab_count: 0, ..Default::default() };
    let out = run(&mut state, AppIntent::GoToTab { window: window_key(1), idx: 0 });
    assert!(out.is_empty());
    assert_eq!(state.active_tab_idx, None);
}

#[test]
fn tear_out_tab_removes_source_tab_and_opens_child_window() {
    let mut state = AppState {
        tab_count: 2,
        active_tab_idx: Some(1),
        live_window_count: 1,
        ..Default::default()
    };
    let out = run(&mut state, AppIntent::TearOutTab { src_window: window_key(1), src_tab: 1 });
    assert_eq!(state.tab_count, 1, "source window loses the torn-out tab");
    assert_eq!(state.active_tab_idx, Some(0));
    assert_eq!(state.live_window_count, 2, "a child window is opened for the tab");
    assert_eq!(out.len(), 2);
    assert_render(&out[0], window_key(1), RedrawReason::TabRemoved);
    match &out[1] {
        AppEffect::WindowOpen { role, .. } => assert_eq!(*role, WindowRole::Child),
        other => panic!("expected WindowOpen(Child), got {other:?}"),
    }
}

// ── Pane lifecycle / navigation ─────────────────────────────────────

#[test]
fn split_pane_increments_count_and_focuses_new_leaf() {
    let mut state = AppState::default();
    let out = run(&mut state, AppIntent::SplitPane { window: window_key(1), dir: SplitDir::Right });
    assert_eq!(state.pane_count, 1);
    assert_eq!(state.focused_pane_idx, Some(0), "first split focuses the new leaf");
    assert_render(only(&out), window_key(1), RedrawReason::Layout);

    let out2 = run(&mut state, AppIntent::SplitPane { window: window_key(1), dir: SplitDir::Down });
    assert_eq!(state.pane_count, 2);
    assert_eq!(state.focused_pane_idx, Some(1));
    assert_render(only(&out2), window_key(1), RedrawReason::Layout);
}

#[test]
fn close_pane_clamps_focus_to_last_remaining_leaf() {
    let mut state = AppState { pane_count: 3, focused_pane_idx: Some(2), ..Default::default() };
    let out = run(&mut state, AppIntent::ClosePane { window: window_key(1) });
    assert_eq!(state.pane_count, 2);
    assert_eq!(state.focused_pane_idx, Some(1), "focus clamps to new max leaf index");
    assert_render(only(&out), window_key(1), RedrawReason::Layout);
}

#[test]
fn close_last_pane_clears_focus_index() {
    let mut state = AppState { pane_count: 1, focused_pane_idx: Some(0), ..Default::default() };
    let _ = run(&mut state, AppIntent::ClosePane { window: window_key(1) });
    assert_eq!(state.pane_count, 0);
    assert_eq!(state.focused_pane_idx, None, "no panes -> focus cleared");
}

#[test]
fn close_pane_count_saturates_at_zero() {
    let mut state = AppState { pane_count: 0, focused_pane_idx: None, ..Default::default() };
    let out = run(&mut state, AppIntent::ClosePane { window: window_key(1) });
    assert_eq!(state.pane_count, 0, "pane_count must not wrap below zero");
    assert_eq!(state.focused_pane_idx, None);
    // Still repaints the layout even on the empty edge.
    assert_render(only(&out), window_key(1), RedrawReason::Layout);
}

#[test]
fn resize_pane_renders_only_with_two_or_more_panes() {
    let mut state = AppState { pane_count: 1, ..Default::default() };
    let single = run(
        &mut state,
        AppIntent::ResizePane { window: window_key(1), dir: SplitDir::Left, cells: 2 },
    );
    assert!(single.is_empty(), "single-pane resize is a no-op: {single:?}");

    state.pane_count = 2;
    let split = run(
        &mut state,
        AppIntent::ResizePane { window: window_key(1), dir: SplitDir::Left, cells: 2 },
    );
    assert_render(only(&split), window_key(1), RedrawReason::Layout);
}

#[test]
fn directional_focus_renders_only_when_multiple_panes() {
    let mut state = AppState { pane_count: 1, ..Default::default() };
    for intent in [
        AppIntent::FocusPaneLeft { window: window_key(1) },
        AppIntent::FocusPaneRight { window: window_key(1) },
        AppIntent::FocusPaneUp { window: window_key(1) },
        AppIntent::FocusPaneDown { window: window_key(1) },
    ] {
        let out = run(&mut state, intent);
        assert!(out.is_empty(), "single-pane directional focus is a no-op: {out:?}");
    }

    state.pane_count = 2;
    let out = run(&mut state, AppIntent::FocusPaneLeft { window: window_key(1) });
    assert_render(only(&out), window_key(1), RedrawReason::Focus);
}

// ── Mouse ───────────────────────────────────────────────────────────

#[test]
fn mouse_move_coalesces_identical_positions() {
    let mut state = AppState::default();
    // First move to a fresh position emits Hover.
    let first =
        run(&mut state, AppIntent::MouseMove { window: window_key(1), pos: logical_pos(5.0, 5.0) });
    assert_render(only(&first), window_key(1), RedrawReason::Hover);
    assert_eq!(state.last_mouse_pos, Some(logical_pos(5.0, 5.0)));

    // Same position again: coalesced away (no effect).
    let repeat =
        run(&mut state, AppIntent::MouseMove { window: window_key(1), pos: logical_pos(5.0, 5.0) });
    assert!(repeat.is_empty(), "identical mouse move must coalesce: {repeat:?}");

    // A different position emits again.
    let moved =
        run(&mut state, AppIntent::MouseMove { window: window_key(1), pos: logical_pos(6.0, 5.0) });
    assert_render(only(&moved), window_key(1), RedrawReason::Hover);
}

#[test]
fn left_button_transition_guards_selection_render() {
    let mut state = AppState::default();
    // Press: transition false -> true, emits Selection.
    let press = run(
        &mut state,
        AppIntent::MouseButton {
            window: window_key(1),
            pressed: true,
            button: MouseButton::Left,
            mods: ModKey::empty(),
            pos: logical_pos(1.0, 2.0),
        },
    );
    assert!(state.mouse_left_down);
    assert_eq!(state.last_mouse_pos, Some(logical_pos(1.0, 2.0)));
    assert_render(only(&press), window_key(1), RedrawReason::Selection);

    // Redundant press (already down): position updates, but no render.
    let again = run(
        &mut state,
        AppIntent::MouseButton {
            window: window_key(1),
            pressed: true,
            button: MouseButton::Left,
            mods: ModKey::empty(),
            pos: logical_pos(3.0, 4.0),
        },
    );
    assert!(again.is_empty(), "redundant press must not re-render: {again:?}");
    assert_eq!(state.last_mouse_pos, Some(logical_pos(3.0, 4.0)), "position still tracked");

    // Release: transition true -> false, emits Selection.
    let release = run(
        &mut state,
        AppIntent::MouseButton {
            window: window_key(1),
            pressed: false,
            button: MouseButton::Left,
            mods: ModKey::empty(),
            pos: logical_pos(3.0, 4.0),
        },
    );
    assert!(!state.mouse_left_down);
    assert_render(only(&release), window_key(1), RedrawReason::Selection);
}

#[test]
fn non_left_button_renders_user_input_and_tracks_position() {
    let mut state = AppState::default();
    let out = run(
        &mut state,
        AppIntent::MouseButton {
            window: window_key(1),
            pressed: true,
            button: MouseButton::Right,
            mods: ModKey::empty(),
            pos: logical_pos(7.0, 8.0),
        },
    );
    assert!(!state.mouse_left_down, "right button must not set the left-down flag");
    assert_eq!(state.last_mouse_pos, Some(logical_pos(7.0, 8.0)));
    assert_render(only(&out), window_key(1), RedrawReason::UserInput);
}

// ── Foreground process ──────────────────────────────────────────────

#[test]
fn foreground_proc_change_is_transition_guarded() {
    let mut state = AppState::default();
    // None -> Some("vim") is a change: renders TitleOrTab.
    let first = run(
        &mut state,
        AppIntent::ForegroundProcChanged { pane: PaneId(1), name: Some("vim".to_string()) },
    );
    assert_eq!(state.fg_proc_name.as_deref(), Some("vim"));
    assert_render(only(&first), WindowKey::new(0), RedrawReason::TitleOrTab);

    // Same name again: deduped, no effect.
    let same = run(
        &mut state,
        AppIntent::ForegroundProcChanged { pane: PaneId(1), name: Some("vim".to_string()) },
    );
    assert!(same.is_empty(), "identical proc name must dedup: {same:?}");

    // Some("vim") -> None is a change: renders again.
    let cleared = run(&mut state, AppIntent::ForegroundProcChanged { pane: PaneId(1), name: None });
    assert_eq!(state.fg_proc_name, None);
    assert_render(only(&cleared), WindowKey::new(0), RedrawReason::TitleOrTab);
}

// ── Selection ───────────────────────────────────────────────────────

#[test]
fn selection_start_extend_end_gate_on_active_flag() {
    let mut state = AppState::default();
    // Extend before any start: nothing active, no effect.
    let early = run(
        &mut state,
        AppIntent::SelectionExtend { window: window_key(1), to: Pos { row: 0, col: 0 } },
    );
    assert!(early.is_empty(), "extend without an active selection is a no-op: {early:?}");

    // Start activates + renders.
    let start = run(
        &mut state,
        AppIntent::SelectionStart {
            window: window_key(1),
            anchor: Pos { row: 1, col: 2 },
            mode: SelectionMode::Cell,
        },
    );
    assert!(state.selection_active);
    assert_render(only(&start), window_key(1), RedrawReason::Selection);

    // Extend now renders while active.
    let extend = run(
        &mut state,
        AppIntent::SelectionExtend { window: window_key(1), to: Pos { row: 3, col: 4 } },
    );
    assert_render(only(&extend), window_key(1), RedrawReason::Selection);

    // End clears + renders.
    let end = run(&mut state, AppIntent::SelectionEnd { window: window_key(1) });
    assert!(!state.selection_active);
    assert_render(only(&end), window_key(1), RedrawReason::Selection);

    // A second end is a no-op (already inactive).
    let end2 = run(&mut state, AppIntent::SelectionEnd { window: window_key(1) });
    assert!(end2.is_empty());
}

#[test]
fn clear_selection_only_acts_when_active() {
    let mut state = AppState::default();
    let idle = run(&mut state, AppIntent::ClearSelection { window: window_key(1) });
    assert!(idle.is_empty(), "clear with no selection is a no-op: {idle:?}");

    state.selection_active = true;
    let cleared = run(&mut state, AppIntent::ClearSelection { window: window_key(1) });
    assert!(!state.selection_active);
    assert_render(only(&cleared), window_key(1), RedrawReason::Selection);
}

// ── Search overlay ──────────────────────────────────────────────────

#[test]
fn search_open_close_are_transition_guarded() {
    let mut state = AppState::default();
    let open = run(&mut state, AppIntent::OpenSearch { window: window_key(1) });
    assert!(state.search_open);
    assert_render(only(&open), window_key(1), RedrawReason::Overlay);

    // Re-open when already open: no-op.
    let reopen = run(&mut state, AppIntent::OpenSearch { window: window_key(1) });
    assert!(reopen.is_empty(), "re-open of open search is a no-op: {reopen:?}");

    let close = run(&mut state, AppIntent::CloseSearch { window: window_key(1) });
    assert!(!state.search_open);
    assert_render(only(&close), window_key(1), RedrawReason::Overlay);

    // Re-close when already closed: no-op.
    let reclose = run(&mut state, AppIntent::CloseSearch { window: window_key(1) });
    assert!(reclose.is_empty());
}

#[test]
fn search_query_and_step_render_only_while_open() {
    let mut state = AppState::default();
    // Closed: query/step do nothing.
    let closed_q =
        run(&mut state, AppIntent::SearchQuery { window: window_key(1), query: "foo".to_string() });
    assert!(closed_q.is_empty());
    let closed_s = run(&mut state, AppIntent::SearchStep { window: window_key(1), forward: true });
    assert!(closed_s.is_empty());

    state.search_open = true;
    let open_q =
        run(&mut state, AppIntent::SearchQuery { window: window_key(1), query: "foo".to_string() });
    assert_render(only(&open_q), window_key(1), RedrawReason::Overlay);
    let open_s = run(&mut state, AppIntent::SearchStep { window: window_key(1), forward: false });
    assert_render(only(&open_s), window_key(1), RedrawReason::Overlay);
}

// ── Command palette ─────────────────────────────────────────────────

#[test]
fn toggle_command_palette_flips_state_each_call() {
    let mut state = AppState::default();
    let open = run(&mut state, AppIntent::ToggleCommandPalette { window: window_key(1) });
    assert!(state.palette_open);
    assert_render(only(&open), window_key(1), RedrawReason::Overlay);

    let close = run(&mut state, AppIntent::ToggleCommandPalette { window: window_key(1) });
    assert!(!state.palette_open, "toggle flips back to closed");
    assert_render(only(&close), window_key(1), RedrawReason::Overlay);
}

#[test]
fn palette_filter_step_submit_gate_on_open() {
    let mut state = AppState::default();
    // Closed: nothing happens.
    let filt = run(
        &mut state,
        AppIntent::PaletteFilter { window: window_key(1), filter: "x".to_string() },
    );
    assert!(filt.is_empty());
    let step = run(&mut state, AppIntent::PaletteStep { window: window_key(1), delta: 1 });
    assert!(step.is_empty());
    let submit_closed = run(
        &mut state,
        AppIntent::PaletteSubmit {
            window: window_key(1),
            choice: PaletteChoice { id: "a".to_string() },
        },
    );
    assert!(submit_closed.is_empty(), "submit while closed is a no-op");

    // Open: filter/step render; submit closes and renders once.
    state.palette_open = true;
    let filt_open = run(
        &mut state,
        AppIntent::PaletteFilter { window: window_key(1), filter: "x".to_string() },
    );
    assert_render(only(&filt_open), window_key(1), RedrawReason::Overlay);

    let submit_open = run(
        &mut state,
        AppIntent::PaletteSubmit {
            window: window_key(1),
            choice: PaletteChoice { id: "a".to_string() },
        },
    );
    assert!(!state.palette_open, "submit closes the palette");
    assert_render(only(&submit_open), window_key(1), RedrawReason::Overlay);
}

// ── OS drag / broadcast ─────────────────────────────────────────────

#[test]
fn os_drag_outcome_emits_matching_drag_end() {
    let mut state = AppState::default();
    let out = run(
        &mut state,
        AppIntent::OsDragOutcome(PendingDragOutcomeCore {
            src_window: window_key(2),
            committed: true,
        }),
    );
    match only(&out) {
        AppEffect::OsDragEnd { src_window, committed } => {
            assert_eq!(*src_window, window_key(2));
            assert!(*committed);
        }
        other => panic!("expected OsDragEnd, got {other:?}"),
    }
}

#[test]
fn set_broadcast_scope_is_transition_guarded() {
    let mut state = AppState::default();
    // Off -> CurrentTab is a change: renders the title/tab strip.
    let change =
        run(&mut state, AppIntent::SetBroadcastScope { scope: BroadcastScope::CurrentTab });
    assert_eq!(state.broadcast_scope, BroadcastScope::CurrentTab);
    assert_render(only(&change), WindowKey::new(0), RedrawReason::TitleOrTab);

    // Setting the same scope again: deduped no-op.
    let same = run(&mut state, AppIntent::SetBroadcastScope { scope: BroadcastScope::CurrentTab });
    assert!(same.is_empty(), "no-op scope set must not re-render: {same:?}");
}
