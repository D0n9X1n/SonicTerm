//! Scrollbar mouse-input wiring (#386 PR-C).
//!
//! PR-A landed the pure-function model (`sonic_ui::scrollbar`); PR-B
//! wired the renderer to emit the bar quads. This module is the input
//! glue: it converts a logical-pixel pointer event on the active pane
//! into either a `view_top` jump (track click) or the start of a drag
//! gesture (thumb press). Auto-hide-on-hover proximity is PR-D scope.
//!
//! All coordinates here are **logical pixels** to match the pane-rect
//! layout the renderer also uses. The width constant mirrors the one
//! held inside `sonic_shared::render::core::emit_pane_scrollbar` — if
//! you change one, change the other (PR-D will lift this into config).
//!
//! NOT exported above the `app` module: tests reach in via
//! `pub(crate)` helpers exposed on `App`.

use sonic_core::config::ScrollbarMode;
use sonic_ui::scrollbar::{self, HitTarget, Point, Rect, ScrollbarGeometry};

/// Bar width in logical pixels. Must stay in sync with
/// `sonic_shared::render::core::SCROLLBAR_WIDTH_PX` (PR-B).
pub const SCROLLBAR_WIDTH_PX: f32 = 8.0;

/// Active drag gesture on a pane's scrollbar thumb.
///
/// Captured on mouse-down on a thumb; consumed on mouse-up. While set,
/// `CursorMoved` events route to `apply_drag` instead of extending a
/// selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarDragState {
    /// Pane the drag was started on. We pin the gesture to this pane
    /// so cursor moves that wander outside the pane keep scrolling it
    /// rather than starting a selection in a neighbour.
    pub pane_id: u64,
    /// Cached geometry captured at press time — using a captured snapshot
    /// (rather than recomputing every CursorMoved) keeps the drag
    /// monotonic when the grid grows beneath the cursor.
    pub geometry: ScrollbarGeometry,
    /// y of the cursor at press (logical px).
    pub press_y: f32,
    /// Offset from `thumb_rect.y` to `press_y` — preserves the
    /// "grab point" so the thumb doesn't jump under the cursor.
    pub grab_offset: f32,
    /// Viewport height (rows) at press; used by `apply_drag`.
    pub viewport_rows: u16,
    /// Total rows (scrollback + viewport) at press; used by `apply_drag`.
    pub total_rows: u64,
}

/// Result of [`hit`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitOutcome {
    /// Click missed the scrollbar — caller should fall through to the
    /// existing selection / hyperlink path.
    Miss,
    /// Click landed on the thumb — start a drag with the contained state.
    StartDrag(ScrollbarDragState),
    /// Click landed above the thumb — caller should page-up.
    PageUp,
    /// Click landed below the thumb — caller should page-down.
    PageDown,
}

/// Classify a logical-px pointer press against the scrollbar of a single
/// pane.
///
/// Returns [`HitOutcome::Miss`] when the scrollbar is hidden, when the
/// grid isn't scrollable, or when the click falls outside the bar.
pub fn hit(
    pane_rect: Rect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: ScrollbarMode,
    pane_id: u64,
    press: Point,
) -> HitOutcome {
    let Some(geometry) = scrollbar::compute(
        viewport_rows,
        total_rows,
        view_top,
        pane_rect,
        mode,
        SCROLLBAR_WIDTH_PX,
    ) else {
        return HitOutcome::Miss;
    };
    match scrollbar::hit_test(&geometry, press) {
        HitTarget::None => HitOutcome::Miss,
        HitTarget::Thumb => HitOutcome::StartDrag(ScrollbarDragState {
            pane_id,
            geometry,
            press_y: press.y,
            grab_offset: press.y - geometry.thumb_rect.y,
            viewport_rows,
            total_rows,
        }),
        HitTarget::TrackAbove => HitOutcome::PageUp,
        HitTarget::TrackBelow => HitOutcome::PageDown,
    }
}

/// Translate a `CursorMoved` cursor position (logical px) during a drag
/// back into a `view_top` row. The thumb's `grab_offset` keeps the
/// cursor's pixel position constant relative to the thumb.
pub fn apply_drag_at(state: &ScrollbarDragState, cursor: Point) -> u64 {
    let thumb_y = cursor.y - state.grab_offset;
    scrollbar::thumb_to_view_top(&state.geometry, thumb_y, state.viewport_rows, state.total_rows)
}

/// Translate a `CursorMoved` y (logical px) during a drag back into a
/// `view_top` row. The thumb's `grab_offset` keeps the cursor's pixel
/// position constant relative to the thumb.
pub fn apply_drag(state: &ScrollbarDragState, cursor_y: f32) -> u64 {
    apply_drag_at(state, Point::new(state.geometry.thumb_rect.x, cursor_y))
}

/// Track-click jump: page up by the viewport size, clamped to 0.
pub fn page_up(view_top: u64, viewport_rows: u16) -> u64 {
    view_top.saturating_sub(viewport_rows as u64)
}

