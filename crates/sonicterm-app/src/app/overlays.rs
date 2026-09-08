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
use sonicterm_ui::command_palette::{CommandPaletteMode, PaletteEntry, TabColorChoice};
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
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
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

pub(super) struct PalettePointerCapture {
    window_id: WindowId,
    target: PalettePointerTarget,
}

pub(super) enum PalettePointerTarget {
    Entry(PaletteEntry),
    Outside,
}

enum PalettePointerHit {
    Row { index: usize, entry: PaletteEntry },
    Outside,
    Inside,
}

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

/// Derive window-local command facts; an optional grid is the caller's already-held active-pane view.
pub(super) fn command_palette_context(
    window: &WindowState,
    active_grid: Option<&Grid>,
) -> sonicterm_ui::command_label::CommandContext {
    use sonicterm_ui::command_label::CommandContext;
    if window.hidden {
        // When: `window` is hidden, it cannot lend command targets to an attached palette.
        return CommandContext::default();
    }
    let tab = window.tab_states.get(window.tabs.active_index());
    let active_pane = tab.map(|tab| tab.active_pane).filter(|id| window.panes.contains_key(id));
    let selection_available = active_pane.is_some_and(|pane_id| {
        let valid = |grid: &Grid| {
            window.selection.is_some_and(|mut selection| {
                !selection.is_empty()
                    && !sonicterm_ui::selection::revalidate_selection(&mut selection, pane_id, grid)
            })
        };
        match active_grid {
            Some(grid) => valid(grid),
            None => window
                .panes
                .get(&pane_id)
                .and_then(|pane| pane.parser.try_lock())
                .is_some_and(|parser| valid(parser.grid())),
        }
    });
    let focus_available =
        tab.map_or([None; 4], |tab| tab.tree.focus_neighbors(tab.active_pane)).map(|neighbor| {
            neighbor.is_some_and(|id| active_pane.is_some() && window.panes.contains_key(&id))
        });
    CommandContext {
        window_available: true,
        tab_count: window.tabs.len(),
        pane_available: active_pane.is_some(),
        selection_available,
        read_only: window.copy_mode.as_ref().is_some_and(|state| state.is_read_only()),
        focus_available,
    }
}

impl App {
    fn command_palette_pointer_hit(&mut self, window_id: WindowId) -> Option<PalettePointerHit> {
        let window = self.windows.get(&window_id)?;
        let renderer = window.renderer.as_ref()?;
        let (width, height) = renderer.logical_size();
        let (x, y) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let layout = PaletteLayout::compute(
            &mut self.command_palette,
            width,
            height,
            self.config.appearance.panel_padding,
            renderer.scale_factor(),
        )?;
        if !layout.border.contains(x, y) {
            // When: `layout.border` excludes the pointer, reserve an outside dismissal rather than a terminal click.
            return Some(PalettePointerHit::Outside);
        }
        if let Some(row) = layout.rows.iter().find(|row| row.rect.contains(x, y)) {
            // When: `row.rect` contains the pointer, retain its displayed identity before refreshing live targets.
            let entry = self.command_palette.visible().get(row.item_index).copied()?.clone();
            return Some(PalettePointerHit::Row { index: row.item_index, entry });
        }
        Some(PalettePointerHit::Inside)
    }

