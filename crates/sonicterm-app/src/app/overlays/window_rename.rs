use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::window::WindowId;

use crate::app::App;

impl App {
    pub(in crate::app) fn start_rename_window(&mut self, id: WindowId) {
        let Some(window) = self.windows.get(&id).filter(|window| !window.hidden) else {
            // When: id is absent or hidden, never borrow a different window's identity.
            return;
        };
        let Some(key) = self.window_keys.get(id) else {
            // When: id has not been admitted, a helper cannot become a rename target.
            return;
        };
        self.interaction.command_palette.start_rename_window(window.custom_window_name.clone());
        self.interaction.window_rename_target = Some(key);
        self.interaction.palette_attached_window = (Some(id) != self.main_window_id).then_some(id);
        self.interaction.palette_pointer_capture = None;
        self.update_command_palette_ime_cursor_area();
        self.request_redraw_for_overlay(Some(id));
    }

    pub(in crate::app) fn cancel_window_rename(&mut self, id: WindowId) {
        if self.interaction.command_palette.mode() == CommandPaletteMode::RenameWindow
            && self.interaction.window_rename_target.and_then(|key| self.window_keys.resolve(key))
                == Some(id)
        {
            self.interaction.command_palette.close();
            self.interaction.window_rename_target = None;
            self.interaction.palette_attached_window = None;
            self.interaction.palette_pointer_capture = None;
        }
    }

    pub(super) fn submit_window_name(&mut self) {
        let target = self
            .interaction
            .window_rename_target
            .and_then(|key| self.window_keys.resolve(key))
            .filter(|id| self.windows.contains_key(id));
        let Some(id) = target else {
            // When: target no longer resolves, cancel without selecting the current main window.
            self.interaction.command_palette.close();
            self.interaction.window_rename_target = None;
            self.interaction.palette_attached_window = None;
            return;
        };
        let Some(name) = self.interaction.command_palette.window_name_submission() else {
            // When: window_name_submission rejects input, retain the old title and paint the localized rejection.
            self.request_redraw_for_overlay(Some(id));
            return;
        };
        self.windows.get_mut(&id).unwrap().custom_window_name = name;
        // A user's own rename shows at once, never on a throttled trailing edge.
        let _ = self.sync_native_title(id, std::time::Instant::now(), true);
        self.interaction.command_palette.close();
        self.interaction.window_rename_target = None;
        self.interaction.palette_attached_window = None;
        self.request_redraw_for_overlay(Some(id));
    }
}

#[cfg(test)]
#[path = "window_rename_tests.rs"]
mod window_rename_tests;
