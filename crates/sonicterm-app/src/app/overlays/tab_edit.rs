use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::{CommandPaletteMode, TabColorChoice};
use winit::window::WindowId;

use crate::app::{App, FrontmostKind};

/// Window and stable tab identity captured when a rename or color editor opens.
///
/// Submit resolves this exact tab in its original live window, and does nothing when
/// the tab closed or moved; it never falls back to whichever tab is active by then.
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct TabEditTarget {
    window: sonicterm_types::WindowKey,
    tab: sonicterm_ui::tabs::TabId,
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
    /// Record the active tab of `window_id` as the only tab a rename or color editor may change.
    pub(super) fn capture_tab_edit_target(&mut self, window_id: Option<WindowId>) {
        self.tab_edit_target = window_id.and_then(|id| {
            let window = self.window_keys.get(id)?;
            let tab = self.windows.get(&id)?.tabs.active()?.id;
            Some(TabEditTarget { window, tab })
        });
    }

    /// Apply `edit` only while the captured tab still lives in its original window.
    ///
    /// The capture is consumed. A closed window, a closed tab, or a tab moved to
    /// another window leaves every tab unchanged; the edit never falls back to the
    /// active tab.
    pub(super) fn edit_captured_tab(
        &mut self,
        edit: impl FnOnce(&mut sonicterm_ui::tabs::TabBar, sonicterm_ui::tabs::TabId) -> bool,
    ) {
        let Some(target) = self.tab_edit_target.take() else {
            // When: no target was captured, no editor opened for a tab and nothing may change.
            return;
        };
        let Some(window) =
            self.window_keys.resolve(target.window).and_then(|id| self.windows.get_mut(&id))
        else {
            // When: resolve finds no live window, the captured tab closed with it.
            return;
        };
        if edit(&mut window.tabs, target.tab) {
            window.request_redraw();
        }
    }

    /// Close a tab rename or color editor whose captured tab belongs to the closing window `id`.
    pub(in crate::app) fn cancel_tab_edit(&mut self, id: WindowId) {
        let editing_tab = matches!(
            self.command_palette.mode(),
            CommandPaletteMode::RenameTab | CommandPaletteMode::TabColor
        );
        let captured =
            self.tab_edit_target.and_then(|target| self.window_keys.resolve(target.window));
        if editing_tab && captured == Some(id) {
            self.command_palette.close();
            self.tab_edit_target = None;
            self.palette_attached_window = None;
            self.palette_pointer_capture = None;
        }
    }

    pub(in crate::app) fn start_rename_active_tab(&mut self) {
        self.palette_pointer_capture = None;
        let body = self.active_tab_title_body().unwrap_or_default();
        self.command_palette.start_rename_tab(body);
        self.palette_attached_window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        self.capture_tab_edit_target(self.palette_attached_window.or(self.main_window_id));
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

    pub(in crate::app) fn start_update_tab_color(&mut self) {
        self.palette_pointer_capture = None;
        let title = self.active_tab_title_body().unwrap_or_else(|| "current tab".to_string());
        let choices = theme_tab_color_choices(&self.theme);
        self.command_palette.start_tab_color_picker(title, choices);
        self.palette_attached_window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        self.capture_tab_edit_target(self.palette_attached_window.or(self.main_window_id));
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    pub(super) fn apply_selected_tab_color(&mut self) {
        let Some(choice) = self.command_palette.selected_tab_color().cloned() else {
            // When: selected_tab_color has no entry at the current index the
            // picker is empty or the selection is stale; leave the color as is.
            self.tab_edit_target = None;
            return;
        };
        // Recolor only the tab captured when the picker opened, never whichever tab is active now.
        self.edit_captured_tab(|tabs, tab| match choice.hex {
            Some(hex) => tabs.set_custom_color(tab, hex),
            // Reset to Default restores the captured tab's theme color.
            None => tabs.clear_custom_color(tab),
        });
    }
}

#[cfg(test)]
#[path = "tab_edit_tests.rs"]
mod tab_edit_tests;