    fn release_command_palette_pointer(
        &mut self,
        window_id: WindowId,
        hit: Option<PalettePointerHit>,
    ) {
        let Some(capture) = self.palette_pointer_capture.take() else {
            // When: `palette_pointer_capture` is empty, swallow the opener's release without choosing a row.
            return;
        };
        if capture.window_id != window_id
            || self.palette_attached_window.or(self.main_window_id) != Some(window_id)
            || !self.command_palette.is_open()
            || self.command_palette.mode() != CommandPaletteMode::Commands
            || self.palette_ime_is_composing()
        {
            // When: `capture` no longer belongs to this open command input, cancel rather than execute across context changes.
            return;
        }
        match (capture.target, hit) {
            (PalettePointerTarget::Outside, Some(PalettePointerHit::Outside)) => {
                self.command_palette.close();
                self.palette_attached_window = None;
                self.request_redraw_for_overlay(Some(window_id));
            }
            (PalettePointerTarget::Entry(pressed), Some(PalettePointerHit::Row { entry, .. }))
                if pressed.same_identity(&entry) =>
            {
                // Revalidate live context before delegating the displayed identity to the Enter route.
                self.refresh_command_palette_context();
                if self.command_palette.current().is_some_and(|entry| pressed.same_identity(entry))
                {
                    self.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
                }
                self.request_redraw_for_overlay(Some(window_id));
            }
            _ => {
                // When: `hit` no longer names `capture.target`, leave the modal open without dispatch.
            }
        }
    }

