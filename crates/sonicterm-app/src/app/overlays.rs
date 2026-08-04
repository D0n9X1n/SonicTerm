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
use sonicterm_ui::command_palette::TabColorChoice;
use sonicterm_ui::overlays::{
    command_palette_query_caret_prefix, PaletteLayout, PALETTE_ROW_PAD_X,
};
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

pub fn theme_tab_color_choices(theme: &Theme) -> Vec<TabColorChoice> {
    let bg = theme.colors.background.0.to_ascii_lowercase();
    let mut choices = vec![TabColorChoice { name: "Reset to Default".to_string(), hex: None }];
    let mut pairs = vec![
        ("ANSI Black", theme.colors.ansi.black.0.as_str()),
        ("ANSI Red", theme.colors.ansi.red.0.as_str()),
        ("ANSI Green", theme.colors.ansi.green.0.as_str()),
        ("ANSI Yellow", theme.colors.ansi.yellow.0.as_str()),
        ("ANSI Blue", theme.colors.ansi.blue.0.as_str()),
        ("ANSI Magenta", theme.colors.ansi.magenta.0.as_str()),
        ("ANSI Cyan", theme.colors.ansi.cyan.0.as_str()),
        ("ANSI White", theme.colors.ansi.white.0.as_str()),
        ("Bright Black", theme.colors.bright.black.0.as_str()),
        ("Bright Red", theme.colors.bright.red.0.as_str()),
        ("Bright Green", theme.colors.bright.green.0.as_str()),
        ("Bright Yellow", theme.colors.bright.yellow.0.as_str()),
        ("Bright Blue", theme.colors.bright.blue.0.as_str()),
        ("Bright Magenta", theme.colors.bright.magenta.0.as_str()),
        ("Bright Cyan", theme.colors.bright.cyan.0.as_str()),
        ("Bright White", theme.colors.bright.white.0.as_str()),
    ];
    pairs.retain(|(_, hex)| hex.to_ascii_lowercase() != bg);
    choices.extend(
        pairs.into_iter().map(|(name, hex)| TabColorChoice {
            name: name.to_string(),
            hex: Some(hex.to_string()),
        }),
    );
    choices
}