/// Track-click jump: page down by the viewport size, clamped to the
/// live tail (`scrollback_len`, i.e. `total_rows - viewport_rows`).
pub fn page_down(view_top: u64, viewport_rows: u16, total_rows: u64) -> u64 {
    let max_view_top = total_rows.saturating_sub(viewport_rows as u64);
    view_top.saturating_add(viewport_rows as u64).min(max_view_top)
}

use super::App;
use sonic_shared::render::GpuRenderer;

impl App {
    /// Look up the active pane's scrollbar geometry / scrollback metrics
    /// and classify a logical-px press against them.
    ///
    /// Returns [`HitOutcome::Miss`] (and stays a no-op) when there is no
    /// active pane / no renderer / the click is outside the bar — the
    /// caller then falls through to the existing selection path.
    pub(crate) fn scrollbar_hit_at(&self, lx: f32, ly: f32) -> HitOutcome {
        let Some(ws) = self.main() else { return HitOutcome::Miss };
        let tab_idx = ws.tabs.active_index();
        let Some(st) = ws.tab_states.get(tab_idx) else { return HitOutcome::Miss };
        let active_id = st.active_pane;
        let pane_rects = self.compute_active_pane_rects();
        let Some((_, ui_rect)) = pane_rects.iter().find(|(id, _)| *id == active_id) else {
            return HitOutcome::Miss;
        };
        let pane_rect = Rect::new(ui_rect.x, ui_rect.y, ui_rect.w, ui_rect.h);
        let Some(pane) = ws.panes.get(&active_id) else { return HitOutcome::Miss };
        // try_lock keeps with the §4 land-mine "render uses try_lock,
        // not lock" rule; a busy parser briefly defers scrollbar input
        // until the next move event rather than risk an AB-BA deadlock.
        let Some(parser) = pane.parser.try_lock() else { return HitOutcome::Miss };
        let grid = parser.grid();
        let viewport_rows = grid.rows;
        let total_rows = grid.scrollback_len() as u64 + viewport_rows as u64;
        let view_top = GpuRenderer::resolved_view_top_abs(grid, pane.viewport_top_abs);
        drop(parser);
        hit(
            pane_rect,
            viewport_rows,
            total_rows,
            view_top,
            self.config.appearance.scrollbar,
            active_id,
            Point::new(lx, ly),
        )
    }

    /// Compute the new `view_top` for an in-flight scrollbar drag, given
    /// the latest logical-px cursor position. Returns `None` if no drag
    /// is active on the main window.
    pub(crate) fn scrollbar_drag_apply(&self, cursor_x: f32, cursor_y: f32) -> Option<(u64, u64)> {
        let ws = self.main()?;
        let state = ws.scrollbar_drag.as_ref()?;
        Some((state.pane_id, apply_drag_at(state, Point::new(cursor_x, cursor_y))))
    }

    /// Apply a track-click page jump on the active pane.
    /// `forward` = page-down (toward live tail); `false` = page-up.
    pub(crate) fn scrollbar_track_page(&mut self, forward: bool) {
        let Some(ws) = self.main() else { return };
        let tab_idx = ws.tabs.active_index();
        let Some(st) = ws.tab_states.get(tab_idx) else { return };
        let active_id = st.active_pane;
        let Some(pane) = ws.panes.get(&active_id) else { return };
        let (viewport_rows, total_rows, view_top) = {
            let Some(parser) = pane.parser.try_lock() else { return };
            let grid = parser.grid();
            let vp = grid.rows;
            let total = grid.scrollback_len() as u64 + vp as u64;
            let vt = GpuRenderer::resolved_view_top_abs(grid, pane.viewport_top_abs);
            (vp, total, vt)
        };
        let new_top = if forward {
            page_down(view_top, viewport_rows, total_rows)
        } else {
            page_up(view_top, viewport_rows)
        };
        let live_top = total_rows.saturating_sub(viewport_rows as u64);
        self.set_active_pane_view_top(new_top, live_top);
    }

    /// Write `view_top` to the active pane's `viewport_top_abs` (clearing
    /// to `None` when at the live tail so the auto-follow behaviour
    /// resumes) and request a redraw.
    pub(crate) fn set_active_pane_view_top(&mut self, view_top: u64, live_top: u64) {
        let Some(ws) = self.main_mut() else { return };
        let tab_idx = ws.tabs.active_index();
        let Some(st) = ws.tab_states.get(tab_idx) else { return };
        let active_id = st.active_pane;
        if let Some(pane) = ws.panes.get_mut(&active_id) {
            pane.viewport_top_abs = if view_top >= live_top { None } else { Some(view_top) };
        }
        super::mark_all_panes_dirty(&ws.panes);
        if let Some(w) = ws.window.as_ref() {
            w.request_redraw();
        }
        // #386 PR-D: any view_top jump (track click, prompt-nav, copy
        // mode scroll, mouse-wheel) counts as scrollbar activity.
        self.mark_scrollbar_active(active_id);
    }

    /// Test-only inspector for the live scrollbar-drag state.
    #[doc(hidden)]
    pub fn __test_scrollbar_drag(&self) -> Option<ScrollbarDragState> {
        self.main().and_then(|ws| ws.scrollbar_drag)
    }
}
