//! Scrollback scroll mutator + cursor→pane resolver.
//!
//! Wires mouse-wheel and `Action::Scroll(_)` keymap actions into the
//! canonical `PaneState.viewport_top_abs` field that the scrollbar drag
//! path already writes to. Three callers converge here:
//!
//! * `WindowEvent::MouseWheel` — uses [`App::pane_at_cursor`] to pick the
//!   pane under the cursor before scrolling.
//! * `Action::Scroll(_)` keymap dispatch — always targets the active pane.
//! * (Future) Copy-mode scroll — out of scope.
//!
//! Alt-screen panes short-circuit: full-screen TUIs (vim/htop/fzf) own
//! scroll semantics themselves and the host must not synthesize a viewport
//! shift behind their back.

use sonicterm_gpu::core::GpuRenderer;

use super::App;

impl App {
    /// Apply a signed line delta to a pane's `viewport_top_abs`, clamped to
    /// `[0, scrollback.len()]`. Negative `delta_lines` scrolls UP into the
    /// scrollback; positive scrolls DOWN toward the live tail. When the
    /// resulting offset is at or past the live tail, `viewport_top_abs`
    /// snaps back to `None` so auto-follow resumes.
    ///
    /// Alt-screen panes are a no-op: full-screen TUIs own their own scroll
    /// semantics and the host must not synthesize a viewport shift behind
    /// their back.
    ///
    /// Sole writer for wheel + keymap; scrollbar drag writes the
    /// same field via `scrollbar_input::set_active_pane_view_top`.
    #[doc(hidden)]
    pub fn scroll_pane(&mut self, pane_id: u64, delta_lines: i32) {
        if delta_lines == 0 {
            // When: a zero delta_lines still repaints every pane and marks the
            // scrollbar active, so it is discarded before the parser lock.
            return;
        }
        let Some(ws) = self.main() else {
            // When: main() has no workspace there is no pane map to resolve
            // pane_id against, so the wheel event is dropped.
            return;
        };
        let Some(pane) = ws.panes.get(&pane_id) else {
            // When: panes no longer holds pane_id the pane closed between the
            // wheel event and this lookup, so the scroll is dropped.
            return;
        };
        // Snapshot scroll metrics under the parser lock. `lock` matches
        // the scrollbar_input.rs pattern for input-side reads on the main
        // thread; only render paths are required to use `try_lock`.
        // We intentionally do NOT use `try_lock` here: dropping a wheel
        // event because the PTY parser is mid-burst would be a worse UX
        // than briefly waiting for it.
        let (live_top, current_view_top) = {
            let parser = pane.parser.lock();
            let grid = parser.grid();
            if grid.is_alt() {
                // When: grid.is_alt() the pane is a full-screen TUI that owns
                // scroll itself, so the host must not shift its viewport.
                return;
            }
            let live_top = grid.scrollback_len() as u64;
            let current = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
            (live_top, current)
        };
        let new_view_top: u64 = if delta_lines < 0 {
            current_view_top.saturating_sub((-(delta_lines as i64)) as u64)
        } else {
            // When: delta_lines is positive the view moves toward the live
            // tail, and min(live_top) stops it passing the newest row.
            current_view_top.saturating_add(delta_lines as u64).min(live_top)
        };
        let Some(ws) = self.main_mut() else {
            // When: main_mut() yields None the pane map cannot be written, so
            // the snapshot is discarded rather than unwrapped into a panic.
            return;
        };
        if let Some(pane) = ws.panes.get_mut(&pane_id) {
            pane.viewport_top_abs = if new_view_top >= live_top {
                None
            } else {
                // When: new_view_top stays below live_top the pane keeps an
                // explicit anchor instead of resuming auto-follow.
                Some(new_view_top)
            };
        }
        super::mark_all_panes_dirty(&ws.panes);
        if let Some(w) = ws.window.as_ref() {
            w.request_redraw();
        }
        // Parity: any view_top jump from wheel/keymap is
        // scrollbar activity for auto-hide bookkeeping.
        self.mark_scrollbar_active(pane_id);
    }

    /// Return the pane id under the given logical-px cursor position in the
    /// active tab, or `None` if the point falls outside every pane (e.g.
    /// over the tab bar or window padding).
    ///
    /// Used by `WindowEvent::MouseWheel` to target the pane under the
    /// cursor. The keymap path always targets the active pane and does
    /// NOT call this.
    #[doc(hidden)]
    pub fn pane_at_cursor(&self, lx: f32, ly: f32) -> Option<u64> {
        for (pane_id, rect) in self.compute_active_pane_rects() {
            if lx >= rect.x && lx < rect.x + rect.w && ly >= rect.y && ly < rect.y + rect.h {
                // When: the point falls inside rect the first matching pane_id
                // in layout order wins and later rects are not tested.
                return Some(pane_id);
            }
        }
        None
    }

    /// Viewport row count of the active pane (for `Page{Up,Down}` deltas).
    /// Returns `None` when there is no active pane or the parser lock is
    /// contended on an alternate code path.
    pub(crate) fn active_pane_viewport_rows(&self) -> Option<u16> {
        let pane = self.active_pane()?;
        let parser = pane.parser.try_lock()?;
        Some(parser.grid().rows)
    }
}
