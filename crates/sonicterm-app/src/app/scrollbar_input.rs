//! Scrollbar mouse-input wiring (#386 PR-C).
//!
//! PR-A landed the pure-function model (`sonicterm_ui::scrollbar`); PR-B
//! wired the renderer to emit the bar quads. This module is the input
//! glue: it converts a logical-pixel pointer event on the active pane
//! into either a `view_top` jump (track click) or the start of a drag
//! gesture (thumb press). Auto-hide-on-hover proximity is PR-D scope.
//!
//! All coordinates here are **logical pixels** to match the pane-rect
//! layout the renderer also uses. The width constant mirrors the one
//! held inside `sonicterm_gpu::core::emit_pane_scrollbar` — if
//! you change one, change the other (PR-D will lift this into config).
//!
//! NOT exported above the `app` module: tests reach in via
//! `pub(crate)` helpers exposed on `App`.

use sonicterm_cfg::config::ScrollbarMode;
use sonicterm_ui::scrollbar::{self, HitTarget, Point, Rect, ScrollbarGeometry};

/// Bar width in logical pixels, before DPI scaling. Must stay in sync with
/// the authored width inside `sonicterm_gpu::core::emit_pane_scrollbar`
/// (also 8.0). Callers scale this by the renderer's `scale_factor()` so the
/// grabbable band matches the *drawn* band — the renderer draws the bar at
/// `8.0 * scale` raster px, so hit-testing the bare 8.0 leaves the left of
/// the thumb dead on fractional-DPI displays (issue #711).
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
    width_px: f32,
) -> HitOutcome {
    let Some(geometry) = scrollbar::compute(
        viewport_rows,
        total_rows,
        view_top,
        pane_rect,
        mode,
        width_px,
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
use sonicterm_gpu::core::GpuRenderer;
use winit::window::WindowId;

/// Inset a pane's layout rect by the renderer's per-side content padding so
/// the right-aligned scrollbar is hit-tested where it is actually drawn.
///
/// The renderer draws the bar inside `content_rect` — the pane rect minus
/// padding (`core.rs` `emit_pane_scrollbar` consumes the padded pane view) —
/// but hit-testing used the raw, unpadded layout rect. Since the track is
/// right-aligned, the grabbable band sat ~`padding_right` px to the RIGHT of
/// the visible thumb; at fractional DPI (e.g. 12 logical * 1.75 = 21 px right
/// padding vs a 14 px bar) the two bands stopped overlapping and clicks on
/// the visible thumb missed entirely (issue #711). Apply the same inset here.
fn content_inset_rect(pane: Rect, pl: f32, pr: f32, pt: f32, pb: f32) -> Rect {
    Rect::new(
        pane.x + pl,
        pane.y + pt,
        (pane.w - pl - pr).max(0.0),
        (pane.h - pt - pb).max(0.0),
    )
}

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
        // Inset by the renderer's content padding so the hit band lines up
        // with the drawn (right-aligned) bar, not the raw pane edge (#711).
        let pane_rect = match self.main_renderer() {
            Some(r) => content_inset_rect(
                Rect::new(ui_rect.x, ui_rect.y, ui_rect.w, ui_rect.h),
                r.padding_left_px(),
                r.padding_right_px(),
                r.padding_top_px(),
                r.padding_bottom_px(),
            ),
            None => Rect::new(ui_rect.x, ui_rect.y, ui_rect.w, ui_rect.h),
        };
        let Some(pane) = ws.panes.get(&active_id) else { return HitOutcome::Miss };
        // try_lock keeps with the §4 land-mine "render uses try_lock,
        // not lock" rule; a busy parser briefly defers scrollbar input
        // until the next move event rather than risk an AB-BA deadlock.
        let Some(parser) = pane.parser.try_lock() else { return HitOutcome::Miss };
        let grid = parser.grid();
        let viewport_rows = grid.rows;
        let total_rows = grid.scrollback_len() as u64 + viewport_rows as u64;
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        drop(parser);
        // Match the renderer's DPI-scaled bar width so the whole drawn thumb
        // is grabbable, not just the rightmost 8px (issue #711).
        let scale = self.main_renderer().map_or(1.0, GpuRenderer::scale_factor);
        hit(
            pane_rect,
            viewport_rows,
            total_rows,
            view_top,
            self.config.appearance.scrollbar,
            active_id,
            Point::new(lx, ly),
            SCROLLBAR_WIDTH_PX * scale,
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
            let vt = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
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

    // ── Child-window scrollbar input (#pane-scrollbar) ──────────────────
    // Mirrors of the main-window scrollbar input above, but operating on a
    // torn-out child `WindowState`. Torn-out windows had no scrollbar input
    // wiring at all (the bar was invisible and inert). The pure geometry
    // helpers (`hit`, `apply_drag_at`, `page_*`) are window-agnostic; only
    // the state lookups differ.

    /// Hit-test a logical-px pointer on the active pane's scrollbar in the
    /// child window `win_id`.
    pub(crate) fn scrollbar_hit_at_in_child(
        &self,
        win_id: WindowId,
        lx: f32,
        ly: f32,
    ) -> HitOutcome {
        let Some(child) = self.windows.get(&win_id) else { return HitOutcome::Miss };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get(tab_idx) else { return HitOutcome::Miss };
        let active_id = st.active_pane;
        let pane_rects = App::compute_pane_rects_for(child);
        let Some((_, ui_rect)) = pane_rects.iter().find(|(id, _)| *id == active_id) else {
            return HitOutcome::Miss;
        };
        // Inset by the child renderer's content padding so the hit band lines
        // up with the drawn right-aligned bar (#711), same as the main path.
        let pane_rect = match child.renderer.as_ref() {
            Some(r) => content_inset_rect(
                Rect::new(ui_rect.x, ui_rect.y, ui_rect.w, ui_rect.h),
                r.padding_left_px(),
                r.padding_right_px(),
                r.padding_top_px(),
                r.padding_bottom_px(),
            ),
            None => Rect::new(ui_rect.x, ui_rect.y, ui_rect.w, ui_rect.h),
        };
        let Some(pane) = child.panes.get(&active_id) else { return HitOutcome::Miss };
        let Some(parser) = pane.parser.try_lock() else { return HitOutcome::Miss };
        let grid = parser.grid();
        let viewport_rows = grid.rows;
        let total_rows = grid.scrollback_len() as u64 + viewport_rows as u64;
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        drop(parser);
        // Match the renderer's DPI-scaled bar width (issue #711).
        let scale = child.renderer.as_ref().map_or(1.0, GpuRenderer::scale_factor);
        hit(
            pane_rect,
            viewport_rows,
            total_rows,
            view_top,
            self.config.appearance.scrollbar,
            active_id,
            Point::new(lx, ly),
            SCROLLBAR_WIDTH_PX * scale,
        )
    }

    /// Apply an in-flight scrollbar drag in the child window `win_id`,
    /// returning `(pane_id, new_view_top)` if a drag is active.
    pub(crate) fn scrollbar_drag_apply_in_child(
        &self,
        win_id: WindowId,
        cursor_x: f32,
        cursor_y: f32,
    ) -> Option<(u64, u64)> {
        let child = self.windows.get(&win_id)?;
        let state = child.scrollbar_drag.as_ref()?;
        Some((state.pane_id, apply_drag_at(state, Point::new(cursor_x, cursor_y))))
    }

    /// Page the active pane's scrollbar in the child window `win_id`.
    pub(crate) fn scrollbar_track_page_in_child(&mut self, win_id: WindowId, forward: bool) {
        let (active_id, viewport_rows, total_rows, view_top) = {
            let Some(child) = self.windows.get(&win_id) else { return };
            let tab_idx = child.tabs.active_index();
            let Some(st) = child.tab_states.get(tab_idx) else { return };
            let active_id = st.active_pane;
            let Some(pane) = child.panes.get(&active_id) else { return };
            let Some(parser) = pane.parser.try_lock() else { return };
            let grid = parser.grid();
            let vp = grid.rows;
            let total = grid.scrollback_len() as u64 + vp as u64;
            let vt = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
            (active_id, vp, total, vt)
        };
        let new_top = if forward {
            page_down(view_top, viewport_rows, total_rows)
        } else {
            page_up(view_top, viewport_rows)
        };
        let live_top = total_rows.saturating_sub(viewport_rows as u64);
        self.set_child_pane_view_top(win_id, active_id, new_top, live_top);
    }

    /// Write `view_top` to a child pane's `viewport_top_abs` (clearing to
    /// `None` at the live tail) and request a redraw.
    pub(crate) fn set_child_pane_view_top(
        &mut self,
        win_id: WindowId,
        pane_id: u64,
        view_top: u64,
        live_top: u64,
    ) {
        let Some(child) = self.windows.get_mut(&win_id) else { return };
        if let Some(pane) = child.panes.get_mut(&pane_id) {
            pane.viewport_top_abs = if view_top >= live_top { None } else { Some(view_top) };
        }
        super::mark_all_panes_dirty(&child.panes);
        // Parity with the main window's `mark_scrollbar_active`: use
        // `entry().or_insert_with` (NOT `get_mut`). The `scrollbar_vis`
        // entry is created lazily by the render path, so on a freshly
        // torn-out child the first scroll happens BEFORE any entry exists —
        // `get_mut` would silently no-op and the auto-hide bar would stay
        // hidden until something else rendered. Create-on-demand fixes that.
        let now = std::time::Instant::now();
        child
            .scrollbar_vis
            .entry(pane_id)
            .or_insert_with(|| super::scrollbar_visibility::ScrollbarVisState::new(now))
            .mark_active(now);
        child.request_redraw();
    }

    /// Test-only inspector for the live scrollbar-drag state.
    #[doc(hidden)]
    pub fn __test_scrollbar_drag(&self) -> Option<ScrollbarDragState> {
        self.main().and_then(|ws| ws.scrollbar_drag)
    }
}
