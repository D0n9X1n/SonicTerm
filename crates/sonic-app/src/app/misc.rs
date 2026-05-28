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
use sonic_ui::prefs::{PrefsHit, PrefsState};
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
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, ChildWindow, PaneState, TabState,
    UserEvent,
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
        let hid = grid.row(row)[col as usize].hyperlink?;
        let uri = guard.hyperlinks().lookup(hid).map(|h| h.uri.clone());
        drop(guard);
        uri
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
    pub(super) fn copy_selection(&mut self) {
        let Some(sel) = self.selection.as_ref() else {
            return;
        };
        if sel.is_empty() {
            return;
        }
        let Some(pane) = self.active_pane() else { return };
        let text = sel.as_text(pane.parser.lock().grid());
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
        if self.pending_prefs_open {
            self.pending_prefs_open = false;
            self.create_prefs_window(el);
        }
    }
    pub(super) fn drain_menubar_actions(&mut self, el: &ActiveEventLoop) {
        let mut ran_any = false;
        for action in crate::menubar_bridge::drain() {
            tracing::debug!("menubar action: {action:?}");
            self.run_action(&action);
            ran_any = true;
        }
        // Menubar dispatch can set window-creation flags (e.g.
        // `pending_prefs_open` via OpenPreferences). The KeyboardInput
        // path used to consume these inline; the menubar / UserEvent
        // path didn't, so ⌘, from the macOS menubar — and from the
        // keymap, since that path also flows through here when the
        // EventLoopProxy delivers — silently dropped the request.
        // Funnel through the single drain helper so every dispatch
        // site is covered. See `drain_pending_window_creates`.
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
        self.tab_states.push(TabState {
            tree: PaneTree::leaf(pane_id),
            active_pane: pane_id,
            search: None,
        });
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
