//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
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
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    window_dpi, with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent,
    WindowState,
};

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
        sonicterm_cfg::url_scan::url_at_char_col(&row_text, col as usize).map(|m| m.url)
    }

    /// Convert a VIEWPORT row (0 = top visible row, as returned by
    /// `GpuRenderer::pixel_to_cell`) to a scrollback-ABSOLUTE row for the
    /// focused pane, so a `Selection` tracks the same TEXT as the viewport
    /// scrolls. Resolves the pane's view top under the same `try_lock`
    /// discipline as the selection helpers (CLAUDE.md §4) and drops the lock
    /// before returning. Returns `None` when there is no active pane or the
    /// parser is busy; callers fall back to treating the viewport row as
    /// absolute (correct while unscrolled).
    pub(super) fn viewport_row_to_abs(&self, viewport_row: u16) -> Option<u64> {
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let view_top =
            GpuRenderer::resolved_view_top_abs_legacy(guard.grid(), pane.viewport_top_abs);
        drop(guard);
        Some(view_top + viewport_row as u64)
    }

    /// Compute a word selection (double-click) at scrollback-ABSOLUTE
    /// `abs_row` / `col` from the focused pane's grid. Locks the parser only
    /// long enough to read the grid and build the `Selection`, drops it, then
    /// returns the owned (Copy) value — so callers never hold the parser lock
    /// across `selection_set`/redraw (CLAUDE.md §4). Falls back to a point
    /// selection when the parser is busy.
    pub(super) fn word_selection_at(&self, abs_row: u64, col: u16) -> Selection {
        let Some(pane) = self.active_pane() else {
            return Selection::new(abs_row, col);
        };
        let Some(guard) = pane.parser.try_lock() else {
            return Selection::new(abs_row, col);
        };
        let sel = Selection::word_at(guard.grid(), abs_row, col);
        drop(guard);
        sel
    }

    /// Compute a line selection (triple-click) at scrollback-ABSOLUTE
    /// `abs_row` from the focused pane's grid. Same lock discipline as
    /// [`Self::word_selection_at`].
    pub(super) fn line_selection_at(&self, abs_row: u64) -> Selection {
        let Some(pane) = self.active_pane() else {
            return Selection::new(abs_row, 0);
        };
        let Some(guard) = pane.parser.try_lock() else {
            return Selection::new(abs_row, 0);
        };
        let sel = Selection::line_at(guard.grid(), abs_row);
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
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let view_top =
            GpuRenderer::resolved_view_top_abs_legacy(guard.grid(), pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::word_drag(guard.grid(), anchor, (cursor_abs, col));
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
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let view_top =
            GpuRenderer::resolved_view_top_abs_legacy(guard.grid(), pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::line_drag(guard.grid(), anchor_row, cursor_abs);
        drop(guard);
        Some(sel)
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

    /// Recompute hovered URL on the main window from the current cursor
    /// position and modifier state. Called on every `CursorMoved` and
    /// every `ModifiersChanged` so press / release / drift transitions
    /// all converge to the same source of truth.
    pub(super) fn refresh_hovered_url(&mut self) {
        let new_hover = self.compute_current_hovered_url();
        let prev = self.main().and_then(|ws| ws.hovered_url.clone());
        let changed = new_hover != prev;
        if let Some(ws) = self.main_mut() {
            ws.hovered_url = new_hover;
        }
        // Pointer-cursor transition: an auto-detected URL only flips to the
        // pointer when it's ACTIVE (open-URL modifier held) — a plain-hover
        // hint keeps the text cursor (it's not clickable yet). OSC 8 keeps
        // its always-on pointer below.
        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
        let has_active_hover = self
            .main()
            .and_then(|ws| ws.hovered_url.as_ref())
            .map(|h| h.active)
            .unwrap_or(false);
        let want_pointer = has_active_hover
            || self
                .main_renderer()
                .and_then(|r| r.pixel_to_cell(cursor_pos.0 as f32, cursor_pos.1 as f32))
                .and_then(|(row, col)| self.osc8_uri_at(row, col))
                .is_some();
        let current_hover_link = self.main().map(|ws| ws.hover_link).unwrap_or(false);
        if want_pointer != current_hover_link {
            if let Some(ws) = self.main_mut() {
                ws.hover_link = want_pointer;
            }
            if let Some(w) = self.main_window() {
                w.set_cursor(if want_pointer { CursorIcon::Pointer } else { CursorIcon::Default });
            }
        }
        if changed {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }

    fn compute_current_hovered_url(&self) -> Option<super::hovered_url::HoveredUrl> {
        // Two-tier hover: a URL under the cursor is detected REGARDLESS of the
        // modifier so plain hover can show a yellow hint underline. The
        // `active` flag (modifier held) is what upgrades it to the clickable
        // accent look + pointer cursor downstream. Clicking still goes through
        // the modifier-gated `dispatch_modifier_click`, so detecting without
        // the modifier never makes a URL openable on a plain click.
        let cursor_pos = self.main()?.cursor_pos;
        // Gate to the ACTIVE pane: `pixel_to_cell` hit-tests against the
        // window, but `focused_pane_row_text` (below) reads the active pane's
        // grid. In a split, hovering an INACTIVE pane at a row/col that
        // happens to match a URL in the active pane would otherwise highlight
        // the active pane's URL. Only proceed when the cursor is over the
        // active pane itself. (#660 review)
        let active_id = self
            .main()
            .and_then(|ws| ws.tab_states.get(ws.tabs.active_index()))
            .map(|st| st.active_pane);
        let hit = self.pane_at_cursor(cursor_pos.0 as f32, cursor_pos.1 as f32);
        if hit != active_id {
            return None;
        }
        let r = self.main_renderer()?;
        let (row, col) = r.pixel_to_cell(cursor_pos.0 as f32, cursor_pos.1 as f32)?;
        // OSC 8 has its own affordance — don't double up.
        if self.osc8_uri_at(row, col).is_some() {
            return None;
        }
        let row_text = self.focused_pane_row_text(row)?;
        let mut hov = super::hovered_url::hovered_from_row(&row_text, row, col)?;
        hov.active = self.url_open_modifier_held();
        Some(hov)
    }
    /// True iff the platform "open this in the browser" modifier is held.
    /// macOS: Cmd (super). Windows / Linux: Ctrl.
    pub(super) fn url_open_modifier_held(&self) -> bool {
        let mods = self.main_modifiers();
        if cfg!(target_os = "macos") {
            mods.super_key()
        } else {
            mods.control_key()
        }
    }
    pub(super) fn open_ssh_pane(&mut self, target: &str) {
        match sonicterm_io::ssh::parse_target(target) {
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
        self.copy_mode_set(Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at(cursor)));
        if let Some(panes) = self.main_panes() {
            mark_all_panes_dirty(panes);
        }
    }

    pub(super) fn enter_quick_select(&mut self) {
        let Some(pane) = self.active_pane() else { return };
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

    pub(super) fn copy_selection(&mut self) {
        let sel = match self.main().and_then(|ws| ws.selection) {
            Some(s) => s,
            None => return,
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
        let updated = {
            let Some(ws) = self.main_mut() else { return };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get(i) else { return };
            let pane_id = st.active_pane;
            let Some(pane) = ws.panes.get_mut(&pane_id) else { return };
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
        // Issue #553 Phase A: in-process tear-out drain. Replaces the
        // legacy `Command::new`-based spawn — Phase B will delete the
        // dead `spawn_tearout_child` + `--tear-out-payload` CLI flag.
        // Ordering MUST stay before `drain_pending_os_teardown` (the
        // PR #533 invariant — `cancel_drag_session` must see the new
        // child window already inserted).
        if let Some(req) = self.pending_tear_out.take() {
            self.drain_pending_tear_out(el, req);
        }
    }

    /// Issue #553 Phase A: resolve a queued `PendingTearOut` request
    /// into a real torn-out window. Locates the source tab via the
    /// recorded `(WindowId, tab_idx)` handle, detaches the tab state
    /// (which MOVES the live `PtyHandle` — see `tear_out.rs` for the
    /// `detach_tab_state` contract), and hands it to the reusable
    /// `install_torn_out_window` builder positioned at the drop screen
    /// position from Win32 `GetCursorPos`.
    fn drain_pending_tear_out(&mut self, el: &ActiveEventLoop, req: crate::app::PendingTearOut) {
        // Only main-window tear-out is wired in Phase A; child-window
        // tear-out continues to flow through `tear_out_tab` directly.
        // The source check guards against a stale request after the
        // source window has gone away.
        let main_id = match self.main_window().map(|w| w.id()) {
            Some(id) => id,
            None => {
                tracing::warn!(
                    src = ?req.source_window,
                    "drain_pending_tear_out: no main window — dropping request"
                );
                return;
            }
        };
        if req.source_window != main_id {
            tracing::warn!(
                src = ?req.source_window, main = ?main_id,
                "drain_pending_tear_out: source is not main — falling back to noop \
                 (child-window tear-out drained via tear_out_tab inline)"
            );
            return;
        }
        let Some((tab, state, panes)) = self.detach_tab_state(req.source_tab_idx) else {
            tracing::warn!(
                idx = req.source_tab_idx,
                "drain_pending_tear_out: detach_tab_state returned None — dropping request"
            );
            return;
        };
        if self.install_torn_out_window(el, tab, state, panes, Some(req.drop_screen_pos)).is_none()
        {
            tracing::warn!("drain_pending_tear_out: install_torn_out_window failed");
            return;
        }
        self.tear_out_apply_source_side(req.source_tab_idx);
        tracing::info!(
            at = ?req.drop_screen_pos,
            "in-process tear-out completed (Issue #553 Phase A — no child process spawned)"
        );
    }

    /// Issue #462 (speculative defensive fix): drain a deferred
    /// `cancel_drag_session` request raised by `handle_os_drag_ended`
    /// on the `DroppedOnEmpty` branch. Callers MUST invoke this
    /// AFTER [`Self::drain_pending_window_creates`] so any
    /// tear-out-spawn has produced its new window before cross-window
    /// drag-residue cleanup mutates `self.windows`. The all-windows
    /// loop inside `cancel_drag_session` still runs UNCONDITIONALLY
    /// when this drain fires (preserves the
    /// `os_drag_cleanup.rs:172-201` idempotence guarantee — the flag
    /// controls WHEN, not WHETHER).
    pub(super) fn drain_pending_os_teardown(&mut self) {
        if self.pending_os_teardown {
            self.pending_os_teardown = false;
            self.cancel_drag_session();
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
        use sonicterm_ui::tabs::Tab;

        let attrs = super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title("SonicTerm")
                    .with_decorations(true)
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
            sonicterm_gpu::core::RendererSettings {
                font_family: &self.config.font.family,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
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
        renderer.set_titlebar_inset(0.0);
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());
        let real_sf = window_dpi(&window);
        renderer.force_rebuild_for_scale(real_sf);
        let real_inner = window.inner_size();
        renderer.resize(real_inner.width.max(1), real_inner.height.max(1));

        let (cols, rows) = renderer.cells();
        let pane_state = self.spawn_pane_state_for_child(cols, rows, window.clone());
        let pane_id = super::next_pane_id();
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane_state);

        let mut tabs = TabBar::new();
        tabs.push(Tab::new("shell 1".to_string()));

        let win_id = window.id();
        let child = WindowState {
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
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            test_drag_chip_marker: None,
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
            if let Some(w) = self.main_window() {
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
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }
    pub(super) fn new_tab(&mut self, title: impl Into<String>) {
        let pane_id = next_pane_id();
        let pane = self.spawn_pane();
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, pane);
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
    }
    pub(super) fn close_tab_at(&mut self, index: usize) {
        let Some(ws) = self.main_mut() else { return };
        if index >= ws.tab_states.len() {
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
