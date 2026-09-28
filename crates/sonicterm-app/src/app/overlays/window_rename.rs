use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::window::WindowId;

use crate::app::{App, FrontmostKind};

impl App {
    /// Consume paste for the routed window's rename editor before search or terminal admission.
    pub(in crate::app) fn paste_window_name_for_kind(&mut self, kind: FrontmostKind) -> bool {
        let window_id = match kind {
            FrontmostKind::Child(id) => Some(id),
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => self.main_window_id,
        };
        let Some(id) = window_id else {
            // When: window_id is absent, no terminal window can own the rename editor.
            return false;
        };
        if !self.command_palette_owns_input(id)
            || self.command_palette.mode() != CommandPaletteMode::RenameWindow
        {
            // When: the rename editor does not own id, preserve that window's normal paste routing.
            return false;
        }
        let target = self.window_rename_target.and_then(|key| self.window_keys.resolve(key));
        if target != Some(id) || self.windows.get(&id).is_none_or(|window| window.hidden) {
            // When: the captured target is stale or hidden, consume the modal's paste without terminal fallback.
            return true;
        }
        if self.palette_ime_is_composing() {
            // When: palette_ime_is_composing is true, paste must not insert beside unfinished IME text.
            return true;
        }
        self.paste_window_name();
        // Rejected, empty, or unavailable clipboard text still belongs to the editor, never a PTY.
        true
    }

    pub(super) fn paste_window_name(&mut self) {
        let text = self
            .test_clipboard_text
            .clone()
            .or_else(|| self.clipboard.as_mut().and_then(|clipboard| clipboard.get_text().ok()));
        if let Some(text) = text {
            self.command_palette.input_window_name(&text);
            self.update_command_palette_ime_cursor_area();
            self.request_redraw_for_overlay(self.palette_attached_window);
        }
    }

    pub(in crate::app) fn start_rename_window(&mut self, id: WindowId) {
        let Some(window) = self.windows.get(&id).filter(|window| !window.hidden) else {
            // When: id is absent or hidden, never borrow a different window's identity.
            return;
        };
        let Some(key) = self.window_keys.get(id) else {
            // When: id has not been admitted, a helper cannot become a rename target.
            return;
        };
        self.command_palette.start_rename_window(window.custom_window_name.clone());
        self.window_rename_target = Some(key);
        self.palette_attached_window = (Some(id) != self.main_window_id).then_some(id);
        self.palette_pointer_capture = None;
        self.update_command_palette_ime_cursor_area();
        self.request_redraw_for_overlay(Some(id));
    }

    pub(in crate::app) fn cancel_window_rename(&mut self, id: WindowId) {
        if self.command_palette.mode() == CommandPaletteMode::RenameWindow
            && self.window_rename_target.and_then(|key| self.window_keys.resolve(key)) == Some(id)
        {
            self.command_palette.close();
            self.window_rename_target = None;
            self.palette_attached_window = None;
            self.palette_pointer_capture = None;
        }
    }

    pub(super) fn submit_window_name(&mut self) {
        let target = self
            .window_rename_target
            .and_then(|key| self.window_keys.resolve(key))
            .filter(|id| self.windows.contains_key(id));
        let Some(id) = target else {
            // When: target no longer resolves, cancel without selecting the current main window.
            self.command_palette.close();
            self.window_rename_target = None;
            self.palette_attached_window = None;
            return;
        };
        let Some(name) = self.command_palette.window_name_submission() else {
            // When: window_name_submission rejects input, retain the old title and paint the localized rejection.
            self.request_redraw_for_overlay(Some(id));
            return;
        };
        self.windows.get_mut(&id).unwrap().custom_window_name = name;
        if let Some(native) = &self.windows[&id].window {
            native.set_title(&self.native_window_title(id).unwrap());
        }
        self.command_palette.close();
        self.window_rename_target = None;
        self.palette_attached_window = None;
        self.request_redraw_for_overlay(Some(id));
    }
}

#[cfg(test)]
#[path = "window_rename_tests.rs"]
mod window_rename_tests;
