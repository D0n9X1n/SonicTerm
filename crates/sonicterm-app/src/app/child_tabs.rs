//! Child-window tab and pane operations: drag-merge, reap, main-window
//! visibility, pane spawning, and the per-child tab and pane mutators.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_cfg::keymap::Direction;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::{pane::PaneTree, tabs::Tab};
use sonicterm_vt::vt::Parser;
use winit::window::{Window, WindowId};

use super::child_window::resize_visible_panes_in_child;
use super::{next_pane_id, App, PaneState, TabState};

impl App {
    pub(super) fn merge_child_into_target(
        &mut self,
        src_id: WindowId,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) -> bool {
        match self.transfer_tab(Some(src_id), src_idx, Some(target.window), target.slot) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    target: "sonicterm_app::app::child_window",
                    ?error,
                    "drag-merge refused; source tab retained"
                );
                false
            }
        }
    }
    // Ordering: `reap_call_count` is Relaxed — a test-observable tally with no
    // other state ordered against it.
    pub(super) fn reap_empty_child(&mut self, win_id: WindowId) {
        // Bump the test-observable counter on EVERY invocation (even no-ops on
        // stale ids) so tests can pin that child-window cleanup routed through
        // this contract rather than a raw `windows.remove`.
        self.reap_call_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(child) = self.windows.get(&win_id) {
            if child.tabs.is_empty() {
                if let Some(mut removed) = self.windows.remove(&win_id) {
                    for pane in std::mem::take(&mut removed.panes).into_values() {
                        self.retire_pane(pane);
                    }
                    // Close the governor owners before the state drops.
                    self.release_owners_of(&mut removed);
                    self.release_child_window_registries(win_id);
                    drop(removed);
                    tracing::info!(
                        target: "sonicterm_app::app::child_window",
                        "child window reaped after drag-merge; remaining children={}",
                        self.child_window_count()
                    );
                    self.request_exit_if_no_active_windows();
                }
            }
        }
    }
    pub(super) fn merge_main_into_child(
        &mut self,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) -> bool {
        match self.transfer_tab(None, src_idx, Some(target.window), target.slot) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    target: "sonicterm_app::app::child_window",
                    ?error,
                    "drag-merge refused; source tab retained"
                );
                false
            }
        }
    }
    pub(super) fn hide_main_window(&mut self) {
        if let Some(id) = self.main_window_id {
            self.cancel_window_rename(id);
            self.cancel_tab_edit(id);
        }
        if let Some(w) = self.main_window() {
            w.set_visible(false);
        }
        if let Some(ws) = self.main_mut() {
            ws.hidden = true;
            ws.redraw.cancel_surface_probe();
            ws.redraw.request_in_flight = false;
            ws.redraw.deferred = false;
        }
        tracing::info!(
            target: "sonicterm_app::app::child_window",
            "main window hidden (drained); windows={}",
            self.windows.len()
        );
    }
    pub(super) fn show_main_window(&mut self) {
        if let Some(w) = self.main_window() {
            w.set_visible(true);
        }
        if let Some(ws) = self.main_mut() {
            ws.hidden = false;
            ws.refresh_monitor_period();
            ws.redraw.backend_occluded = false;
            ws.redraw.cancel_surface_probe();
            ws.invalidate_visibility_frame();
            ws.request_visible_frame();
        }
    }

    /// Build a fresh `PaneState` bound to the given child window's
    /// `(cols, rows, Arc<Window>)` snapshot, spawning the pane's PTY, its VT
    /// loop with reply backpressure. Shared by `spawn_tab_in_child` and
    /// `split_active_pane_in_child` so both get identical thread wiring.
    ///
    /// The VT worker derives every shared handle from the completed `PaneState`,
    /// so command, media, cursor, and keyboard state cannot diverge from what the
    /// child window reads.
    // Lock order: parser releases before test_pane_launches; neither guard survives PTY or worker creation.
    pub(super) fn spawn_pane_state_for_child(
        &self,
        pane_id: u64,
        cols: u16,
        rows: u16,
        child_window: Arc<Window>,
        launch: &super::pane_launch::PaneLaunch,
    ) -> PaneState {
        use sonicterm_grid::grid::Grid;
        use sonicterm_vt::vt::Parser;
        // Honour the user's configured scrollback depth; child
        // windows must match the main window, not the Grid's 10k default.
        let mut grid = Grid::new(cols, rows);
        grid.set_scrollback_limit(self.config.terminal.scrollback);
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            grid,
            None,
            Arc::clone(&self.capture_staging_pool),
        )));
        // Seed theme defaults for OSC 10/11/12 + OSC 4 palette.
        {
            let mut p = parser.lock();
            super::seed_parser_theme_colors(&mut p, &self.theme);
        }
        let redraw_target = Arc::new(Mutex::new(Some(child_window.id())));
        #[cfg(test)]
        self.test_pane_launches.borrow_mut().push((pane_id, launch.clone()));
        let shell_opts = sonicterm_io::pty::ShellSpawnOpts {
            clean_e2e: self.runtime_smoke.is_some(),
            ..launch.shell_spawn_opts(
                self.config.terminal.term_program.clone(),
                self.config.terminal.shell.clone(),
            )
        };
        let pty = match PtyHandle::spawn_default_shell(cols, rows, shell_opts) {
            Ok(pty) => Some(pty),
            Err(e) => {
                tracing::error!(
                    target: "sonicterm_app::app::child_window",
                    "failed to spawn pty for child pane: {e}"
                );
                None
            }
        };
        let mut pane_state = PaneState::new_with_media_pool(parser, pty, &self.inline_media_pool);
        self.reserve_pane_teardown(&mut pane_state);
        pane_state.redraw_target = redraw_target;
        if pane_state.pty.is_some() {
            super::spawn_pane::spawn_pane_workers(
                pane_id,
                &pane_state,
                self.event_loop_proxy.clone(),
                "sonicterm-vt-loop-child",
            );
        }
        pane_state
    }

    /// Spawn a new tab containing a single fresh pane inside the
    /// child window identified by `win_id`. Returns `false` if no
    /// such child window exists (caller should fall back to the main
    /// App's `new_tab`). The new pane's redraw target is bound to the
    /// child window so VT output redraws the child, not the main App.
    pub(super) fn spawn_tab_in_child(&mut self, win_id: WindowId) -> bool {
        // Snapshot everything we need from the child up-front so the
        // mutable borrow ends before we spawn the VT thread (which
        // captures clones), then re-borrow to install the new tab.
        let launch = super::pane_launch::PaneLaunch::from_window(
            self.windows.get(&win_id),
            &self.local_hostname,
        );
        let (cols, rows, child_window) = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: `windows` no longer holds `win_id`, so the recorded child
                // is gone and the caller falls back to the main App's `new_tab`.
                return false;
            };
            let Some(renderer) = child.renderer.as_ref() else {
                // When: this child has no `renderer`, so no cell grid size exists
                // to spawn the pane's PTY against.
                return false;
            };
            let Some(win) = child.window.as_ref() else {
                // When: this child has no `window`, so the new pane would have no
                // redraw target to bind its VT output to.
                return false;
            };
            let (c, r) = renderer.cells();
            (c, r, win.clone())
        };
        let pane_id = next_pane_id();
        let pane_state =
            self.spawn_pane_state_for_child(pane_id, cols, rows, child_window.clone(), &launch);
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: windows lost win_id, retire the uninstalled pane instead of closing its native transport inline.
            self.retire_pane(pane_state);
            return false;
        };
        child.panes.insert(pane_id, pane_state);
        let n = child.tabs.len() + 1;
        child.tabs.push(Tab::new(format!("shell {n}")));
        child.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        resize_visible_panes_in_child(child);
        true
    }

    // ──────────────────────────────────────────────────────────────────
    // per-child action helpers
    //
    // These mirror the equivalent main-window mutators in
    // `app/misc.rs` and `app/spawn_pane.rs` but operate on a child
    // window's owned (tabs / tab_states / panes) triple. Each helper:
    //   * returns `true` if it mutated state (so the caller knows to
    //     bump `redraw_request_count`),
    //   * issues `child.request_redraw()` on the child handle
    //     when state changed,
    //   * returns `false` (no-op + no redraw) when the recorded child
    //     no longer exists — the keymap_dispatch caller then falls
    //     through to the main-window default.
    //
    // The empty-tab-vec post-condition (close the window? leave it
    // dangling? merge into main?) is deliberately left to the existing
    // teardown plumbing — `reap_empty_child` runs on user-event drain
    // and on the next focus event, so we don't replicate that
    // single-source-of-truth here.
    // ──────────────────────────────────────────────────────────────────

    /// Close the active tab of the given child window. Returns `true`
    /// on success.
    pub(super) fn close_active_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let idx = {
            let Some(child) = self.windows.get(&win_id) else {
                // When: `windows` no longer holds `win_id`, so the recorded child
                // is gone and the caller falls back to the main-window default.
                return false;
            };
            child.tabs.active_index()
        };
        self.close_tab_at_in_child(win_id, idx)
    }

    /// Close the tab at `idx` in the given child window. Used by the
    /// close-button (×) hit-test path in the child's tab bar, which
    /// passes the clicked index directly (not the active one). Returns
    /// `true` on success.
    ///
    /// When this drains the child to zero tabs it invokes
    /// [`Self::reap_empty_child`] itself, so callers never have to. Every close
    /// path (× button, Cmd+W, close-active-pane-or-tab) flows through here and
    /// gets the reap for free; a caller-responsible reap would leave a closed
    /// single-pane child window as a ghost frame.
    pub(super) fn close_tab_at_in_child(&mut self, win_id: WindowId, idx: usize) -> bool {
        let (drained, retired) = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: `windows` no longer holds `win_id`, so the recorded child
                // is gone and there is no tab list to close from.
                return false;
            };
            if idx >= child.tab_states.len() {
                // When: `idx` is past the end of `tab_states`, so the click
                // resolved to a tab that has already been removed.
                return false;
            }
            let st = child.tab_states.remove(idx);
            let retired: Vec<_> =
                st.tree.leaves().into_iter().filter_map(|id| child.remove_pane(id)).collect();
            if let Some(tab_id) = child.tabs.tabs().get(idx).map(|t| t.id) {
                child.tabs.close(tab_id);
            }
            resize_visible_panes_in_child(child);
            (child.tabs.is_empty(), retired)
        };
        for pane in retired {
            self.retire_pane(pane);
        }
        if drained {
            self.reap_empty_child(win_id);
        }
        true
    }

    /// Close-active-pane-or-tab inside a child window. Mirrors the
    /// iTerm2/wezterm rule: > 1 pane → close the focused pane only,
    /// else → close the whole tab.
    pub(super) fn close_active_pane_or_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is neither a
            // pane tree nor a tab for this action to close.
            return false;
        };
        let pane_count = st.tree.leaves().len();
        if pane_count <= 1 {
            // When: `pane_count` is the last leaf, so closing it means closing
            // the tab. Drop the borrows so the tab path can re-borrow.
            let _ = st;
            let _ = child;
            return self.close_active_tab_in_child(win_id);
        }
        let focus = st.active_pane;
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            // When: `tree.close` accepted `focus`, so a pane really left the
            // layout and the survivors must be refocused and resized.
            st.active_pane = new_focus;
            // Same reason as the main-window path: the search was scanning the
            // grid that just went away.
            if let Some(search) = st.search.as_mut() {
                search.invalidate_for_new_grid();
            }
            let retired = child.remove_pane(focus);
            // The surviving sibling's PaneRect just grew to cover the closed
            // pane's area. Push the new layout into its Grid + PtyHandle so the
            // survivor (and TUIs like vim) reflow into the freed space; without
            // this the survivor keeps its narrow split-time column count until
            // the OS window is resized.
            resize_visible_panes_in_child(child);
            if let Some(r) = child.renderer.as_mut() {
                r.flash_pane_focus(new_focus);
            }
            child.request_redraw();
            if let Some(pane) = retired {
                self.retire_pane(pane);
            }
            return true;
        }
        false
    }

    /// Advance the active tab in the child window.
    pub(super) fn next_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        child.tabs.next();
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    /// Step back one tab in the child window.
    pub(super) fn prev_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        child.tabs.prev();
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    /// Activate a specific tab index in the child window.
    pub(super) fn activate_tab_in_child(&mut self, win_id: WindowId, idx: usize) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        child.tabs.activate(idx);
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    /// Activate the last tab in the child window.
    pub(super) fn activate_last_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    // ──────────────────────────────────────────────────────────────────
    // per-child PANE mutators
    //
    // Mirror of the per-child tab helpers above, but for pane-level
    // actions (`Action::SplitRight`, `SplitDown`, `ClosePane`,
    // `FocusPane(_)`, `TogglePaneZoom`, `ResizePane{Left,Right,Up,Down}`).
    // Same contract as the tab helpers: return `true` if mutated state
    // and request_redraw on the child's window; return `false` (no-op)
    // when the recorded child no longer exists so keymap_dispatch can
    // fall back to the main-window default.
    //
    // Without these, Cmd+D / Cmd+Shift+D / Cmd+[ / Cmd+] / Cmd+Z typed
    // in a torn-out child window would silently mutate the MAIN App's
    // active tab instead of this child's.
    // ──────────────────────────────────────────────────────────────────

    /// Split the active pane of the given child window in `dir`. Returns
    /// `true` on success.
    pub(super) fn split_active_pane_in_child(&mut self, win_id: WindowId, dir: Direction) -> bool {
        let Some(child) = self.windows.get(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let Some(tab) = child.tab_states.get(child.tabs.active_index()) else {
            // When: `tab_states` has no active tab, refuse before creating a speculative PTY.
            return false;
        };
        if !tab.tree.leaves().contains(&tab.active_pane)
            || !child.panes.contains_key(&tab.active_pane)
        {
            // When: `active_pane` is not a live leaf, preserve topology without spawning another shell.
            return false;
        }
        let launch = super::pane_launch::PaneLaunch::from_window(Some(child), &self.local_hostname);
        let new_id = next_pane_id();
        let pane_state =
            if let (Some(renderer), Some(win)) = (child.renderer.as_ref(), child.window.as_ref()) {
                let (cols, rows) = renderer.cells();
                self.spawn_pane_state_for_child(new_id, cols, rows, win.clone(), &launch)
            } else if child.renderer.is_none() && child.window.is_none() {
                // When: both `renderer` and `window` are absent — a headless
                // test child still needs pane ownership without a live PTY.
                let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
                    Grid::new(80, 24),
                    None,
                    Arc::clone(&self.capture_staging_pool),
                )));
                PaneState::new_with_media_pool(parser, None, &self.inline_media_pool)
            } else {
                // When: only one of `renderer`/`window` exists, so the child is
                // mid-construction and cell metrics cannot be trusted yet.
                return false;
            };
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: windows lost win_id, retire the uninstalled native pane through the shared driver.
            self.retire_pane(pane_state);
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: tab_states lacks tab_idx, preserve the layout and retire the uninstalled pane.
            self.retire_pane(pane_state);
            return false;
        };
        let focus = st.active_pane;
        if !st.tree.split(focus, dir, new_id) {
            // When: tree.split refuses focus, new_id has no UI owner but its native transport still needs retirement.
            self.retire_pane(pane_state);
            return false;
        }
        st.active_pane = new_id;
        child.panes.insert(new_id, pane_state);
        resize_visible_panes_in_child(child);
        if let Some(r) = child.renderer.as_mut() {
            r.flash_pane_focus(new_id);
        }
        child.request_redraw();
        true
    }

    /// Close the active pane in the given child window. If the active
    /// tab has only one pane left, degrades to closing the tab (same
    /// iTerm2/wezterm rule as the main-window `close_active_pane`).
    pub(super) fn close_active_pane_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no pane
            // tree from which to remove the focused leaf.
            return false;
        };
        let focus = st.active_pane;
        if matches!(st.tree, PaneTree::Leaf { id, .. } if id == focus) {
            // When: `matches` finds a lone `Leaf` holding `focus`, so closing the
            // pane closes the whole tab rather than one split.

            // Release the &mut WindowState borrow so the tab path can re-borrow.
            let _ = child;
            return self.close_active_tab_in_child(win_id);
        }
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            // When: `tree.close` accepted `focus`, so the layout actually lost a
            // pane and the survivors must be resized and refocused.
            st.active_pane = new_focus;
            // Same reason as the main-window path: the search was scanning the
            // grid that just went away.
            if let Some(search) = st.search.as_mut() {
                search.invalidate_for_new_grid();
            }
            let retired = child.remove_pane(focus);
            resize_visible_panes_in_child(child);
            if let Some(r) = child.renderer.as_mut() {
                r.flash_pane_focus(new_focus);
            }
            child.request_redraw();
            if let Some(pane) = retired {
                self.retire_pane(pane);
            }
            return true;
        }
        false
    }

    /// Move pane focus in the given direction within the active tab of
    /// the given child window.
    pub(super) fn focus_pane_dir_in_child(&mut self, win_id: WindowId, dir: Direction) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(next) = child
            .tab_states
            .get(tab_idx)
            .and_then(|tab| tab.tree.focus_neighbor(tab.active_pane, dir))
        else {
            // When: no neighbor exists in `dir`, this child still consumes the
            // recognized action instead of allowing it to mutate the main window.
            return true;
        };
        if let Some(change) = child.begin_pane_focus_change(next) {
            child.finish_pane_focus_change(change);
        }
        true
    }

    /// Toggle zoom on the active pane in the given child window.
    pub(super) fn toggle_active_pane_zoom_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no pane
            // tree holding a zoom flag to toggle.
            return false;
        };
        let active = st.active_pane;
        if st.tree.toggle_zoom(active) {
            resize_visible_panes_in_child(child);
            child.request_redraw();
        }
        // Routed regardless of toggle result so the action does not leak
        // to the main window.
        true
    }

    /// Resize the active split edge in the given direction within the
    /// active tab of the given child window.
    pub(super) fn resize_active_split_in_child(
        &mut self,
        win_id: WindowId,
        dir: Direction,
    ) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no split
            // tree whose edge could move.
            return false;
        };
        if st.tree.resize_split(st.active_pane, dir, 0.05) {
            resize_visible_panes_in_child(child);
            child.request_redraw();
        }
        // Routed regardless of resize result.
        true
    }

    /// Test-only: report where the child redraw path anchors the OS IME
    /// candidate area — `"palette"`, `"search"` or `"terminal"` — mirroring the
    /// precedence the child render path applies.
    #[doc(hidden)]
    pub fn __test_child_ime_candidate_anchor_kind(&self, win_id: WindowId) -> Option<&'static str> {
        let child = self.windows.get(&win_id)?;
        if self.command_palette.is_open() && self.palette_attached_window == Some(win_id) {
            // When: the palette is open and attached to `win_id`, so it owns the
            // caret and the candidate window anchors to its query row.
            return Some("palette");
        }
        let search_open = child
            .tab_states
            .get(child.tabs.active_index())
            .is_some_and(|state| state.search.is_some());
        Some(if search_open { "search" } else { "terminal" })
    }
}

#[cfg(test)]
#[path = "child_tabs_tests.rs"]
mod child_tabs_tests;
