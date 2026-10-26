//! Window-owned scrollbar interaction and redraw adapters.

use super::*;

impl App {
    // Ordering: redraw_request_count uses Relaxed as a plain tally of redraw
    // requests; it guards no other data, so no happens-before edge is required.
    pub(super) fn request_scrollbar_redraw(&self) {
        self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(main) = self.main() {
            main.request_window_redraw();
        }
    }

    /// Refresh Auto-mode right-edge hover state from the last cursor
    /// position. Returns `true` when any pane crosses the threshold. The
    /// window's scrollbar wake asks for the frame a crossing needs, once.
    pub(crate) fn refresh_scrollbar_hover_from_cursor(&mut self) -> bool {
        if !matches!(self.config.appearance.scrollbar, ScrollbarMode::Auto) {
            // When: the configured scrollbar matches Always or Never there is no
            // hover threshold to cross, so no state is touched and no redraw runs.
            return false;
        }
        let pane_rects = self.compute_active_pane_rects();
        if pane_rects.is_empty() {
            // When: pane_rects is empty the active tab has no laid-out panes, so
            // there is nothing for the cursor to be near.
            return false;
        }
        let (cursor_x, cursor_y) = self.main().map(|main| main.cursor_pos).unwrap_or((0.0, 0.0));
        let cursor = (cursor_x as f32, cursor_y as f32);
        let rects: Vec<(u64, f32, f32, f32, f32)> =
            pane_rects.iter().map(|(id, rect)| (*id, rect.x, rect.y, rect.w, rect.h)).collect();
        let mode = self.config.appearance.scrollbar;
        self.main_mut()
            .map(|main| main.update_scrollbar_hover(&rects, cursor, mode, Instant::now()))
            .unwrap_or(false)
    }

    /// Test-only shim for the CursorMoved scrollbar-hover branch. Tests set
    /// `WindowState::cursor_pos`, provide `test_viewport_override`, then call
    /// this to exercise the same production state update and scrollbar wake.
    #[doc(hidden)]
    pub fn __test_refresh_scrollbar_hover_from_cursor(&mut self) -> bool {
        self.refresh_scrollbar_hover_from_cursor()
    }

    /// Child-window mirror of [`Self::refresh_scrollbar_hover_from_cursor`].
    /// Torn-out windows own their own `WindowState`, cursor position, pane
    /// layout, and redraw target, but the Auto-mode hover math must be shared
    /// with the main window. Returns `true` when any pane crosses the right-edge
    /// proximity threshold; the child's scrollbar wake asks for its frame, once.
    pub(crate) fn refresh_scrollbar_hover_from_cursor_in_child(
        &mut self,
        win_id: winit::window::WindowId,
    ) -> bool {
        if !matches!(self.config.appearance.scrollbar, ScrollbarMode::Auto) {
            // When: the configured scrollbar matches Always or Never the child
            // window has no fade to drive, so its hover flags stay untouched.
            return false;
        }
        let Some(child) = self.windows.get(&win_id) else {
            // When: windows no longer holds win_id the torn-out window closed
            // before this cursor move was handled.
            return false;
        };
        let pane_rects = Self::compute_pane_rects_for(child);
        if pane_rects.is_empty() {
            // When: pane_rects is empty the child window has no laid-out panes to
            // test against its cursor position.
            return false;
        }
        let cursor = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
        let rects: Vec<(u64, f32, f32, f32, f32)> =
            pane_rects.iter().map(|(id, rect)| (*id, rect.x, rect.y, rect.w, rect.h)).collect();
        let mode = self.config.appearance.scrollbar;
        self.windows
            .get_mut(&win_id)
            .map(|child| child.update_scrollbar_hover(&rects, cursor, mode, Instant::now()))
            .unwrap_or(false)
    }

    pub(crate) fn clear_scrollbar_hover(&mut self) -> bool {
        let mode = self.config.appearance.scrollbar;
        let changed = self
            .main_mut()
            .map(|main| main.clear_scrollbar_hover_states(mode, Instant::now()))
            .unwrap_or(false);
        if changed {
            self.request_scrollbar_redraw();
        }
        changed
    }

