//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonic_core::{
    config::Config,
    grid::Grid,
    keymap::{Action, Direction, Keymap, ScrollAction},
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use sonic_shared::render::GpuRenderer;
use sonic_ui::pane::PaneTree;
use sonic_ui::selection::Selection;
use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent,
    WindowState,
};
use crate::app::integrated_titlebar_inset;

impl App {
    pub(super) fn hyperlink_uri_at(&self, row: u16, col: u16) -> Option<String> {
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        if row >= grid.rows || col >= grid.cols {
            return None;
        }
        let r = grid.row(row);
        // First: OSC 8 hyperlink interned on the cell itself.
        if let Some(hid) = r[col as usize].hyperlink() {
            let uri = guard.hyperlinks().lookup(hid).map(|h| h.uri.clone());
            drop(guard);
            return uri;
        }
        // Second: plain-text URL detection over the row's character
        // content. We render each cell as one column character, so the
        // column index maps 1-to-1 onto a `chars()` position in the
        // reconstructed row string. (Wide chars occupy two columns —
        // the trailing column is `' '` in the grid so it falls outside
        // a URL body match, which is the behavior we want: clicking the
        // right half of a CJK glyph won't accidentally pick up an
        // adjacent URL.)
        let mut row_text = String::with_capacity(grid.cols as usize);
        for i in 0..grid.cols {
            row_text.push(r[i as usize].ch);
        }
        drop(guard);
        sonic_core::url_scan::url_at_char_col(&row_text, col as usize).map(|m| m.url)
    }

    /// OSC 8-only lookup: returns the cell's interned hyperlink URI,
    /// ignoring auto-detected plain-text URLs. Used by the hover
    /// pointer-cursor logic so OSC 8 keeps its existing unconditional
    /// pointer affordance while auto-detected URLs are gated behind
    /// the Cmd / Ctrl open-URL modifier.
    pub(super) fn osc8_uri_at(&self, row: u16, col: u16) -> Option<String> {
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        if row >= grid.rows || col >= grid.cols {
            return None;
        }
        let r = grid.row(row);
        let hid = r[col as usize].hyperlink()?;
        guard.hyperlinks().lookup(hid).map(|h| h.uri.clone())
    }

    /// Reconstruct the focused pane's row string at `row` (one char
    /// per cell) for plain-text URL detection. Returns `None` when the
    /// parser is locked or the row is out of range.
    pub(super) fn focused_pane_row_text(&self, row: u16) -> Option<String> {
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        if row >= grid.rows {
            return None;
        }
        let r = grid.row(row);
        let mut s = String::with_capacity(grid.cols as usize);
        for i in 0..grid.cols {
            s.push(r[i as usize].ch);
        }
        Some(s)
    }