impl App {
    fn palette_ime_preedit(&self) -> &str {
        match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|ws| ws.ime.preedit()).unwrap_or(""),
            None => self.main().map(|ws| ws.ime.preedit()).unwrap_or(""),
        }
    }

    fn update_palette_ime_state(&mut self, ime_event: &winit::event::Ime) {
        let target = self.palette_attached_window;
        let Some(ws) = (match target {
            Some(id) => self.windows.get_mut(&id),
            None => self.main_mut(),
        }) else {
            // When: target names a window already removed, or target is None and
            // there is no main window, drop the IME update — nothing records it.
            return;
        };
        match ime_event {
            winit::event::Ime::Enabled => ws.ime.handle_enabled(),
            winit::event::Ime::Disabled => ws.ime.handle_disabled(),
            winit::event::Ime::Preedit(text, cursor) => ws.ime.handle_preedit(text, *cursor),
            winit::event::Ime::Commit(text) => {
                // When: a Commit arrives the palette consumes text itself, so
                // take_commits drains the buffer and no bytes reach the PTY later.
                ws.ime.handle_commit(text);
                let _ = ws.ime.take_commits();
            }
        }
    }

    pub(super) fn palette_ime_is_composing(&self) -> bool {
        match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|ws| ws.ime.is_composing()).unwrap_or(false),
            None => self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false),
        }
    }

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
            // When: command_palette is closed there is no query row to anchor
            // the IME candidate box to; None leaves the cursor area unchanged.
            return None;
        }
        let mut palette = self.command_palette.clone();
        let layout =
            PaletteLayout::compute(&mut palette, window_w, window_h, panel_padding, scale)?;
        let preedit = self.palette_ime_preedit();
        let prefix = command_palette_query_caret_prefix(&palette, preedit);
        let text_x = layout.query_row.x + PALETTE_ROW_PAD_X * scale;
        let caret_x = text_x + estimate_palette_text_width(&prefix, font_size);
        Some((
            winit::dpi::PhysicalPosition::new(caret_x as i32, layout.query_row.y as i32),
            winit::dpi::PhysicalSize::new(cell_w.ceil() as u32, layout.query_row.h.ceil() as u32),
        ))
    }

    pub(super) fn update_command_palette_ime_cursor_area(&self) {
        if !self.command_palette.is_open() {
            // When: command_palette is closed there is no palette caret to
            // follow; the IME cursor area stays where the terminal set it.
            return;
        }
        let target = self.palette_attached_window;
        let (window, width, height, scale, font_size, cell_w) = if let Some(id) = target {
            // When: target names a child window the palette is attached to it,
            // so measure that child's surface for the IME box.
            let Some(child) = self.windows.get(&id) else {
                // When: id is no longer in windows the child closed since the
                // palette attached; abandon the reposition instead of a dead window.
                return;
            };
            let (Some(window), Some(renderer)) = (child.window.as_ref(), child.renderer.as_ref())
            else {
                // When: the child has no window or renderer yet there is no
                // surface to measure scale and cell width from; skip until ready.
                return;
            };
            let size = window.inner_size();
            (
                window.clone(),
                size.width as f32,
                size.height as f32,
                renderer.scale_factor(),
                sonicterm_ui::tab_spans::tab_title_font_size(renderer.font_size())
                    * renderer.scale_factor(),
                renderer.cell_w,
            )
        } else {
            // When: target is None the palette is attached to no child window,
            // so measure the main window's surface instead.
            let (Some(window), Some(renderer)) = (self.main_window(), self.main_renderer()) else {
                // When: main_window or main_renderer is absent before the first
                // frame there is no surface to place the IME box on; skip.
                return;
            };
            let size = window.inner_size();
            (
                window.clone(),
                size.width as f32,
                size.height as f32,
                renderer.scale_factor(),
                sonicterm_ui::tab_spans::tab_title_font_size(renderer.font_size())
                    * renderer.scale_factor(),
                renderer.cell_w,
            )
        };
        if let Some((pos, size)) = self.command_palette_ime_cursor_area(
            width,
            height,
            self.config.appearance.panel_padding,
            scale,
            font_size,
            cell_w,
        ) {
            window.set_ime_cursor_area(pos, size);
        }
    }

    fn command_palette_text_edit(
        &self,
        event: &KeyEvent,
    ) -> Option<sonicterm_ui::text_edit::TextEdit> {
        let mods = match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|ws| ws.modifiers),
            None => self.main().map(|ws| ws.modifiers),
        }
        .unwrap_or_else(ModifiersState::empty);
        super::text_edit::core_text_edit_for_key(&event.logical_key, mods)
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
            // When: command_palette is closed the IME event belongs to the
            // terminal; returning false lets window_event run its commit path.
            return false;
        }
        self.update_palette_ime_state(ime_event);
        match ime_event {
            winit::event::Ime::Commit(text) => {
                for ch in text.chars() {
                    self.command_palette.input_char(ch);
                }
                self.update_command_palette_ime_cursor_area();
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
            winit::event::Ime::Preedit(_, _)
            | winit::event::Ime::Enabled
            | winit::event::Ime::Disabled => {
                self.update_command_palette_ime_cursor_area();
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
        }
        true
    }

    pub(super) fn command_palette_handle_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.command_palette.is_open() {
            // When: command_palette is closed no palette state may change here;
            // both callers gate on their own checks and discard this false.
            return false;
        }
        self.refresh_command_palette_context();
        if self.palette_ime_is_composing() {
            // When: palette_ime_is_composing is true the IME owns the keystroke;
            // swallow every key so a half-formed CJK sequence cannot also navigate.
            if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                if let Some(ws) = match self.palette_attached_window {
                    Some(id) => self.windows.get_mut(&id),
                    None => self.main_mut(),
                } {
                    ws.ime.cancel();
                }
                self.update_command_palette_ime_cursor_area();
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
            return true;
        }
        if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::TabColor
        {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    self.apply_selected_tab_color();
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.command_palette.move_selection_down();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.command_palette.move_selection_up();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                _ => true,
            }
        } else if let Some(edit) = self.command_palette_text_edit(event) {
            // When: command_palette_text_edit maps the chord to an emacs ctrl edit
            // (ctrl+a, ctrl+k, ctrl+w); it rewrites the query in rename and list modes.
            self.command_palette.apply_text_edit(edit);
            self.update_command_palette_ime_cursor_area();
            self.request_redraw_for_overlay(self.palette_attached_window);
            true
        } else if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::RenameTab
        {
            // When: mode is RenameTab the query holds the tab title, so Enter
            // commits it via rename_active_tab_body instead of running an action.
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
                    self.rename_active_tab_body(title);
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    self.command_palette.backspace();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Space) => {
                    self.command_palette.input_char(' ');
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.command_palette.move_cursor_left();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.command_palette.move_cursor_right();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Home) => {
                    self.command_palette.move_cursor_home();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::End) => {
                    self.command_palette.move_cursor_end();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Delete) => {
                    self.command_palette.delete_forward();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Character(s) => {
                    for ch in s.chars() {
                        if !ch.is_control() {
                            self.command_palette.input_char(ch);
                        }
                    }
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                _ => true,
            }
        } else {
            // When: mode is neither TabColor nor RenameTab the palette shows
            // the command list, where Enter runs the selected action.
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    // When: Enter arrives in list mode it runs the highlighted
                    // entry; RenameTab and UpdateTabColor re-enter sub-modes.
                    let action = self.command_palette.current().cloned();
                    let source_window = self.palette_attached_window.or(self.main_window_id);
                    if matches!(action, Some(sonicterm_cfg::keymap::Action::RenameTab)) {
                        // When: matches finds RenameTab the palette stays open as
                        // a rename editor seeded with the active tab title.
                        let body = self.active_tab_title_body().unwrap_or_default();
                        self.command_palette.start_rename_tab(body);
                        self.update_command_palette_ime_cursor_area();
                        self.request_redraw_for_overlay(self.palette_attached_window);
                        return true;
                    }
                    if matches!(action, Some(sonicterm_cfg::keymap::Action::UpdateTabColor)) {
                        // When: matches finds UpdateTabColor the palette switches
                        // to the tab color picker instead of closing.
                        self.start_update_tab_color();
                        return true;
                    }
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    if let Some(a) = action {
                        if let Some(source_window) = source_window {
                            self.run_action_for_window(&a, source_window);
                        } else {
                            // When: source_window is None no originating window
                            // was recorded; run_action picks the frontmost itself.
                            self.run_action(&a);
                        }
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
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Space) => {
                    self.command_palette.input_char(' ');
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.command_palette.move_cursor_left();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.command_palette.move_cursor_right();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Home) => {
                    self.command_palette.move_cursor_home();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::End) => {
                    self.command_palette.move_cursor_end();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Delete) => {
                    self.command_palette.delete_forward();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Character(s) => {
                    for ch in s.chars() {
                        if !ch.is_control() {
                            self.command_palette.input_char(ch);
                        }
                    }
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                _ => true, // swallow other keys while palette is open
            }
        }
    }
    pub(super) fn toggle_command_palette(&mut self) {
        self.refresh_command_palette_context();
        let now_open = self.command_palette.toggle();
        // Notify the reducer of the toggle. The reducer flips `palette_open`
        // and emits Render(Overlay) on every transition.
        self.dispatch_intent(sonicterm_app_core::AppIntent::ToggleCommandPalette {
            window: sonicterm_types::WindowKey::new(0),
        });
        if now_open {
            // Tag with the frontmost window so the palette appears on
            // whatever window the user is looking at, rather than on the
            // main window's render pass.
            self.palette_attached_window = match self.frontmost_kind() {
                FrontmostKind::Child(id) => Some(id),
                _ => None,
            };
            self.update_command_palette_ime_cursor_area();
        } else {
            // When: now_open is false the toggle just closed the palette; drop
            // the attachment so later redraws do not target a stale window.
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
        self.update_command_palette_ime_cursor_area();
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

    pub(super) fn start_update_tab_color(&mut self) {
        let title = self.active_tab_title_body().unwrap_or_else(|| "current tab".to_string());
        let choices = theme_tab_color_choices(&self.theme);
        self.command_palette.start_tab_color_picker(title, choices);
        self.palette_attached_window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    pub(super) fn apply_selected_tab_color(&mut self) {
        let Some(choice) = self.command_palette.selected_tab_color().cloned() else {
            // When: selected_tab_color has no entry at the current index the
            // picker is empty or the selection is stale; leave the color as is.
            return;
        };
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => {
                if let Some(ws) = self.windows.get_mut(&id) {
                    if let Some(hex) = choice.hex {
                        ws.tabs.set_active_custom_color(hex);
                    } else {
                        // When: choice carries no hex the picked entry is Reset
                        // to Default; clear the child tab's color override.
                        ws.tabs.clear_active_custom_color();
                    }
                    ws.request_redraw();
                }
            }
            _ => {
                if let Some(tabs) = self.main_tabs_mut() {
                    if let Some(hex) = choice.hex {
                        tabs.set_active_custom_color(hex);
                    } else {
                        // When: choice carries no hex the picked entry is Reset
                        // to Default; clear the override so the theme color wins.
                        tabs.clear_active_custom_color();
                    }
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
        }
    }
    pub(crate) fn draw_command_palette_overlay(&self) {
        if !self.command_palette.is_open() {
            // When: command_palette is closed there is no query or selection
            // state to report; this helper only emits a trace line.
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
        // Notify the reducer of the open transition (Render(Overlay) —
        // transition-guarded so a re-open against an already-open overlay
        // is a no-op).
        self.dispatch_intent(sonicterm_app_core::AppIntent::OpenSearch {
            window: sonicterm_types::WindowKey::new(0),
        });
        // Cmd+F typed in a torn-out child opens a search bar on THAT child's
        // active tab, not the main window's.
        if let FrontmostKind::Child(id) = self.frontmost_kind() {
            // When: frontmost_kind reports Child the frontmost window is a
            // torn-out child, so route the search bar to its active tab.
            if self.open_search_in_child(id) {
                // When: open_search_in_child succeeded the child window owns
                // the new search bar; return so main does not open a second.
                return;
            }
            // Child id was stale — fall through to main, clear stale.
            self.frontmost_window = None;
        }
        let (i, pane_id) = {
            let Some(ws) = self.main() else {
                // When: main is absent before the window exists there is no tab
                // to hold the new SearchState; leave search unopened.
                return;
            };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else {
                // When: tab_states has no entry at active index i, tabs and
                // tab_states have diverged; open no search bar rather than guess.
                return;
            };
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

    /// Child-window mirror of `open_search`. Opens a search bar on the
    /// active tab of the given child window. Returns `true` on success,
    /// `false` if the recorded id is stale so the caller can fall back to
    /// the main App default.
    pub(super) fn open_search_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: win_id is no longer in windows the child closed since it
            // was recorded; return false so open_search falls back to main.
            return false;
        };
        let i = child.tabs.active_index();
        // When: tab_states has no entry at the child's active index i, tabs
        // and tab_states have diverged; report failure instead of guessing.
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

    /// Redraw helper for app-level overlays (palette) that need to wake
    /// whichever window is currently hosting them. `None` ⇒ main window;
    /// `Some(id)` ⇒ that child window. Silently no-ops if the recorded id
    /// is stale.
    pub(super) fn request_redraw_for_overlay(&mut self, attached: Option<WindowId>) {
        self.input_dirty = true;
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
        let Some(ws) = self.main() else {
            // When: main has not been created yet no tab can hold a SearchState,
            // so report search inactive rather than claiming it owns the keys.
            return false;
        };
        let i = ws.tabs.active_index();
        ws.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
#[path = "overlays_tests.rs"]
mod overlays_tests;