    /// Consume attached-modal pointer input without stealing a previously latched terminal or chrome gesture.
    pub(super) fn command_palette_handle_pointer_event(
        &mut self,
        window_id: WindowId,
        event: &WindowEvent,
    ) -> bool {
        if !self.command_palette.is_open() {
            // When: `command_palette` is closed, revoke any capture and leave terminal event ownership unchanged.
            self.palette_pointer_capture = None;
            return false;
        }
        if self.palette_attached_window.or(self.main_window_id) != Some(window_id) {
            // When: `window_id` differs from the palette host, leave that window's pointer input independent.
            return false;
        }
        if matches!(
            event,
            WindowEvent::Focused(false)
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Ime(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::CloseRequested
                | WindowEvent::Destroyed
        ) {
            // When: `matches!` identifies input or lifecycle changes, revoke the click while preserving normal event handling.
            self.palette_pointer_capture = None;
            return false;
        }
        let Some(window) = self.windows.get_mut(&window_id).filter(|window| !window.hidden) else {
            // When: `window_id` has no visible host, its stale capture cannot borrow another window's context.
            self.palette_pointer_capture = None;
            return false;
        };
        if window.mouse_down
            || window.pointer_gesture.is_some()
            || window.pressed_tab.is_some()
            || window.drag_session.is_some()
            || window.scrollbar_drag.is_some()
            || window.splitter_drag.is_some()
        {
            // When: `window` already owns a held gesture, its original handler must receive motion and the paired release.
            self.palette_pointer_capture = None;
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                window.cursor_pos = (position.x, position.y);
                window.invalidate_path_hover();
                if let Some(native) = window.window.as_ref() {
                    native.set_cursor(CursorIcon::Default);
                }
                true
            }
            WindowEvent::CursorLeft { .. } => {
                window.cursor_pos = (-1.0, -1.0);
                window.invalidate_path_hover();
                self.palette_pointer_capture = None;
                true
            }
            WindowEvent::CursorEntered { .. } => true,
            WindowEvent::MouseInput { state, button, .. } => {
                // When: `MouseInput` belongs to the modal, never forward either half of the click to a terminal.
                if self.command_palette.mode() != CommandPaletteMode::Commands
                    || self.palette_ime_is_composing()
                    || *button != MouseButton::Left
                {
                    // When: `button`, mode, or composition forbids activation, consume input without retaining a click.
                    self.palette_pointer_capture = None;
                    return true;
                }
                let hit = self.command_palette_pointer_hit(window_id);
                match state {
                    ElementState::Pressed => {
                        self.palette_pointer_capture = match hit {
                            Some(PalettePointerHit::Row { index, entry }) => {
                                // Capture the displayed identity before any live target refresh can move this row.
                                self.command_palette.select_visible_index(index);
                                Some(PalettePointerCapture {
                                    window_id,
                                    target: PalettePointerTarget::Entry(entry),
                                })
                            }
                            Some(PalettePointerHit::Outside) => Some(PalettePointerCapture {
                                window_id,
                                target: PalettePointerTarget::Outside,
                            }),
                            _ => None,
                        };
                        self.request_redraw_for_overlay(Some(window_id));
                    }
                    ElementState::Released => self.release_command_palette_pointer(window_id, hit),
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // When: `MouseWheel` belongs to the modal, cancel held clicks and keep all deltas away from the PTY.
                self.palette_pointer_capture = None;
                if self.command_palette.mode() != CommandPaletteMode::Commands
                    || self.palette_ime_is_composing()
                {
                    // When: `command_palette` is editing a title/color or composing, wheel input cannot change its selection.
                    return true;
                }
                let vertical = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(position) => position.y,
                };
                if vertical.is_finite() && vertical != 0.0 {
                    // When: `vertical` is finite and nonzero, move at most one row without looping over event magnitudes.
                    self.refresh_command_palette_context();
                    let _ = self.command_palette_pointer_hit(window_id);
                    let last = self.command_palette.len().saturating_sub(1);
                    let selected = self.command_palette.selected();
                    let next = if selected >= self.command_palette.len() {
                        if vertical > 0.0 {
                            last
                        } else {
                            // When: `vertical` moves forward from no selection, begin at the first row.
                            0
                        }
                    } else if vertical > 0.0 {
                        // When: `vertical` moves backward, clamp to the first row rather than wrapping.
                        selected.saturating_sub(1)
                    } else {
                        // When: `vertical` moves forward from a row, clamp to the last row rather than wrapping.
                        (selected + 1).min(last)
                    };
                    self.command_palette.select_visible_index(next);
                    self.request_redraw_for_overlay(Some(window_id));
                }
                true
            }
            _ => false,
        }
    }

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
                renderer.font_size() * renderer.scale_factor(),
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
                renderer.font_size() * renderer.scale_factor(),
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
        logical_key: &winit::keyboard::Key,
    ) -> Option<sonicterm_ui::text_edit::TextEdit> {
        let mods = match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|ws| ws.modifiers),
            None => self.main().map(|ws| ws.modifiers),
        }
        .unwrap_or_else(ModifiersState::empty);
        super::text_edit::core_text_edit_for_key(logical_key, mods)
    }

    fn command_palette_modifiers(&self) -> ModifiersState {
        match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|ws| ws.modifiers),
            None => self.main().map(|ws| ws.modifiers),
        }
        .unwrap_or_else(ModifiersState::empty)
    }

    fn command_palette_input_text(
        &mut self,
        logical_key: &winit::keyboard::Key,
        event_text: Option<Option<&str>>,
    ) {
        use winit::keyboard::{Key, NamedKey};
        let text = match event_text {
            Some(text) => text,
            None => match logical_key {
                Key::Character(text) => Some(text.as_str()),
                Key::Named(NamedKey::Space) => Some(" "),
                _ => None,
            },
        };
        for ch in text.unwrap_or_default().chars().filter(|ch| !ch.is_control()) {
            self.command_palette.input_char(ch);
        }
    }

    pub(super) fn refresh_command_palette_context(&mut self) {
        use sonicterm_ui::command_label::CommandContext;
        let window_id = if self.command_palette.is_open() {
            self.palette_attached_window.or(self.main_window_id)
        } else {
            // When: `command_palette` is closed, the next open follows the existing frontmost policy.
            match self.frontmost_kind() {
                FrontmostKind::Child(id) => Some(id),
                _ => self.main_window_id,
            }
        };
        let window = window_id.and_then(|id| self.windows.get(&id)).filter(|window| !window.hidden);
        let context = window
            .map_or_else(CommandContext::default, |window| command_palette_context(window, None));
        self.command_palette.set_context(context);
        let empty = TabBar::new();
        self.command_palette
            .set_tabs(window.map(|window| &window.tabs).unwrap_or(&empty), &self.i18n);
    }

    /// Match native input to the window that owns the visible palette.
    pub(super) fn command_palette_owns_input(&self, window_id: WindowId) -> bool {
        self.command_palette.is_open()
            && self.palette_attached_window.or(self.main_window_id) == Some(window_id)
    }

    pub(super) fn command_palette_handle_ime_in_window(
        &mut self,
        window_id: WindowId,
        ime_event: &winit::event::Ime,
    ) -> bool {
        self.command_palette_owns_input(window_id) && self.command_palette_handle_ime(ime_event)
    }

    pub(super) fn command_palette_handle_ime(&mut self, ime_event: &winit::event::Ime) -> bool {
        if !self.command_palette.is_open() {
            // When: command_palette is closed the IME event belongs to the
            // terminal; returning false lets window_event run its commit path.
            return false;
        }
        self.palette_pointer_capture = None;
        self.refresh_command_palette_context();
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
        let mods = self.command_palette_modifiers();
        let text = super::text_edit::printable_event_text(event, mods);
        self.command_palette_handle_input(&event.logical_key, Some(text))
    }

    pub(super) fn command_palette_handle_logical_key(
        &mut self,
        logical_key: &winit::keyboard::Key,
    ) -> bool {
        self.command_palette_handle_input(logical_key, None)
    }

    fn command_palette_handle_input(
        &mut self,
        logical_key: &winit::keyboard::Key,
        event_text: Option<Option<&str>>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.command_palette.is_open() {
            // When: command_palette is closed no palette state may change here;
            // both callers gate on their own checks and discard this false.
            return false;
        }
        self.palette_pointer_capture = None;
        self.refresh_command_palette_context();
        if self.palette_ime_is_composing() {
            // When: palette_ime_is_composing is true the IME owns the keystroke;
            // swallow every key so a half-formed CJK sequence cannot also navigate.
            if matches!(*logical_key, Key::Named(NamedKey::Escape)) {
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
            match logical_key {
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
        } else if let Some(edit) = self.command_palette_text_edit(logical_key) {
            // When: command_palette_text_edit maps the chord to an emacs ctrl edit
            // (ctrl+a, ctrl+k, ctrl+w); it rewrites the query in rename and list modes.
            self.command_palette.apply_text_edit(edit);
            self.update_command_palette_ime_cursor_area();
            self.request_redraw_for_overlay(self.palette_attached_window);
            true
        } else if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::RenameTab
        {
            // When: mode is RenameTab, Enter commits the title to the attached window rather than dispatching a command.
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    let title = self.command_palette.query().trim().to_string();
                    let source = self.palette_attached_window.or(self.main_window_id);
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    if let Some(window) = source.and_then(|id| self.windows.get_mut(&id)) {
                        window.tabs.set_active_custom_title(title);
                        window.request_redraw();
                    }
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    self.command_palette.backspace();
                    self.update_command_palette_ime_cursor_area();
                    self.request_redraw_for_overlay(self.palette_attached_window);
                    true
                }
                Key::Named(NamedKey::Space) | Key::Character(_) => {
                    self.command_palette_input_text(logical_key, event_text);
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
                _ => true,
            }
        } else {
            // When: mode is neither TabColor nor RenameTab the palette shows
            // the command list, where Enter runs the selected action.
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    // When: Enter arrives in list mode it runs the highlighted
                    // entry; RenameTab and UpdateTabColor re-enter sub-modes.
                    let Some(action) = self.command_palette.current().cloned() else {
                        // When: `current` is absent, keep a disabled or empty result visible without dispatch.
                        self.request_redraw_for_overlay(self.palette_attached_window);
                        return true;
                    };
                    let source_window = self.palette_attached_window.or(self.main_window_id);
                    let action = match action {
                        sonicterm_ui::command_palette::PaletteEntry::Command(action) => action,
                        sonicterm_ui::command_palette::PaletteEntry::Tab { id, .. } => {
                            // When: `Tab` supplies `id`, resolve it in the captured source before closing the palette.
                            let target = source_window.and_then(|window_id| {
                                let window =
                                    self.windows.get(&window_id).filter(|window| !window.hidden)?;
                                window
                                    .tabs
                                    .tabs()
                                    .iter()
                                    .position(|tab| tab.id == id)
                                    .map(|index| (window_id, index))
                            });
                            let Some((window_id, index)) = target else {
                                // When: `target` vanished, never substitute another tab or window at the old position.
                                self.request_redraw_for_overlay(self.palette_attached_window);
                                return true;
                            };
                            self.command_palette.close();
                            self.palette_attached_window = None;
                            self.run_action_for_window(&Action::ActivateTab(index), window_id);
                            self.request_redraw_for_overlay(Some(window_id));
                            return true;
                        }
                    };
                    if matches!(action, sonicterm_cfg::keymap::Action::RenameTab) {
                        // When: matches finds RenameTab the palette stays open as
                        // a rename editor seeded with the active tab title.
                        let body = source_window
                            .and_then(|id| self.windows.get(&id))
                            .and_then(|window| window.tabs.active_title_body())
                            .unwrap_or_default();
                        self.command_palette.start_rename_tab(body);
                        self.update_command_palette_ime_cursor_area();
                        self.request_redraw_for_overlay(self.palette_attached_window);
                        return true;
                    }
                    if matches!(action, sonicterm_cfg::keymap::Action::UpdateTabColor) {
                        // When: matches finds UpdateTabColor the palette switches
                        // to the tab color picker instead of closing.
                        let title = source_window
                            .and_then(|id| self.windows.get(&id))
                            .and_then(|window| window.tabs.active_title_body())
                            .unwrap_or_default();
                        let choices = theme_tab_color_choices(&self.theme);
                        self.command_palette.start_tab_color_picker(title, choices);
                        self.request_redraw_for_overlay(self.palette_attached_window);
                        return true;
                    }
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    if let Some(source_window) = source_window {
                        self.run_action_for_window(&action, source_window);
                    } else {
                        // When: `source_window` is absent, only target-free commands reach the existing dispatcher.
                        self.run_action(&action);
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
                Key::Named(NamedKey::Space) | Key::Character(_) => {
                    self.command_palette_input_text(logical_key, event_text);
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
                _ => true, // swallow other keys while palette is open
            }
        }
    }
    /// Open live-tab navigation for the explicit window without changing action routing ownership.
    pub(super) fn open_tab_selector(&mut self, window_id: WindowId) {
        if self.windows.get(&window_id).is_none_or(|window| window.hidden) {
            // When: `window_id` is missing or hidden, do not populate a selector from another window.
            return;
        }
        self.palette_pointer_capture = None;
        self.palette_attached_window =
            (Some(window_id) != self.main_window_id).then_some(window_id);
        // Seed the requested window before opening selects a row from the cached tab inventory.
        self.command_palette.set_tabs(&self.windows[&window_id].tabs, &self.i18n);
        self.command_palette.open_tabs();
        self.refresh_command_palette_context();
        self.update_command_palette_ime_cursor_area();
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    pub(super) fn toggle_command_palette(&mut self) {
        self.palette_pointer_capture = None;
        self.refresh_command_palette_context();
        let now_open = self.command_palette.toggle();
        // Notify the reducer of the toggle. The reducer flips `palette_open`
        // and emits Render(Overlay) on every transition.
        self.observe_intent(sonicterm_app_core::AppIntent::ToggleCommandPalette {
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
        self.palette_pointer_capture = None;
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

    pub(super) fn start_update_tab_color(&mut self) {
        self.palette_pointer_capture = None;
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
        let source = self.palette_attached_window.or(self.main_window_id);
        if let Some(window) = source.and_then(|id| self.windows.get_mut(&id)) {
            if let Some(hex) = choice.hex {
                window.tabs.set_active_custom_color(hex);
            } else {
                // When: choice has no hex, restore the attached window's active tab to its theme color.
                window.tabs.clear_active_custom_color();
            }
            window.request_redraw();
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
        self.observe_intent(sonicterm_app_core::AppIntent::OpenSearch {
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
