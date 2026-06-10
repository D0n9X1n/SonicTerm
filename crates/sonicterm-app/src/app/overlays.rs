//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

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
use sonicterm_ui::overlays::{PaletteLayout, PALETTE_ROW_PAD_X};
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::search::SearchState;
use sonicterm_ui::selection::Selection;
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
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState, UserEvent,
    WindowState,
};

fn estimate_palette_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

impl App {
    pub(super) fn command_palette_ime_cursor_area(
        &self,
        window_w: f32,
        window_h: f32,
        panel_padding: f32,
        scale: f32,
        font_size: f32,
        cell_w: f32,
    ) -> Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)> {
        if !self.command_palette.is_open() {
            return None;
        }
        let mut palette = self.command_palette.clone();
        let layout = PaletteLayout::compute(&mut palette, window_w, window_h, panel_padding, scale)?;
        let cursor = palette.cursor().min(palette.query().len());
        let prefix = palette.query().get(..cursor).unwrap_or(palette.query());
        let text_x = layout.query_row.x + PALETTE_ROW_PAD_X * scale;
        let caret_x = text_x + estimate_palette_text_width(prefix, font_size);
        Some((
            winit::dpi::PhysicalPosition::new(caret_x as i32, layout.query_row.y as i32),
            winit::dpi::PhysicalSize::new(cell_w.ceil() as u32, layout.query_row.h.ceil() as u32),
        ))
    }

    fn command_palette_tab_count(&self) -> usize {
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => {
                self.windows.get(&id).map(|child| child.tabs.len()).unwrap_or(1)
            }
            _ => self.main_tabs().map(|tabs| tabs.len()).unwrap_or(1),
        }
        .max(1)
    }

    fn refresh_command_palette_context(&mut self) {
        let tab_count = self.command_palette_tab_count();
        self.command_palette.set_tab_count(tab_count);
    }

    pub(super) fn command_palette_handle_ime(&mut self, ime_event: &winit::event::Ime) -> bool {
        if !self.command_palette.is_open() {
            return false;
        }
        match ime_event {
            winit::event::Ime::Commit(text) => {
                for ch in text.chars() {
                    self.command_palette.input_char(ch);
                }
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
            winit::event::Ime::Preedit(_, _)
            | winit::event::Ime::Enabled
            | winit::event::Ime::Disabled => {
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
        }
        true
    }

    pub(super) fn command_palette_handle_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.command_palette.is_open() {
            return false;
        }
        self.refresh_command_palette_context();
        if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::RenameTab
        {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    let title = self.command_palette.query().trim().to_string();
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    if !title.is_empty() {
                        self.rename_active_tab_body(title);
                    }
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    self.command_palette.backspace();
                    true
                }
                Key::Named(NamedKey::Space) => {
                    self.command_palette.input_char(' ');
                    true
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.command_palette.move_cursor_left();
                    true
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.command_palette.move_cursor_right();
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
                _ => true,
            }
        } else {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    let action = self.command_palette.current().cloned();
                    if matches!(action, Some(sonicterm_cfg::keymap::Action::RenameTab)) {
                        let body = self.active_tab_title_body().unwrap_or_default();
                        self.command_palette.start_rename_tab(body);
                        return true;
                    }
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
                Key::Named(NamedKey::Space) => {
                    self.command_palette.input_char(' ');
                    true
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.command_palette.move_cursor_left();
                    true
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.command_palette.move_cursor_right();
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
    }
    pub(super) fn toggle_command_palette(&mut self) {
        self.refresh_command_palette_context();
        let now_open = self.command_palette.toggle();
        // M6a-expand-2c-misc: notify reducer of the toggle. The
        // reducer flips `palette_open` and emits Render(Overlay) on
        // every transition.
        self.dispatch_intent(sonicterm_app_core::AppIntent::ToggleCommandPalette {
            window: sonicterm_types::WindowKey::new(0),
        });
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

    pub(super) fn start_rename_active_tab(&mut self) {
        let body = self.active_tab_title_body().unwrap_or_default();
        self.command_palette.start_rename_tab(body);
        self.palette_attached_window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    pub(super) fn active_tab_title_body(&self) -> Option<String> {
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => {
                self.windows.get(&id).and_then(|ws| ws.tabs.active_title_body())
            }
            _ => self.main_tabs().and_then(|tabs| tabs.active_title_body()),
        }
    }

    pub(super) fn rename_active_tab_body(&mut self, body: String) {
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => {
                if let Some(ws) = self.windows.get_mut(&id) {
                    ws.tabs.set_active_custom_title(body);
                    if let Some(w) = ws.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            _ => {
                if let Some(tabs) = self.main_tabs_mut() {
                    tabs.set_active_custom_title(body);
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
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
        // M6a-expand-2c-misc: notify reducer of the open transition
        // (Render(Overlay) — transition-guarded so a re-open against
        // an already-open overlay is a no-op).
        self.dispatch_intent(sonicterm_app_core::AppIntent::OpenSearch {
            window: sonicterm_types::WindowKey::new(0),
        });
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
        let (i, pane_id) = {
            let Some(ws) = self.main() else { return };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else { return };
            (i, t.active_pane)
        };
        let mut s = SearchState::new();
        if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            s.refresh(pane.parser.lock().grid());
        }
        if let Some(ws) = self.main_mut() {
            if let Some(st) = ws.tab_states.get_mut(i) {
                st.search = Some(s);
            }
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
        child.request_redraw();
        true
    }

    /// Epic #289 follow-up — redraw helper for app-level overlays
    /// (palette) that need to wake whichever window is
    /// currently hosting them. `None` ⇒ main window; `Some(id)` ⇒ that
    /// child window. Silently no-ops if the recorded id is stale.
    pub(super) fn request_redraw_for_overlay(&self, attached: Option<WindowId>) {
        match attached {
            Some(id) => {
                if let Some(child) = self.windows.get(&id) {
                    child.request_redraw();
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
        let Some(ws) = self.main() else { return false };
        let i = ws.tabs.active_index();
        ws.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
    }
}