    pub(crate) fn clear_scrollbar_hover_in_child(
        &mut self,
        win_id: winit::window::WindowId,
    ) -> bool {
        let mode = self.config.appearance.scrollbar;
        let changed = self
            .windows
            .get_mut(&win_id)
            .map(|child| child.clear_scrollbar_hover_states(mode, Instant::now()))
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get(&win_id) {
                child.request_window_redraw();
            }
        }
        changed
    }

    /// Mark a pane's scrollbar as "actively in use" so its alpha
    /// resets to fully-visible and the idle hide timer restarts.
    /// Called from the update points (scroll, drag, view_top jump).
    pub(crate) fn mark_scrollbar_active(&mut self, pane_id: u64) {
        let now = Instant::now();
        let mode = self.config.appearance.scrollbar;
        let marked = self
            .main_mut()
            .map(|main| {
                main.note_scrollbar_activity(pane_id, mode, now);
                true
            })
            .unwrap_or(false);
        if marked {
            self.request_scrollbar_redraw();
        }
    }
}

impl super::super::WindowState {
    /// The pane whose scrollbar thumb this window is dragging, if any.
    pub(super) fn scrollbar_drag_pane(&self) -> Option<u64> {
        self.scrollbar_drag.as_ref().map(|drag| drag.pane_id)
    }

    /// Request this window's frame when a pane's alpha has not reached its
    /// target. A settled window has no frame coming, so a retarget that starts
    /// a fade would otherwise leave the bar frozen. Every window method that
    /// retargets ends here; a request already in flight is reused.
    pub(super) fn wake_scrollbar_if_animating(&mut self) {
        let animating = self
            .scrollbar_vis
            .values()
            .any(|state| (state.alpha - state.target).abs() > f32::EPSILON);
        if !animating {
            // When: no pane is `animating`, every bar shows its target and needs no frame.
            return;
        }
        self.mark_redraw(super::super::redraw::RedrawCause::Scrollbar);
        if self.frame_deadlines_allowed() && !self.redraw.request_in_flight {
            self.redraw.request_in_flight = true;
            self.request_window_redraw();
        }
    }

    /// Record scrollbar activity on `pane_id`, creating its state when the
    /// pane has not rendered yet, so a scroll before the first frame still shows the bar.
    pub(crate) fn note_scrollbar_activity(
        &mut self,
        pane_id: u64,
        mode: ScrollbarMode,
        now: Instant,
    ) {
        let drag_pane = self.scrollbar_drag_pane();
        note_pane_activity(&mut self.scrollbar_vis, pane_id, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
    }

    /// Recompute every pane's scrollbar target after this window's drag
    /// started or ended, so a release starts its fade at once.
    pub(super) fn retarget_scrollbars(&mut self, mode: ScrollbarMode, now: Instant) {
        let drag_pane = self.scrollbar_drag_pane();
        retarget_panes(&mut self.scrollbar_vis, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
    }

    /// Start a scrollbar thumb drag, which holds its bar shown from `now`.
    pub(crate) fn begin_scrollbar_drag(
        &mut self,
        drag: super::super::scrollbar_input::ScrollbarDragState,
        mode: ScrollbarMode,
        now: Instant,
    ) {
        self.scrollbar_drag = Some(drag);
        self.retarget_scrollbars(mode, now);
    }

    /// End any scrollbar thumb drag (release, focus loss or drag cancel), so a
    /// bar past its idle window starts fading at `now`. Returns whether a drag ended.
    pub(crate) fn end_scrollbar_drag(&mut self, mode: ScrollbarMode, now: Instant) -> bool {
        let ended = self.scrollbar_drag.take().is_some();
        self.retarget_scrollbars(mode, now);
        ended
    }

    /// Update this window's right-edge hover flags from `cursor` over the
    /// laid-out pane `rects`; true when a pane crossed the threshold.
    pub(crate) fn update_scrollbar_hover(
        &mut self,
        rects: &[(u64, f32, f32, f32, f32)],
        cursor: (f32, f32),
        mode: ScrollbarMode,
        now: Instant,
    ) -> bool {
        let drag_pane = self.scrollbar_drag_pane();
        let changed =
            update_hover_states(&mut self.scrollbar_vis, rects, cursor, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
        changed
    }

    /// Clear this window's right-edge hover flags; true when one was set.
    pub(crate) fn clear_scrollbar_hover_states(
        &mut self,
        mode: ScrollbarMode,
        now: Instant,
    ) -> bool {
        let drag_pane = self.scrollbar_drag_pane();
        let changed = clear_hover_states(&mut self.scrollbar_vis, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
        changed
    }
}

#[cfg(test)]
#[path = "scrollbar_visibility_window_tests.rs"]
mod scrollbar_visibility_window_tests;
