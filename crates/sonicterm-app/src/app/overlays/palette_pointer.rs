use sonicterm_ui::command_palette::{CommandPaletteMode, PaletteEntry};
use sonicterm_ui::overlays::PaletteLayout;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{Key, NamedKey},
    window::{CursorIcon, WindowId},
};

use crate::app::App;

pub(in crate::app) struct PalettePointerCapture {
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

impl App {
    fn command_palette_pointer_hit(&mut self, window_id: WindowId) -> Option<PalettePointerHit> {
        let window = self.windows.get(&window_id)?;
        let renderer = window.renderer.as_ref()?;
        let (width, height) = renderer.logical_size();
        let (pointer_x, pointer_y) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let layout = PaletteLayout::compute(
            &mut self.command_palette,
            width,
            height,
            self.config.appearance.panel_padding,
            renderer.scale_factor(),
        )?;
        if !layout.border.contains(pointer_x, pointer_y) {
            // When: `layout.border` excludes the pointer, reserve an outside dismissal rather than a terminal click.
            return Some(PalettePointerHit::Outside);
        }
        if let Some(row) = layout.rows.iter().find(|row| row.rect.contains(pointer_x, pointer_y)) {
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
    pub(in crate::app) fn command_palette_handle_pointer_event(
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
                    MouseScrollDelta::LineDelta(_, vertical_lines) => f64::from(*vertical_lines),
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
}

#[cfg(test)]
#[path = "palette_pointer_tests.rs"]
mod palette_pointer_tests;
