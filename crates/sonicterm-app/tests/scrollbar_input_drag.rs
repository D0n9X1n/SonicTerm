//! #386 PR-C — scrollbar mouse input wiring.
//!
//! These tests exercise the pure helpers in
//! [`sonicterm_app::app::scrollbar_input`] that translate logical-px pointer
//! events into [`ScrollbarDragState`] + `view_top` jumps. Plumbing into
//! `WindowEvent` (CursorMoved / MouseInput) is verified manually via the
//! §13 GUI smoke; here we lock down the geometry/math contract that the
//! window-event handler depends on so a future refactor cannot silently
//! break the drag.

use sonicterm_app::app::scrollbar_input::{
    apply_drag, apply_drag_at, hit, page_down, page_up, HitOutcome, ScrollbarDragState,
    SCROLLBAR_WIDTH_PX,
};
use sonicterm_core::config::ScrollbarMode;
use sonicterm_ui::scrollbar::{Point, Rect};

const PANE: Rect = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };
const VP_ROWS: u16 = 24;
const TOTAL_ROWS: u64 = 524; // 500 rows of scrollback + 24 viewport
const PANE_ID: u64 = 42;

fn track_x() -> f32 {
    PANE.x + PANE.w - SCROLLBAR_WIDTH_PX + 1.0
}

#[test]
fn click_on_thumb_starts_drag_with_grab_offset() {
    // Start scrolled to the top of scrollback so the thumb sits at y=0.
    let outcome = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        0,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(track_x(), 5.0),
    );
    let HitOutcome::StartDrag(state) = outcome else {
        panic!("expected StartDrag on thumb hit, got {outcome:?}");
    };
    assert_eq!(state.pane_id, PANE_ID);
    assert_eq!(state.viewport_rows, VP_ROWS);
    assert_eq!(state.total_rows, TOTAL_ROWS);
    // grab_offset = press_y - thumb_rect.y; when view_top=0 the thumb
    // sits at the track top so the offset is just press_y.
    assert!(
        (state.grab_offset - 5.0).abs() < 0.001,
        "expected grab_offset ≈ 5.0, got {}",
        state.grab_offset
    );
}

#[test]
fn drag_changes_view_top_proportionally() {
    let HitOutcome::StartDrag(state) = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        0,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(track_x(), 5.0),
    ) else {
        panic!("expected StartDrag");
    };
    // Initial view_top is 0.
    assert_eq!(apply_drag(&state, 5.0), 0);
    // Drag the cursor to the bottom of the track — view_top should
    // saturate at the live tail (= total - viewport = 500).
    let max_view_top = TOTAL_ROWS - VP_ROWS as u64;
    assert_eq!(apply_drag(&state, PANE.h), max_view_top);
    // Drag to roughly midpoint — view_top should be ~half the maximum.
    let mid = apply_drag(&state, PANE.h * 0.5);
    let half = max_view_top / 2;
    assert!(
        mid.abs_diff(half) <= max_view_top / 10,
        "midpoint drag landed at {mid}, expected near {half}"
    );
}

#[test]
fn y_only_drag_changes_view_top() {
    let start_x = track_x();
    let press_y = 5.0;
    let HitOutcome::StartDrag(state) = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        0,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(start_x, press_y),
    ) else {
        panic!("expected StartDrag");
    };

    let initial = apply_drag_at(&state, Point::new(start_x, press_y));
    let after_y_only_move = apply_drag_at(&state, Point::new(start_x, PANE.h * 0.5));

    assert_eq!(initial, 0);
    assert!(
        after_y_only_move > initial,
        "vertical thumb drag with unchanged x must update view_top; got {after_y_only_move}"
    );
}

#[test]
fn cursor_moved_handler_routes_scrollbar_drag_with_mouse_y() {
    let window_event_src = include_str!("../src/app/window_event.rs");
    assert!(
        window_event_src.contains("scrollbar_drag_apply(lx, ly)"),
        "CursorMoved scrollbar drag must pass logical y, not x, into scrollbar_drag_apply"
    );
    assert!(
        !window_event_src.contains("scrollbar_drag_apply(lx)"),
        "regression guard: passing lx as the only drag coordinate ignores vertical motion"
    );
}

