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
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, ChildWindow, PaneState, TabState,
    UserEvent,
};
use crate::app::integrated_titlebar_inset;

impl App {
    pub(super) fn command_palette_handle_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.command_palette.is_open() {
            return false;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.command_palette.close();
                true
            }
            Key::Named(NamedKey::Enter) => {
                let action = self.command_palette.current().cloned();
                self.command_palette.close();
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
        tracing::info!(open = now_open, "command palette toggled");
        self.draw_command_palette_overlay();
        // Synchronous redraw request so the palette appears on the very
        // next frame instead of waiting for the next pty/timer event.
        // Without this, ⌘⇧P / Ctrl+Shift+P has a noticeable visible
        // delay on an otherwise-idle terminal because no other event
        // wakes the event loop.
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
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
    }
    pub(super) fn search_active(&self) -> bool {
        let i = self.tabs.active_index();
        self.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
    }
}
