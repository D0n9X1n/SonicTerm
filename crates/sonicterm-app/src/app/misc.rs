//! Extracted from `app/mod.rs` from the monolithic app module.
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use sonicterm_ui::ime::ImeState;
use sonicterm_ui::status_line::InputState;
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
use sonicterm_io::pty::{PtyHandle, MAX_PTY_INPUT_MESSAGE_BYTES};
use sonicterm_types::{
    classify_shell, encode_payload, PasteRefusal, PasteTarget, ShellDialect, UserPayload,
};
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

use super::viewport_anchor::ViewportBaseline;
use super::{
    invalidate_selection_for_content,
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    window_dpi, with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState,
    UserEvent, WindowState,
};

impl App {
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
            let guard = crate::app::frame_counters::lock_parser(&pane.parser);
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
        self.enter_quick_select_for_kind(self.frontmost_kind());
    }

    pub(super) fn enter_quick_select_for_kind(&mut self, kind: FrontmostKind) {
        let Some(pane) = self.active_pane_id_for_kind(kind).and_then(|id| self.pane_by_id(id))
        else {
            // When: active_pane_id_for_kind or pane_by_id has no live grid, quick-select cannot construct its labels.
            return;
        };
        let (state, rows_scanned) = {
            let guard = crate::app::frame_counters::lock_parser(&pane.parser);
            let grid = guard.grid();
            // Quick select labels what the pane displays: its resolved viewport, history included.
            let view_top = sonicterm_gpu::core::GpuRenderer::resolved_view_top_abs_legacy(
                grid,
                pane.resolved_viewport(grid),
            ) as usize;
            let mut state = sonicterm_ui::copy_mode::CopyModeState::new_at((0, view_top));
            // The scan stops at the 27th URL, so it reports the rows it actually read.
            let (quick_select, rows_visited) =
                sonicterm_ui::copy_mode::QuickSelectState::scan_viewport(grid, view_top);
            state.quick_select = Some(quick_select);
            (state, rows_visited)
        };
        self.note_hover(|hover| hover.quick_select_rows_scanned += rows_scanned);
        let window_id = match kind {
            FrontmostKind::Child(id) => Some(id),
            _ => self.main_window_id,
        };
        if let Some(window) = window_id.and_then(|id| self.windows.get_mut(&id)) {
            window.copy_mode = Some(state);
            mark_all_panes_dirty(&window.panes);
            window.request_window_redraw();
        }
    }
    pub(super) fn scroll_to_prompt(&mut self, forward: bool) {
        let updated = {
            let Some(main) = self.main_mut() else {
                // When: main_mut has no WindowState; there is no viewport to move,
                // so leave `updated` unset and skip the redraw below.
                return;
            };
            let tab_index = main.tabs.active_index();
            let Some(tab) = main.tab_states.get(tab_index) else {
                // When: tab_states has no entry at the active index; without a tab
                // there is no pane whose prompt rows could be searched.
                return;
            };
            let pane_id = tab.active_pane;
            let Some(pane) = main.panes.get_mut(&pane_id) else {
                // When: panes no longer holds pane_id; the tab's active pane closed,
                // so there is no grid to search for a prompt row.
                return;
            };
            let (new_top, at) = {
                let guard = crate::app::frame_counters::lock_parser(&pane.parser);
                let grid = guard.grid();
                let live_top = grid.scrollback_len() as u64;
                let cur = pane.resolved_viewport(grid).unwrap_or(live_top);
                (pick_prompt_target(grid, cur, forward), ViewportBaseline::of(grid))
            };
            if let Some(top) = new_top {
                pane.set_viewport_top_at(at, Some(top));
                tracing::info!(target = top, "scrolled to prompt row");
                true
            } else {
                // When: new_top is None; pick_prompt_target found no prompt row in
                // this direction, so the viewport stays put and no redraw is asked for.
                false
            }
        };
        if updated {
            if let Some(window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(window);
            }
        }
    }
    // Ordering: redraw_request_count fetch_add is Relaxed; the counter is only
    // incremented, never loaded, so it publishes no other memory.
    pub(super) fn drain_menubar_actions(&mut self, event_loop: &ActiveEventLoop) {
        let mut ran_any = false;
        for action in crate::menubar_bridge::drain() {
            tracing::debug!("menubar action: {action:?}");
            self.run_action(&action);
            ran_any = true;
        }
        // Menubar dispatch can set window-creation flags. Funnel through
        // the single drain helper so every dispatch site is covered. See
        // `drain_pending_window_creates`.
        self.drain_pending_window_creates(event_loop);
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
            if let Some(window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(window);
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
            if let Some(window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(window);
            }
        }
    }
    /// Retain one winit path in its source window's current-turn drop batch.
    pub(super) fn collect_winit_file_drop(
        &mut self,
        window_id: WindowId,
        path: std::path::PathBuf,
    ) {
        let Some(pane) = self
            .windows
            .get(&window_id)
            .and_then(|window| window.tab_states.get(window.tabs.active_index()))
            .map(|tab| tab.active_pane)
        else {
            // When: window_id has no active pane, consume the drop without a frontmost fallback.
            return;
        };
        if !self.admits_new_user_input(pane) {
            // When: pane is READONLY at arrival, do not defer its drop until a later writable state.
            self.note_pane_input(pane, InputState::Blocked(1));
            return;
        }
        self.pending_winit_file_drops.entry(window_id).or_default().push(path);
    }

    /// Deliver each window's current-turn winit paths as one paste, discarding closed or newly READONLY targets.
    pub(super) fn drain_winit_file_drops(&mut self) {
        for (window_id, paths) in std::mem::take(&mut self.pending_winit_file_drops) {
            self.paste_file_paths_in_window(window_id, paths);
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
        for (window_id, paths) in drops {
            self.paste_file_paths_in_window(window_id, paths);
        }
    }

    pub(super) fn paste_file_paths_in_window(
        &mut self,
        window_id: WindowId,
        paths: Vec<std::path::PathBuf>,
    ) {
        if !self.windows.contains_key(&window_id) {
            // When: windows no longer contains window_id, discard its drop instead of selecting current focus.
            return;
        }
        self.paste_file_paths_for_kind(self.kind_for(window_id), paths);
        if let Some(window) = self.windows.get(&window_id) {
            window.request_window_redraw();
        }
    }

    pub(super) fn new_tab(&mut self, title: impl Into<String>) {
        let launch = super::pane_launch::PaneLaunch::from_window(self.main(), &self.local_hostname);
        self.new_tab_with_launch(title, launch);
    }

    pub(super) fn new_tab_with_launch(
        &mut self,
        title: impl Into<String>,
        launch: super::pane_launch::PaneLaunch,
    ) {
        let pane_id = next_pane_id();
        let pane = self.spawn_pane(pane_id, &launch);
        if let Some(main) = self.main_mut() {
            main.panes.insert(pane_id, pane);
            main.tabs.push(Tab::new(title));
            main.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        } else {
            // When: main_mut has no destination, retire the spawned pane rather than dropping native custody inline.
            self.retire_pane(pane);
        }
        self.resize_visible_panes();
    }
    pub(super) fn close_tab_at(&mut self, index: usize) {
        let Some(main) = self.main_mut() else {
            // When: main_mut has no WindowState; there is no tab list to close
            // from, so the request is dropped rather than treated as an error.
            return;
        };
        if index >= main.tab_states.len() {
            // When: index is past the end of tab_states; a close request that
            // outlived its tab would panic in Vec::remove, so it is dropped.
            return;
        }
        let tab_state = main.tab_states.remove(index);
        let tab_id = main.tabs.tabs().get(index).map(|tab| tab.id);
        if let Some(id) = tab_id {
            main.tabs.close(id);
        }
        let retired: Vec<_> =
            tab_state.tree.leaves().into_iter().filter_map(|id| main.remove_pane(id)).collect();
        for pane in retired {
            self.retire_pane(pane);
        }
        self.resize_visible_panes();
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
            return self.main_tabs().map(|tabs| tabs.len().saturating_sub(1)).unwrap_or(0);
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
        self.main_tabs().map(|tabs| tabs.len().saturating_sub(1)).unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "misc_tests.rs"]
mod misc_tests;

#[path = "misc_clipboard.rs"]
mod clipboard;
#[path = "misc_window_create.rs"]
mod window_create;

impl App {
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
        let (text, clear_after_copy) = {
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
            let parser = crate::app::frame_counters::lock_parser(&pane.parser);
            let grid = parser.grid();

            // PTY output can arrive after the user selects text but before the
            // queued redraw gets a turn. Validate here too so Cmd+C cannot copy
            // replacement content during that window. This mutates only the
            // normal window selection; copy-mode and search overlays are
            // independent state.
            if invalidate_selection_for_content(
                &mut window.selection,
                &mut window.select_anchor,
                pane_id,
                grid,
            ) {
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
            (selection.as_text(grid), selection.on_alt_screen)
        };
        let copied = self.set_clipboard_text(text);
        self.note_copy_result(window_id, copied);
        if copied && clear_after_copy {
            // A volatile selection is consumed only after confirmed clipboard delivery.
            if let Some(window) = self.windows.get_mut(&window_id) {
                // Clear the highlight and invalidate retained rows before requesting its frame.
                window.selection = None;
                mark_all_panes_dirty(&window.panes);
                window.request_window_redraw();
            }
        }
    }
}

impl App {
    /// Encode each live admitted target, returning the attempted destination count and payload-free refusals.
    fn paste_payload_to_destinations(
        &mut self,
        source_pane: u64,
        payload: &UserPayload,
        source: super::PtyInputSource,
    ) -> (usize, Vec<PasteRefusal>) {
        if !self.admits_new_user_input(source_pane) {
            // When: source_pane is READONLY, consume the whole gesture rather than fan it out to writable peers.
            self.note_pane_input(source_pane, InputState::Blocked(1));
            return (0, Vec::new());
        }
        let mut destinations = vec![source_pane];
        if matches!(self.broadcast, sonicterm_ui::broadcast::BroadcastState::On { source_pane: pane, .. } if pane == source_pane)
        {
            // `source_pane` armed this broadcast, so append only its currently admitted receivers.
            let (receivers, blocked) = self.broadcast_receivers_counted();
            if blocked > 0 {
                // READONLY receivers were skipped; the strip names how many.
                self.note_pane_input(source_pane, InputState::Blocked(blocked));
            }
            destinations.extend(receivers);
        }
        let mut attempted = 0;
        let mut refusals = Vec::new();
        for pane_id in destinations {
            let Some(pane) = self.pane_by_id(pane_id) else {
                // When: pane_id is stale, it has no target state and must not select another pane.
                continue;
            };
            attempted += 1;
            // The short parser read ends here, before shell encoding or any queue admission.
            let bracketed =
                crate::app::frame_counters::lock_parser(&pane.parser).bracketed_paste_enabled();
            let dialect = pane
                .pty
                .as_ref()
                .map(|pty| classify_shell(pty.shell_program_path()))
                .unwrap_or(ShellDialect::Unknown);
            match encode_payload(
                payload,
                PasteTarget { bracketed, dialect },
                MAX_PTY_INPUT_MESSAGE_BYTES,
            ) {
                Ok(bytes) => {
                    self.write_to_pane(pane_id, bytes, source);
                }
                Err(refusal) => refusals.push(refusal),
            }
        }
        (attempted, refusals)
    }
}
