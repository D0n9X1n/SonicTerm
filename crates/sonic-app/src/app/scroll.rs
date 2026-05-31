//! Scrollback scroll mutator + cursor→pane resolver (#412).
//!
//! Wires mouse-wheel and `Action::Scroll(_)` keymap actions into the
//! canonical `PaneState.viewport_top_abs` field that the scrollbar drag
//! path (#410) already writes to. Three callers converge here:
//!
//! * `WindowEvent::MouseWheel` — uses [`App::pane_at_cursor`] to pick the
//!   pane under the cursor before scrolling.
//! * `Action::Scroll(_)` keymap dispatch — always targets the active pane.
//! * (Future) Copy-mode scroll — out of scope for #412.
//!
//! Alt-screen panes short-circuit: full-screen TUIs (vim/htop/fzf) own
//! scroll semantics themselves and the host must not synthesize a viewport
//! shift behind their back.

use sonic_shared::render::GpuRenderer;

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
    /// Sole writer for wheel + keymap; scrollbar drag (#410) writes the
    /// same field via `scrollbar_input::set_active_pane_view_top`.
    #[doc(hidden)]
    pub fn scroll_pane(&mut self, pane_id: u64, delta_lines: i32) {
        if delta_lines == 0 {
            return;
        }
        let Some(ws) = self.main() else { return };
        let Some(pane) = ws.panes.get(&pane_id) else { return };
        // Snapshot scroll metrics under the parser lock. `lock` matches
        // the scrollbar_input.rs pattern for input-side reads (main
        // thread, not the render path) — see CLAUDE.md §4 land-mine.
        // We intentionally do NOT use `try_lock` here: dropping a wheel
        // event because the PTY parser is mid-burst would be a worse UX
        // than briefly waiting for it.
        let (live_top, current_view_top, is_alt) = {
            let parser = pane.parser.lock();
            let grid = parser.grid();
            if grid.is_alt() {
                return;
            }
            let live_top = grid.scrollback_len() as u64;
            let current = GpuRenderer::resolved_view_top_abs(grid, pane.viewport_top_abs);
            (live_top, current, false)
        };
        let _ = is_alt;
        let new_view_top: u64 = if delta_lines < 0 {
            current_view_top.saturating_sub((-(delta_lines as i64)) as u64)
        } else {
            current_view_top.saturating_add(delta_lines as u64).min(live_top)
        };
        let Some(ws) = self.main_mut() else { return };
        if let Some(pane) = ws.panes.get_mut(&pane_id) {
            pane.viewport_top_abs =
                if new_view_top >= live_top { None } else { Some(new_view_top) };
        }
        super::mark_all_panes_dirty(&ws.panes);
        if let Some(w) = ws.window.as_ref() {
            w.request_redraw();
        }
        // #386 PR-D parity: any view_top jump from wheel/keymap is
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
