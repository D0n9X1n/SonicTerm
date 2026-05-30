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
use sonic_ui::search::SearchState;
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
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState,
    UserEvent, WindowState,
};

impl App {
    pub(super) fn cheatsheet_bindings(&self) -> Vec<(String, String)> {
        self.keymap
            .bindings
            .iter()
            .map(|binding| (binding.keys.clone(), format!("{:?}", binding.action.0)))
            .collect()
    }

    pub(super) fn cheatsheet_handle_key(&mut self, event: &KeyEvent) -> bool {
        use sonic_ui::cheatsheet::filter_indices;
        use winit::keyboard::{Key, NamedKey};
        if !self.cheatsheet_open {
            return false;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.cheatsheet_open = false;
                self.cheatsheet_attached_window = None;
                self.cheatsheet.clear();
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                let bindings = self.cheatsheet_bindings();
                let len = filter_indices(&bindings, &self.cheatsheet.query).len();
                self.cheatsheet.move_selection_down(len);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                let bindings = self.cheatsheet_bindings();
                let len = filter_indices(&bindings, &self.cheatsheet.query).len();
                self.cheatsheet.move_selection_up(len);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.cheatsheet.backspace();
                true
            }
            Key::Character(s) => {
                for ch in s.chars() {
                    if !ch.is_control() {
                        self.cheatsheet.input_char(ch);
                    }
                }
                true
            }
            _ => true,
        }
    }

    pub(super) fn toggle_cheatsheet(&mut self) {
        self.cheatsheet_open = !self.cheatsheet_open;
        if self.cheatsheet_open {
            // Epic #289 follow-up: tag the overlay with the OS-frontmost
            // window so the renderer paints it on THAT window, not
            // unconditionally on main. `Child(id)` → that child. `Main`
            // / `Other` / `None` → main (encoded as `None`).
            self.cheatsheet_attached_window = match self.frontmost_kind() {
                FrontmostKind::Child(id) => Some(id),
                _ => None,
            };
            self.command_palette.close();
            self.palette_attached_window = None;
            self.cheatsheet.clear();
        } else {
            self.cheatsheet_attached_window = None;
        }
        tracing::info!(
            open = self.cheatsheet_open,
            attached = ?self.cheatsheet_attached_window,
            "keymap cheat sheet toggled"
        );
        self.request_redraw_for_overlay(self.cheatsheet_attached_window);
    }

    pub(super) fn command_palette_handle_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.command_palette.is_open() {
            return false;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.command_palette.close();
                self.palette_attached_window = None;
                true
            }
            Key::Named(NamedKey::Enter) => {
                let action = self.command_palette.current().cloned();
                self.command_palette.close();
                self.palette_attached_window = None;
                if let Some(a) = action {
                    self.run_action(&a);
                }
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.command_palette.move_selection_down();
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.command_palette.move_selection_up();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.command_palette.backspace();
                true
            }
            Key::Character(s) => {
                for ch in s.chars() {
                    if !ch.is_control() {
                        self.command_palette.input_char(ch);
                    }
                }
                true
            }
            _ => true, // swallow other keys while palette is open
        }
    }
    pub(super) fn toggle_command_palette(&mut self) {
        let now_open = self.command_palette.toggle();
        if now_open {
            // Epic #289 follow-up: tag with the frontmost window so the
            // palette appears on whatever window the user is looking at.
            // Pre-fix this was hardcoded to the main window's render
            // pass — typing Cmd+Shift+P in a torn-out child popped the
            // palette on the original main window instead.
            self.palette_attached_window = match self.frontmost_kind() {
                FrontmostKind::Child(id) => Some(id),
                _ => None,
            };
        } else {
            self.palette_attached_window = None;
        }
        tracing::info!(
            open = now_open,
            attached = ?self.palette_attached_window,
            "command palette toggled"
        );
        self.draw_command_palette_overlay();
        // Synchronous redraw request so the palette appears on the very
        // next frame instead of waiting for the next pty/timer event.
        // Without this, ⌘⇧P / Ctrl+Shift+P has a noticeable visible
        // delay on an otherwise-idle terminal because no other event
        // wakes the event loop. Targets the attached window when set
        // so child windows get a redraw too, not just main.
        self.request_redraw_for_overlay(self.palette_attached_window);
    }
    pub(crate) fn draw_command_palette_overlay(&self) {
        if !self.command_palette.is_open() {
            return;
        }
        tracing::info!(
            query = %self.command_palette.query(),
            selected = self.command_palette.selected(),
            visible_count = self.command_palette.len(),
            "command palette overlay (visual TODO)"
        );
    }
    pub(super) fn open_search(&mut self) {
        // Epic #289 follow-up: route to the OS-frontmost window so
        // Cmd+F typed in a torn-out child opens a search bar on
        // THAT child's active tab, not the main window's.
        if let FrontmostKind::Child(id) = self.frontmost_kind() {
            if self.open_search_in_child(id) {
                return;
            }
            // Child id was stale — fall through to main, clear stale.
            self.frontmost_window = None;
        }
        let i = self.tabs.active_index();
        let pane_id = match self.tab_states.get(i) {
            Some(t) => t.active_pane,
            None => return,
        };
        let mut s = SearchState::new();
        if let Some(pane) = self.panes.get(&pane_id) {
            s.refresh(pane.parser.lock().grid());
        }
        if let Some(st) = self.tab_states.get_mut(i) {
            st.search = Some(s);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }

    /// Epic #289 follow-up — child-window mirror of `open_search`. Opens
    /// a search bar on the active tab of the given child window. Returns
    /// `true` on success, `false` if the recorded id is stale so the
    /// caller can fall back to the main App default.
    pub(super) fn open_search_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let i = child.tabs.active_index();
        let pane_id = match child.tab_states.get(i) {
            Some(t) => t.active_pane,
            None => return false,
        };
        let mut s = SearchState::new();
        if let Some(pane) = child.panes.get(&pane_id) {
            s.refresh(pane.parser.lock().grid());
        }
        if let Some(st) = child.tab_states.get_mut(i) {
            st.search = Some(s);
        }
        child.window.request_redraw();
        true
    }

    /// Epic #289 follow-up — redraw helper for app-level overlays
    /// (palette / cheatsheet) that need to wake whichever window is
    /// currently hosting them. `None` ⇒ main window; `Some(id)` ⇒ that
    /// child window. Silently no-ops if the recorded id is stale.
    pub(super) fn request_redraw_for_overlay(&self, attached: Option<WindowId>) {
        match attached {
            Some(id) => {
                if let Some(child) = self.windows.get(&id) {
                    child.window.request_redraw();
                }
            }
            None => {
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
        }
    }

    pub(super) fn search_active(&self) -> bool {
        let i = self.tabs.active_index();
        self.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
    }
}
