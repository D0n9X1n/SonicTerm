//! Extracted from `app/mod.rs` from the monolithic app module.
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use sonicterm_ui::ime::ImeState;
use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::{SelectMode, Selection};
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::{Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    invalidate_selection_for_content,
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    window_dpi, with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState,
    UserEvent, WindowState,
};

impl App {
    /// Convert a VIEWPORT row (0 = top visible row, as returned by
    /// `GpuRenderer::pixel_to_cell`) to a scrollback-ABSOLUTE row for the
    /// focused pane, so a `Selection` tracks the same TEXT as the viewport
    /// scrolls. Resolves the pane's view top under the same `try_lock`
    /// discipline as the selection helpers (CLAUDE.md §4) and drops the lock
    /// before returning. Returns `None` when there is no active pane or the
    /// parser is busy; callers fall back to treating the viewport row as
    /// absolute (correct while unscrolled).
    pub(super) fn viewport_row_selection_state(
        &self,
        viewport_row: u16,
    ) -> Option<(u64, u64, u64, bool, u64)> {
        let pane_id = self.active_pane_id()?;
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let state = (
            view_top + viewport_row as u64,
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        Some(state)
    }

    /// Compute a word selection (double-click) at scrollback-ABSOLUTE
    /// `abs_row` / `col` from the focused pane's grid. Locks the parser only
    /// long enough to read the grid and build the `Selection`, drops it, then
    /// returns the owned (Copy) value — so callers never hold the parser lock
    /// across `selection_set`/redraw (CLAUDE.md §4). Falls back to a point
    /// selection when the parser is busy.
    pub(super) fn word_selection_at(&self, abs_row: u64, col: u16) -> Selection {
        let Some(pane_id) = self.active_pane_id() else {
            // When: active_pane_id resolves to nothing; fall back to a point selection
            // so a double-click still anchors at the clicked cell.
            return Selection::new(abs_row, col);
        };
        let Some(pane) = self.pane_by_id(pane_id) else {
            // When: pane_by_id cannot resolve pane_id; without a grid the word bounds
            // cannot be computed, so the point selection stands in.
            return Selection::new(abs_row, col);
        };
        let Some(guard) = pane.parser.try_lock() else {
            // When: try_lock finds the parser busy; the render path never blocks on
            // it, so return a point selection rather than stall on the word lookup.
            return Selection::new(abs_row, col);
        };
        let grid = guard.grid();
        let sel = Selection::word_at(grid, abs_row, col).with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        sel
    }

    /// Compute a line selection (triple-click) at scrollback-ABSOLUTE
    /// `abs_row` from the focused pane's grid. Same lock discipline as
    /// [`Self::word_selection_at`].
    pub(super) fn line_selection_at(&self, abs_row: u64) -> Selection {
        let Some(pane_id) = self.active_pane_id() else {
            // When: active_pane_id resolves to nothing; fall back to a point selection
            // so a triple-click still anchors somewhere instead of being dropped.
            return Selection::new(abs_row, 0);
        };
        let Some(pane) = self.pane_by_id(pane_id) else {
            // When: pane_by_id cannot resolve pane_id; without a grid the row extent
            // is unknown, so anchor a point selection at the clicked row.
            return Selection::new(abs_row, 0);
        };
        let Some(guard) = pane.parser.try_lock() else {
            // When: try_lock finds the parser busy; the render path never blocks on
            // it, so a point selection stands in rather than stalling the click.
            return Selection::new(abs_row, 0);
        };
        let grid = guard.grid();
        let sel = Selection::line_at(grid, abs_row).with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        sel
    }

    /// Word-mode drag (double-click then drag): union of the word at the
    /// scrollback-ABSOLUTE `anchor` cell and the word at the cursor cell.
    /// `cursor_viewport_row` is the live viewport row from `pixel_to_cell`;
    /// it is converted to an absolute row against the pane's current view top
    /// inside the same lock. Returns `None` when there is no active pane or
    /// the parser is busy — the caller then SKIPS this move rather than
    /// collapsing the selection (a cell-extend would shrink the word/line
    /// region). Same `try_lock`-then-drop discipline as
    /// [`Self::word_selection_at`] (CLAUDE.md §4): the grid lock is held
    /// only to build the owned (Copy) `Selection`, never across redraw.
    pub(super) fn word_drag_selection_at(
        &self,
        anchor: (u64, u16),
        cursor_viewport_row: u16,
        col: u16,
    ) -> Option<Selection> {
        let pane_id = self.active_pane_id()?;
        let pane = self.pane_by_id(pane_id)?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::word_drag(grid, anchor, (cursor_abs, col)).with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        Some(sel)
    }

    /// Line-mode drag (triple-click then drag): whole rows from the
    /// scrollback-ABSOLUTE `anchor_row` to the cursor row inclusive.
    /// `cursor_viewport_row` is converted to an absolute row inside the lock.
    /// Returns `None` when there is no active pane or the parser is busy —
    /// the caller SKIPS this move (see [`Self::word_drag_selection_at`]).
    pub(super) fn line_drag_selection_at(
        &self,
        anchor_row: u64,
        cursor_viewport_row: u16,
    ) -> Option<Selection> {
        let pane_id = self.active_pane_id()?;
        let pane = self.pane_by_id(pane_id)?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::line_drag(grid, anchor_row, cursor_abs).with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        Some(sel)
    }

    /// Refresh the main window's URI or validated-path hover state.
    pub(super) fn refresh_hovered_url(&mut self) {
        if let Some(window_id) = self.main_window_id {
            self.refresh_target_hover(window_id);
        }
    }

    /// Whether the child window currently holds its platform open modifier.
    pub(super) fn child_url_open_modifier_held(&self, win_id: winit::window::WindowId) -> bool {
        self.open_modifier_held(win_id)
    }

    /// Refresh one child window's URI or validated-path hover state.
    pub(super) fn refresh_hovered_url_in_child(&mut self, win_id: winit::window::WindowId) {
        self.refresh_target_hover(win_id);
    }

    pub(super) fn open_ssh_pane(&mut self, target: &str) {
        match sonicterm_io::ssh::parse_target(target) {
            Ok(parsed) => {
                // When: parse_target accepts the target; the result is only logged,
                // since no build wires an SSH pane to a backend yet.
                #[cfg(feature = "ssh")]
                {
                    tracing::info!("ssh: connecting to {parsed} (pane backend wiring pending)");
                }
                #[cfg(not(feature = "ssh"))]
                {
                    tracing::warn!(
                        "ssh: target {parsed} parsed OK, but this build does not \
                         include the `ssh` feature; rebuild with --features ssh"
                    );
                }
                let _ = parsed; // silence unused-var when neither cfg branch above touches it
            }
            Err(e) => {
                tracing::warn!("ssh: invalid target {target:?}: {e}");
            }
        }
    }
    pub(super) fn enter_copy_mode_for_kind(&mut self, kind: FrontmostKind) {
        let Some(pane_id) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind finds no pane for this kind; copy mode
            // has no grid to anchor its cursor in, so the request is dropped.
            return;
        };
        let Some(pane) = self.pane_by_id(pane_id) else {
            // When: pane_by_id cannot resolve pane_id; the pane closed before the
            // action ran, so there is no cursor position to seed copy mode with.
            return;
        };
        let cursor = {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            (grid.cursor.col as usize, grid.scrollback_len() + grid.cursor.row as usize)
        };
        let state = Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at(cursor));
        match kind {
            FrontmostKind::Child(id) => {
                if let Some(child) = self.windows.get_mut(&id) {
                    child.copy_mode = state;
                    mark_all_panes_dirty(&child.panes);
                }
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                self.copy_mode_set(state);
                if let Some(panes) = self.main_panes() {
                    mark_all_panes_dirty(panes);
                }
            }
        }
    }

    pub(super) fn enter_quick_select(&mut self) {
        let Some(pane) = self.active_pane() else {
            // When: active_pane resolves to nothing; quick-select needs a live grid
            // to build its label set, so the overlay is not entered.
            return;
        };
        let state = {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            let mut state =
                sonicterm_ui::copy_mode::CopyModeState::new_at((0, grid.scrollback_len()));
            state.quick_select = Some(sonicterm_ui::copy_mode::QuickSelectState::from_grid(grid));
            state
        };
        self.copy_mode_set(Some(state));
        if let Some(panes) = self.main_panes() {
            mark_all_panes_dirty(panes);
        }
    }

    pub(super) fn copy_selection_for_kind(&mut self, kind: FrontmostKind) {
        let window_id = match kind {
            FrontmostKind::Child(id) => id,
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                // When: kind is any FrontmostKind other than Child; all three resolve
                // to the main window, which owns the selection being copied.
                let Some(id) = self.main_window_id else {
                    // When: main_window_id is unset; no main window has been created
                    // yet, so there is no window to copy a selection from.
                    return;
                };
                id
            }
        };
        let text = {
            let Some(window) = self.windows.get_mut(&window_id) else {
                // When: windows no longer holds window_id; the window closed before
                // the copy ran, so there is no selection left to read.
                return;
            };
            let Some(pane_id) =
                window.tab_states.get(window.tabs.active_index()).map(|state| state.active_pane)
            else {
                // When: tab_states has no entry at the active index; without a tab
                // there is no active pane whose grid could supply the text.
                return;
            };
            let Some(pane) = window.panes.get(&pane_id) else {
                // When: panes no longer holds pane_id; the pane closed between the
                // selection and the copy, so there is no grid to read text from.
                return;
            };
            let parser = pane.parser.lock();
            let grid = parser.grid();

            // PTY output can arrive after the user selects text but before the
            // queued redraw gets a turn. Validate here too so Cmd+C cannot copy
            // replacement content during that window. This mutates only the
            // normal window selection; copy-mode and search overlays are
            // independent state.
            if invalidate_selection_for_content(&mut window.selection, pane_id, grid) {
                // When: invalidate_selection_for_content cleared the selection; the
                // rows it covered no longer hold the text the user chose.
                return;
            }
            let Some(selection) = window.selection else {
                // When: window.selection is None; nothing is selected, so there is
                // no text to place on the clipboard.
                return;
            };
            if selection.is_empty() {
                // When: selection is empty; a bare click leaves a zero-width range,
                // and copying it would clear the clipboard.
                return;
            }
            selection.as_text(grid)
        };
        self.set_clipboard_text(text);
    }

    pub(super) fn set_clipboard_text(&mut self, text: String) {
        if text.is_empty() {
            // When: text is empty; writing it would clear the user's clipboard,
            // so leave the previous contents intact.
            return;
        }
        if self.test_clipboard_text.is_some() {
            // When: test_clipboard_text is set; the suite captures the copy here
            // so no real system clipboard is touched by a test run.
            self.test_clipboard_text = Some(text.clone());
            return;
        }
        if let Some(cb) = self.clipboard.as_mut() {
            if let Err(e) = cb.set_text(text.clone()) {
                tracing::warn!("clipboard set failed: {e}");
            } else {
                // When: set_text succeeded; record the byte count so a silent
                // clipboard failure stays distinguishable from a real copy.
                tracing::info!("copied {} bytes", text.len());
            }
        }
    }
    pub(super) fn paste_clipboard_for_kind(&mut self, kind: FrontmostKind) {
        let text = if let Some(text) = self.test_clipboard_text.clone() {
            Some(text)
        } else {
            // When: test_clipboard_text is unset; read the real system clipboard,
            // whose get_text error is discarded and reads as nothing to paste.
            self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
        };
        let Some(text) = text else {
            // When: text is None; neither the test override nor the system
            // clipboard yielded anything, so there is nothing to paste.
            return;
        };
        let Some(pane_id) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind finds no pane for this kind; there is
            // no PTY to paste into, so the clipboard text is dropped.
            return;
        };
        let bracketed = self
            .pane_by_id(pane_id)
            .map(|p| p.parser.lock().bracketed_paste_enabled())
            .unwrap_or(false);
        let bytes = wrap_paste(&text, bracketed);
        if !sonicterm_io::pty::pty_input_message_allowed(bytes.len()) {
            // When: pty_input_message_allowed rejects the wrapped clipboard text;
            // warn with the size rather than push a message the writer refuses.
            self.show_notification_for_kind(
                kind,
                sonicterm_ui::overlays::NotificationLevel::Warning,
                format!(
                    "Paste is {:.1} MiB; maximum is {} MiB",
                    bytes.len() as f64 / (1024.0 * 1024.0),
                    sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES / (1024 * 1024)
                ),
            );
            return;
        }
        self.write_to_pane(pane_id, bytes.clone());
        self.broadcast_from(pane_id, bytes);
    }

    pub(super) fn paste_file_paths_for_kind<I>(&mut self, kind: FrontmostKind, paths: I)
    where
        I: IntoIterator<Item = std::path::PathBuf>,
    {
        let quoted = paths
            .into_iter()
            .map(|p| shell_quote_posix(&p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        if quoted.is_empty() {
            // When: quoted is empty; the drop yielded no paths, so a paste would
            // send only the bracketed-paste wrapper to the shell.
            return;
        }
        let Some(pane_id) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind finds no pane for this kind; there is
            // no PTY to receive the dropped paths, so discard them.
            return;
        };
        let bracketed = self
            .pane_by_id(pane_id)
            .map(|p| p.parser.lock().bracketed_paste_enabled())
            .unwrap_or(false);
        let bytes = wrap_paste(&quoted, bracketed);
        if !sonicterm_io::pty::pty_input_message_allowed(bytes.len()) {
            // When: pty_input_message_allowed rejects the wrapped paths; warn the
            // user rather than push a message the PTY writer would refuse.
            self.show_notification_for_kind(
                kind,
                sonicterm_ui::overlays::NotificationLevel::Warning,
                format!(
                    "Dropped paths require {:.1} MiB; maximum is {} MiB",
                    bytes.len() as f64 / (1024.0 * 1024.0),
                    sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES / (1024 * 1024)
                ),
            );
            return;
        }
        self.write_to_pane(pane_id, bytes.clone());
        self.broadcast_from(pane_id, bytes);
    }
    pub(super) fn scroll_to_prompt(&mut self, forward: bool) {
        let updated = {
            let Some(ws) = self.main_mut() else {
                // When: main_mut has no WindowState; there is no viewport to move,
                // so leave `updated` unset and skip the redraw below.
                return;
            };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get(i) else {
                // When: tab_states has no entry at the active index; without a tab
                // there is no pane whose prompt rows could be searched.
                return;
            };
            let pane_id = st.active_pane;
            let Some(pane) = ws.panes.get_mut(&pane_id) else {
                // When: panes no longer holds pane_id; the tab's active pane closed,
                // so there is no grid to search for a prompt row.
                return;
            };
            let new_top = {
                let guard = pane.parser.lock();
                let grid = guard.grid();
                let cur = pane.viewport_top_abs.unwrap_or_else(|| grid.scrollback_len() as u64);
                pick_prompt_target(grid, cur, forward)
            };
            if let Some(top) = new_top {
                pane.viewport_top_abs = Some(top);
                tracing::info!(target = top, "scrolled to prompt row");
                true
            } else {
                // When: new_top is None; pick_prompt_target found no prompt row in
                // this direction, so the viewport stays put and no redraw is asked for.
                false
            }
        };
        if updated {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }
    pub(super) fn drain_pending_window_creates(&mut self, el: &ActiveEventLoop) {
        if self.pending_new_window {
            self.pending_new_window = false;
            self.create_new_terminal_window(el);
        }
        // In-process tear-out drain. The `Command::new`-based spawn
        // (`spawn_tearout_child` + `--tear-out-payload`) is still reached from
        // the Windows OLE drop path, so both routes exist.
        // This drain MUST stay before `drain_pending_os_teardown`, so
        // `cancel_drag_session` sees the new child window already inserted.
        if let Some(req) = self.pending_tear_out.take() {
            self.drain_pending_tear_out(el, req);
        }
    }

    /// Resolve a queued tear-out into a native window while the event loop can
    /// create one. The live tab, pane graph, and PTY move without restarting.
    /// Where the tab a queued tear-out names currently sits.
    ///
    /// The index in the request was recorded when the gesture began, and a tab
    /// closing at a lower index since then leaves it *in range* while naming a
    /// different tab — a bounds check cannot tell the two apart, so trusting it
    /// tears out whichever tab inherited the slot. Resolving through the
    /// recorded id instead follows the tab, and returns `None` when it is gone
    /// so the operation fails rather than promoting a neighbour.
    ///
    /// Split out of the drain so it can be tested: the drain itself needs an
    /// `ActiveEventLoop`, which exists only inside a running winit loop.
    pub(super) fn resolve_tear_out_source_index(
        &self,
        req: &crate::app::PendingTearOut,
    ) -> Option<usize> {
        match req.source_tab_id {
            Some(id) => self.tab_index_of_id(req.source_window, id),
            // Requests built before an id was available keep the old
            // behaviour rather than refusing outright.
            None => Some(req.source_tab_idx),
        }
    }

    fn drain_pending_tear_out(&mut self, el: &ActiveEventLoop, req: crate::app::PendingTearOut) {
        let source_is_main = Some(req.source_window) == self.main_window_id;
        let source_tab_idx = match self.resolve_tear_out_source_index(&req) {
            Some(idx) => idx,
            None => {
                // When: resolve_tear_out_source_index cannot find the recorded tab
                // id; abandon rather than tear out whichever tab inherited the slot.
                tracing::warn!(
                    source = ?req.source_window,
                    recorded_idx = req.source_tab_idx,
                    "drain_pending_tear_out: the dragged tab closed before the drop"
                );
                return;
            }
        };
        let detached = if source_is_main {
            self.detach_tab_state(source_tab_idx)
        } else {
            // When: source_is_main is false; the drag began in a child window, so
            // detach from that child's tab list rather than the main window's.
            self.detach_from_child(req.source_window, source_tab_idx)
        };
        let Some((tab, state, panes)) = detached else {
            // When: detached is None; the source tab vanished between resolving its
            // index and detaching it, so there is nothing to install.
            tracing::warn!(
                source = ?req.source_window,
                idx = source_tab_idx,
                "drain_pending_tear_out: source tab no longer exists"
            );
            return;
        };
        let source = if source_is_main { "main" } else { "child" };
        if self
            .install_torn_out_window(el, tab, state, panes, req.drop_screen_pos, source)
            .is_none()
        {
            // When: install_torn_out_window failed after consuming tab, state, and
            // panes, so those shells are already dropped; only a child gets repaired.
            tracing::warn!(source = ?req.source_window, "drain_pending_tear_out: install failed");
            if !source_is_main {
                self.tear_out_apply_child_source_side(req.source_window, source_tab_idx);
            }
            return;
        }
        if source_is_main {
            self.tear_out_apply_source_side(source_tab_idx);
        } else {
            // When: source_is_main is false; repair the child the tab left, reaping
            // it when empty and otherwise activating the removed slot's neighbour.
            self.tear_out_apply_child_source_side(req.source_window, source_tab_idx);
        }
        tracing::info!(
            source = ?req.source_window,
            at = ?req.drop_screen_pos,
            "in-process tear-out completed"
        );
    }

    /// Drain a deferred `cancel_drag_session` request raised by
    /// `handle_os_drag_ended` on the `DroppedOnEmpty` branch. Callers MUST
    /// invoke this AFTER [`Self::drain_pending_window_creates`] so any
    /// tear-out-spawn has produced its new window before cross-window
    /// drag-residue cleanup mutates `self.windows`. The all-windows
    /// loop inside `cancel_drag_session` still runs UNCONDITIONALLY
    /// when this drain fires, preserving that cleanup's idempotence —
    /// the flag controls WHEN, not WHETHER.
    pub(super) fn drain_pending_os_teardown(&mut self) {
        if self.pending_os_teardown {
            self.pending_os_teardown = false;
            self.cancel_drag_session();
        }
    }

    /// Create a fresh top-level terminal window, install its renderer,
    /// spawn one tab + PTY-backed pane, register it with the OS-drag
    /// backend, and mark it as the new frontmost window.
    ///
    /// CRITICAL: this must work whether `self.windows` is empty or not.
    /// On macOS with `quit_on_last_window_close = false`, the process
    /// stays alive after the user closes the last window (dock icon +
    /// native menubar), so Cmd+N from that empty-windows state must
    /// still spawn a fresh terminal rather than do nothing.
    pub(super) fn create_new_terminal_window(&mut self, el: &ActiveEventLoop) {
        use sonicterm_ui::tabs::Tab;

        let attrs = super::with_app_icon(super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::NATIVE_WINDOW_TITLE)
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0)),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                // When: create_window is refused by the OS; log and leave the
                // existing windows running rather than aborting the process.
                tracing::error!("Action::NewWindow: create_window failed: {e}");
                return;
            }
        };
        window.set_ime_allowed(true);

        let mut renderer = match GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            sonicterm_gpu::core::RendererSettings {
                font_family: &self.config.font.family,
                font_dirs: &self.font_dirs,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                font_weight_scale: self.config.font.effective_weight_scale(),
                padding: [
                    self.config.window.padding_left,
                    self.config.window.padding_right,
                    self.config.window.padding_top,
                    self.config.window.padding_bottom,
                ],
                appearance: sonicterm_gpu::core::SurfaceAppearance {
                    backdrop: self.config.appearance.backdrop,
                    opacity: self.config.appearance.opacity,
                    scrollbar: self.config.appearance.scrollbar,
                    panel_padding: self.config.appearance.panel_padding,
                    software_render_mode: self.config.appearance.software_render_mode,
                },
                role: "child",
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                // When: GpuRenderer::new fails; return before the window is
                // registered, so it closes instead of showing nothing forever.
                tracing::error!("Action::NewWindow: renderer init failed: {e}");
                return;
            }
        };
        if !self.configure_child_renderer(
            &mut renderer,
            &window,
            super::tear_out::ChildRendererOrigin::Fresh,
        ) {
            // When: configure_child_renderer rejects the surface size; drop the
            // window rather than register one whose cell grid would be unusable.
            let real_inner = window.inner_size();
            tracing::error!(
                width = real_inner.width,
                height = real_inner.height,
                "Action::NewWindow rejected unsafe initial size"
            );
            return;
        }

        let (cols, rows) = renderer.cells();
        let pane_id = super::next_pane_id();
        let pane_state = self.spawn_pane_state_for_child(pane_id, cols, rows, window.clone());
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane_state);

        let mut tabs = TabBar::new();
        tabs.push(Tab::new("shell 1".to_string()));

        let win_id = window.id();
        let child = WindowState {
            // Registered when the window is inserted; construction has no
            // governor in scope.
            owner: None,
            role: crate::app::WindowRole::Terminal,
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs,
            tab_states: vec![TabState::new(PaneTree::leaf(pane_id), pane_id)],
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            path_probe: super::path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(win_id, child);
        self.register_window_with_os_drag_backend(win_id, &window);
        window.request_redraw();
        // Eagerly mark frontmost so the next Cmd+T / Cmd+W routes
        // here before the OS Focus event arrives — mirrors the
        // tear_out_tab pattern.
        self.frontmost_window = Some(win_id);
        tracing::info!(
            "Action::NewWindow: spawned terminal window; windows={}",
            self.windows.len()
        );
    }
    // Ordering: redraw_request_count fetch_add is Relaxed; the counter is only
    // incremented, never loaded, so it publishes no other memory.
    pub(super) fn drain_menubar_actions(&mut self, el: &ActiveEventLoop) {
        let mut ran_any = false;
        for action in crate::menubar_bridge::drain() {
            tracing::debug!("menubar action: {action:?}");
            self.run_action(&action);
            ran_any = true;
        }
        // Menubar dispatch can set window-creation flags. Funnel through
        // the single drain helper so every dispatch site is covered. See
        // `drain_pending_window_creates`.
        self.drain_pending_window_creates(el);
        // Request a redraw if any action ran. On macOS, NSMenu intercepts
        // chords like ⌘W and ⌘T before winit sees them and dispatches the
        // bound `Action` via this bridge instead of the KeyboardInput arm
        // in `window_event`. The KeyboardInput arm always follows
        // `run_action` with `window.request_redraw()`; this path used to
        // not, so a ⌘W "close tab" mutated state but left the tab bar
        // looking unchanged on screen until the *next* unrelated event
        // (a second ⌘W, a mouse move, or PTY output) finally repainted.
        // Users perceived this as "Ctrl/Cmd+W needs two presses." Mirror
        // the keyboard path so the first press is visible immediately.
        if ran_any {
            self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }
    /// Test-only mirror of [`Self::drain_menubar_actions`] that omits the
    /// `ActiveEventLoop`-dependent window-creation drain, so the drain can be
    /// driven without a running winit loop. Bumps
    /// [`Self::redraw_request_count`] once per drained action batch, matching
    /// the first-press repaint contract in [`Self::drain_menubar_actions`].
    // Ordering: redraw_request_count fetch_add is Relaxed; the counter is only
    // incremented, never loaded, so it publishes no other memory.
    #[doc(hidden)]
    pub fn __test_drain_menubar_actions(&mut self) {
        let mut ran_any = false;
        for action in crate::menubar_bridge::drain() {
            self.run_action(&action);
            ran_any = true;
        }
        if ran_any {
            self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }
    pub(super) fn drain_os_drag(&mut self) {
        for payload in crate::os_drag_bridge::drain_tab_payloads() {
            let idx = self.new_tab_from_payload(&payload);
            tracing::info!(idx, "spawned tab from OS-drag payload");
        }
        self.drain_pending_os_drag_payloads();
        let drops = crate::os_drag_bridge::drain_file_drops();
        if drops.is_empty() {
            // When: drops is empty; no file drop arrived, so skip the paste and
            // the redraw that only a real drop needs.
            return;
        }
        for paths in drops {
            self.paste_file_paths_for_kind(self.frontmost_kind(), paths);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }
    pub(super) fn new_tab(&mut self, title: impl Into<String>) {
        self.new_tab_with_launch(title, super::pane_launch::PaneLaunch::default());
    }

    pub(super) fn new_tab_with_launch(
        &mut self,
        title: impl Into<String>,
        launch: super::pane_launch::PaneLaunch,
    ) {
        let pane_id = next_pane_id();
        let pane = self.spawn_pane(pane_id, &launch);
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, pane);
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        // Own the new pane now: until it has one, its memory is attributed to
        // nothing and the next 30-second sample is a long time to be blind.
        self.reconcile_pane_owners();
    }
    pub(super) fn close_tab_at(&mut self, index: usize) {
        let Some(ws) = self.main_mut() else {
            // When: main_mut has no WindowState; there is no tab list to close
            // from, so the request is dropped rather than treated as an error.
            return;
        };
        if index >= ws.tab_states.len() {
            // When: index is past the end of tab_states; a close request that
            // outlived its tab would panic in Vec::remove, so it is dropped.
            return;
        }
        let st = ws.tab_states.remove(index);
        let tab_id = ws.tabs.tabs().get(index).map(|t| t.id);
        if let Some(id) = tab_id {
            ws.tabs.close(id);
        }
        for id in st.tree.leaves() {
            ws.panes.remove(&id);
        }
    }
    pub(super) fn drain_pending_os_drag_payloads(&mut self) {
        if self.main_mut().is_none() || self.pending_os_drag_payloads.is_empty() {
            // When: main_mut has no WindowState or pending_os_drag_payloads is
            // empty; nothing can be replayed, so the queue waits for a later drain.
            return;
        }
        let pending = std::mem::take(&mut self.pending_os_drag_payloads);
        for payload in pending {
            let idx = self.new_tab_from_payload(&payload);
            tracing::info!(idx, "spawned queued OS-drag payload");
        }
    }

    pub fn new_tab_from_payload(&mut self, payload: &crate::os_drag::TabPayload) -> usize {
        if self.main_mut().is_none() {
            // When: main_mut has no WindowState yet; queue the payload so a drop
            // arriving before the main window exists is replayed, not lost.
            self.pending_os_drag_payloads.push(payload.clone());
            tracing::info!(
                tab = %payload.tab_title,
                "os_drag: queued payload until main WindowState exists"
            );
            return self.main_tabs().map(|t| t.len().saturating_sub(1)).unwrap_or(0);
        }

        let title = if payload.tab_title.is_empty() {
            "received tab".to_string()
        } else {
            // When: the payload carries a tab_title; reuse the source tab's name
            // so a dragged tab keeps its identity on the receiving window.
            payload.tab_title.clone()
        };
        self.new_tab(title);
        tracing::info!(
            tab = %payload.tab_title,
            "os_drag: received payload; spawned destination tab"
        );
        self.main_tabs().map(|t| t.len().saturating_sub(1)).unwrap_or(0)
    }
}