    /// Recompute `self.hovered_url` from the current cursor position
    /// and modifier state. Called on every `CursorMoved` and every
    /// `ModifiersChanged` so press / release / drift transitions all
    /// converge to the same source of truth.
    pub(super) fn refresh_hovered_url(&mut self) {
        let new_hover = self.compute_current_hovered_url();
        let changed = new_hover != self.hovered_url;
        self.hovered_url = new_hover;
        // Pointer-cursor transition: auto-detected URL needs the
        // open-URL modifier held; OSC 8 keeps its always-on pointer.
        let want_pointer = self.hovered_url.is_some()
            || self
                .renderer
                .as_ref()
                .and_then(|r| r.pixel_to_cell(self.cursor_pos.0 as f32, self.cursor_pos.1 as f32))
                .and_then(|(row, col)| self.osc8_uri_at(row, col))
                .is_some();
        if want_pointer != self.hover_link {
            self.hover_link = want_pointer;
            if let Some(w) = &self.window {
                w.set_cursor(if want_pointer { CursorIcon::Pointer } else { CursorIcon::Default });
            }
        }
        if changed {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn compute_current_hovered_url(&self) -> Option<super::hovered_url::HoveredUrl> {
        if !self.url_open_modifier_held() {
            return None;
        }
        let r = self.renderer.as_ref()?;
        let (row, col) = r.pixel_to_cell(self.cursor_pos.0 as f32, self.cursor_pos.1 as f32)?;
        // OSC 8 has its own affordance — don't double up.
        if self.osc8_uri_at(row, col).is_some() {
            return None;
        }
        let row_text = self.focused_pane_row_text(row)?;
        super::hovered_url::hovered_from_row(&row_text, row, col)
    }
    /// True iff the platform "open this in the browser" modifier is held.
    /// macOS: Cmd (super). Windows / Linux: Ctrl.
    pub(super) fn url_open_modifier_held(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key()
        }
    }
    pub(super) fn open_ssh_pane(&mut self, target: &str) {
        match sonic_core::ssh::parse_target(target) {
            Ok(parsed) => {
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
    pub(super) fn enter_copy_mode(&mut self) {
        let Some(pane) = self.active_pane() else { return };
        let cursor = {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            (grid.cursor.col as usize, grid.scrollback_len() + grid.cursor.row as usize)
        };
        self.copy_mode = Some(sonic_ui::copy_mode::CopyModeState::new_at(cursor));
        mark_all_panes_dirty(&self.panes);
    }

    pub(super) fn enter_quick_select(&mut self) {
        let Some(pane) = self.active_pane() else { return };
        let state = {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            let mut state = sonic_ui::copy_mode::CopyModeState::new_at((0, grid.scrollback_len()));
            state.quick_select = Some(sonic_ui::copy_mode::QuickSelectState::from_grid(grid));
            state
        };
        self.copy_mode = Some(state);
        mark_all_panes_dirty(&self.panes);
    }

    pub(super) fn copy_selection(&mut self) {
        let Some(sel) = self.selection.as_ref() else {
            return;
        };
        if sel.is_empty() {
            return;
        }
        let Some(pane) = self.active_pane() else { return };
        let text = sel.as_text(pane.parser.lock().grid());
        self.set_clipboard_text(text);
    }

    pub(super) fn set_clipboard_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(cb) = self.clipboard.as_mut() {
            if let Err(e) = cb.set_text(text.clone()) {
                tracing::warn!("clipboard set failed: {e}");
            } else {
                tracing::info!("copied {} bytes", text.len());
            }
        }
    }
    pub(super) fn paste_clipboard(&mut self) {
        if let Some(cb) = self.clipboard.as_mut() {
            if let Ok(text) = cb.get_text() {
                let bracketed = self
                    .active_pane()
                    .map(|p| p.parser.lock().bracketed_paste_enabled())
                    .unwrap_or(false);
                let bytes = wrap_paste(&text, bracketed);
                self.write_to_pty(bytes);
            }
        }
    }
    pub(super) fn scroll_to_prompt(&mut self, forward: bool) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get(i) else { return };
        let pane_id = st.active_pane;
        let Some(pane) = self.panes.get_mut(&pane_id) else { return };
        let new_top = {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            let cur = pane.viewport_top_abs.unwrap_or_else(|| grid.scrollback_len() as u64);
            pick_prompt_target(grid, cur, forward)
        };
        if let Some(top) = new_top {
            pane.viewport_top_abs = Some(top);
            tracing::info!(target = top, "scrolled to prompt row");
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
    pub(super) fn drain_pending_window_creates(&mut self, el: &ActiveEventLoop) {
        if self.pending_new_window {
            self.pending_new_window = false;
            self.create_new_terminal_window(el);
        }
    }

    /// Bug fix (Cmd+W on last tab): consume the `pending_exit` flag
    /// set by the keymap dispatcher when `CloseActivePaneOrTab`
    /// emptied the main window's tab list. Mirrors the post-tabbar-
    /// close logic in `window_event.rs` (`WindowEvent::MouseInput`
    /// → `TabHit::Close`): if no child windows are alive AND the
    /// config says we should exit on last-window-close, call
    /// `el.exit()`; otherwise hide the main window (dock-alive
    /// Chrome mode). Idempotent: the flag is cleared on every drain.
    pub(super) fn drain_pending_exit(&mut self, el: &ActiveEventLoop) {
        if !self.pending_exit {
            return;
        }
        self.pending_exit = false;
        if !self.tabs.is_empty() || !self.windows.is_empty() {
            // State changed between dispatch and drain (e.g. a new
            // tab opened or a child window appeared) — abort the
            // pending exit to be safe.
            return;
        }
        if Self::should_exit_on_last_window_close(&self.config) {
            el.exit();
        } else {
            self.hide_main_window();
        }
    }

    /// Epic #289 Phase E (Haiku follow-up on PR #297): create a fresh
    /// top-level terminal window, install its renderer, spawn one
    /// tab + PTY-backed pane, register it with the OS-drag backend,
    /// and mark it as the new frontmost window.
    ///
    /// CRITICAL: this must work whether `self.windows` is empty or
    /// not. The motivating bug: on macOS with
    /// `quit_on_last_window_close = false`, after the user closes
    /// the last window the process stays alive (dock icon + native
    /// menubar), but Cmd+N was a no-op → the user was stuck with no
    /// way to open a new window. After this fix, Cmd+N from the
    /// dock-alive empty-windows state spawns a fresh terminal.
    pub(super) fn create_new_terminal_window(&mut self, el: &ActiveEventLoop) {
        use sonic_ui::tabs::Tab;

        let attrs = super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title("Sonic")
                    .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0)),
            ),
            self.config.appearance.backdrop,
        );
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("Action::NewWindow: create_window failed: {e}");
                return;
            }
        };
        window.set_ime_allowed(true);

        let mut renderer = match GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            sonic_shared::render::RendererSettings {
                font_family: &self.config.font.family,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                padding: [
                    self.config.window.padding_left,
                    self.config.window.padding_right,
                    self.config.window.padding_top,
                    self.config.window.padding_bottom,
                ],
                appearance: sonic_shared::render::SurfaceAppearance {
                    backdrop: self.config.appearance.backdrop,
                    opacity: self.config.appearance.opacity,
                },
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Action::NewWindow: renderer init failed: {e}");
                return;
            }
        };
        // Epic #300 P4 follow-up wire (NewWindow path).
        if let Some(proxy) = self.event_loop_proxy.clone() {
            renderer.set_async_loader(super::build_async_fallback_loader_for_proxy(proxy));
        }
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);
        renderer.set_titlebar_inset(integrated_titlebar_inset());
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());
        let real_sf = window.scale_factor() as f32;
        renderer.force_rebuild_for_scale(real_sf);
        let real_inner = window.inner_size();
        renderer.resize(real_inner.width.max(1), real_inner.height.max(1));

        let (cols, rows) = renderer.cells();
        let cursor_visible_arc = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pane_state =
            self.spawn_pane_state_for_child(cols, rows, window.clone(), cursor_visible_arc.clone());
        let pane_id = super::next_pane_id();
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane_state);

        let mut tabs = TabBar::new();
        tabs.push(Tab::new("shell 1".to_string()));

        let win_id = window.id();
        let child = WindowState {
            role: crate::app::WindowRole::Terminal,
            window: window.clone(),
            renderer,
            tabs,
            tab_states: vec![TabState::new(PaneTree::leaf(pane_id), pane_id)],
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            cursor_visible: cursor_visible_arc,
            last_render: Instant::now(),
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
        };
        self.windows.insert(win_id, child);
        self.register_window_with_os_drag_backend(win_id, &window);
        window.request_redraw();
        // Eagerly mark frontmost so the next Cmd+T / Cmd+W routes
        // here before the OS Focus event arrives — mirrors the
        // tear_out_tab Phase B pattern.
        self.frontmost_window = Some(win_id);
        tracing::info!(
            "Action::NewWindow: spawned terminal window; windows={}",
            self.windows.len()
        );
    }
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
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
    /// Test-only mirror of [`Self::drain_menubar_actions`] that omits the
    /// `ActiveEventLoop`-dependent window-creation drain. Used by the
    /// `close_pane_or_tab_semantics` regression suite to assert that a
    /// menubar-bridged action increments [`Self::redraw_request_count`]
    /// exactly once per drained action batch — the contract PR #200
    /// added (Cmd+W "two presses" bug) and the PR #271 follow-up
    /// audit hardened with a real counter assertion.
    #[doc(hidden)]
    pub fn __test_drain_menubar_actions(&mut self) {
        let mut ran_any = false;
        for action in crate::menubar_bridge::drain() {
            self.run_action(&action);
            ran_any = true;
        }
        if ran_any {
            self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
    pub(super) fn drain_os_drag(&mut self) {
        for payload in crate::os_drag_bridge::drain_tab_payloads() {
            let idx = self.new_tab_from_payload(&payload);
            tracing::info!(idx, "spawned tab from OS-drag payload");
        }
        let drops = crate::os_drag_bridge::drain_file_drops();
        if drops.is_empty() {
            return;
        }
        // Pre-compute bracketed-paste preference under a short-lived
        // borrow so we don't hold the parser lock across write_to_pty.
        let bracketed =
            self.active_pane().map(|p| p.parser.lock().bracketed_paste_enabled()).unwrap_or(false);
        for paths in drops {
            let quoted = paths
                .iter()
                .map(|p| shell_quote_posix(&p.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(" ");
            let bytes = wrap_paste(&quoted, bracketed);
            self.write_to_pty(bytes);
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    pub(super) fn new_tab(&mut self, title: impl Into<String>) {
        let pane_id = next_pane_id();
        let pane = self.spawn_pane();
        self.panes.insert(pane_id, pane);
        self.tabs.push(Tab::new(title));
        self.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
    }
    pub(super) fn close_tab_at(&mut self, index: usize) {
        if index >= self.tab_states.len() {
            return;
        }
        let st = self.tab_states.remove(index);
        for id in st.tree.leaves() {
            self.panes.remove(&id);
        }
        if let Some(id) = self.tabs.tabs().get(index).map(|t| t.id) {
            self.tabs.close(id);
        }
    }
    pub fn new_tab_from_payload(&mut self, payload: &crate::os_drag::TabPayload) -> usize {
        let title = if payload.tab_title.is_empty() {
            "received tab".to_string()
        } else {
            payload.tab_title.clone()
        };
        self.new_tab(title);
        tracing::info!(
            tab = %payload.tab_title,
            "os_drag: received payload; spawned destination tab"
        );
        self.tabs.len().saturating_sub(1)
    }
}