#[test]
fn click_on_track_above_thumb_pages_up() {
    // Scroll near the live tail so the thumb sits at the bottom and the
    // area above it is the "track above" zone.
    let max_view_top = TOTAL_ROWS - VP_ROWS as u64;
    let outcome = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        max_view_top,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(track_x(), 5.0),
    );
    assert_eq!(outcome, HitOutcome::PageUp);
    let new = page_up(max_view_top, VP_ROWS);
    assert_eq!(new, max_view_top - VP_ROWS as u64);
}

#[test]
fn click_on_track_below_thumb_pages_down() {
    // Scroll to the top: thumb at top, anything below is "track below".
    let outcome = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        0,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(track_x(), PANE.h - 5.0),
    );
    assert_eq!(outcome, HitOutcome::PageDown);
    let new = page_down(0, VP_ROWS, TOTAL_ROWS);
    assert_eq!(new, VP_ROWS as u64);
}

#[test]
fn click_outside_scrollbar_returns_miss() {
    // Click well inside the grid area, far from the right-edge bar.
    let outcome =
        hit(PANE, VP_ROWS, TOTAL_ROWS, 0, ScrollbarMode::Always, PANE_ID, Point::new(50.0, 50.0));
    assert_eq!(outcome, HitOutcome::Miss);
}

#[test]
fn hidden_scrollbar_mode_always_misses() {
    // Mode::Never must short-circuit before any geometry math.
    let outcome = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        0,
        ScrollbarMode::Never,
        PANE_ID,
        Point::new(track_x(), 5.0),
    );
    assert_eq!(outcome, HitOutcome::Miss);
}

#[test]
fn grid_not_scrollable_misses() {
    // total_rows == viewport_rows — no scrollback, no bar.
    let outcome = hit(
        PANE,
        VP_ROWS,
        VP_ROWS as u64,
        0,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(track_x(), 5.0),
    );
    assert_eq!(outcome, HitOutcome::Miss);
}

#[test]
fn page_up_clamped_to_zero() {
    assert_eq!(page_up(10, VP_ROWS), 0);
    assert_eq!(page_up(0, VP_ROWS), 0);
}

#[test]
fn page_down_clamped_to_live_tail() {
    let max = TOTAL_ROWS - VP_ROWS as u64;
    assert_eq!(page_down(max, VP_ROWS, TOTAL_ROWS), max);
    assert_eq!(page_down(max - 5, VP_ROWS, TOTAL_ROWS), max);
}

#[test]
fn drag_state_preserves_press_geometry_across_growth() {
    // After capture, growing total_rows in the underlying grid must not
    // perturb apply_drag — the geometry is snapshotted. Document the
    // semantics so a future refactor doesn't accidentally re-fetch live
    // metrics inside the drag handler.
    let HitOutcome::StartDrag(state) = hit(
        PANE,
        VP_ROWS,
        TOTAL_ROWS,
        0,
        ScrollbarMode::Always,
        PANE_ID,
        Point::new(track_x(), 5.0),
    ) else {
        panic!("expected StartDrag");
    };
    // Simulate the underlying grid growing by 100 rows between press
    // and the first CursorMoved — apply_drag uses the snapshotted total.
    let cloned_state = ScrollbarDragState { total_rows: state.total_rows + 100, ..state };
    let live_drag = apply_drag(&cloned_state, PANE.h);
    let snap_drag = apply_drag(&state, PANE.h);
    assert_ne!(
        live_drag, snap_drag,
        "test sanity check — snapshotting vs not snapshotting should differ"
    );
    // The real handler holds `state` (not `cloned_state`); confirm that.
    assert_eq!(snap_drag, TOTAL_ROWS - VP_ROWS as u64);
}
