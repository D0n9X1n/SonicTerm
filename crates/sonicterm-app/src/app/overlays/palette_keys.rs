use sonicterm_cfg::keymap::Action;
use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::{event::KeyEvent, keyboard::ModifiersState};

use super::tab_edit::theme_tab_color_choices;
use crate::app::{key_encoding::key_event_to_string, App};

impl App {
    fn command_palette_text_edit(
        &self,
        logical_key: &winit::keyboard::Key,
    ) -> Option<sonicterm_ui::text_edit::TextEdit> {
        let mods = match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|window| window.modifiers),
            None => self.main().map(|window| window.modifiers),
        }
        .unwrap_or_else(ModifiersState::empty);
        super::text_edit::core_text_edit_for_key(logical_key, mods)
    }

    fn command_palette_modifiers(&self) -> ModifiersState {
        match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|window| window.modifiers),
            None => self.main().map(|window| window.modifiers),
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
        if self.command_palette.mode() == CommandPaletteMode::RenameWindow {
            self.command_palette.input_window_name(text.unwrap_or_default());
        } else {
            // When: another palette mode owns input, retain its existing printable-character policy.
            for character in
                text.unwrap_or_default().chars().filter(|character| !character.is_control())
            {
                self.command_palette.input_char(character);
            }
        }
    }

    pub(in crate::app) fn command_palette_handle_key(&mut self, event: &KeyEvent) -> bool {
        let mods = self.command_palette_modifiers();
        if self.command_palette.mode() == CommandPaletteMode::RenameWindow
            && !self.palette_ime_is_composing()
            && key_event_to_string(event, mods).and_then(|key| self.keymap.lookup(&key))
                == Some(&Action::PasteFromClipboard)
        {
            // When: RenameWindow receives PasteFromClipboard, keep the payload local even in READONLY.
            self.paste_window_name();
            return true;
        }
        let text = super::text_edit::printable_event_text(event, mods);
        self.command_palette_handle_input(&event.logical_key, Some(text))
    }

    pub(in crate::app) fn command_palette_handle_logical_key(
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
                if let Some(window) = match self.palette_attached_window {
                    Some(id) => self.windows.get_mut(&id),
                    None => self.main_mut(),
                } {
                    window.ime.cancel();
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
                    self.tab_edit_target = None;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    let source = self.palette_attached_window.or(self.main_window_id);
                    self.apply_selected_tab_color();
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    // Repaint the host even when a closed or moved tab left nothing to change.
                    self.request_redraw_for_overlay(source);
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
            // When: command_palette_text_edit recognizes a native or Control edit, only the attached editor consumes it.
            self.command_palette.apply_text_edit(edit);
            self.update_command_palette_ime_cursor_area();
            self.request_redraw_for_overlay(self.palette_attached_window);
            true
        } else if matches!(
            self.command_palette.mode(),
            CommandPaletteMode::RenameTab | CommandPaletteMode::RenameWindow
        ) {
            // When: matches! selects RenameTab or RenameWindow, consume keys locally instead of dispatching terminal commands.
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    let source = self.palette_attached_window.or(self.main_window_id);
                    self.command_palette.close();
                    self.window_rename_target = None;
                    self.tab_edit_target = None;
                    self.palette_attached_window = None;
                    self.request_redraw_for_overlay(source);
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    // When: Enter ends a name edit, window and tab names use stable targets
                    // rather than active-tab state.
                    if self.command_palette.mode() == CommandPaletteMode::RenameWindow {
                        // When: RenameWindow owns the editor, validation must finish before changing the native title.
                        self.submit_window_name();
                        return true;
                    }
                    let title = self.command_palette.query().trim().to_string();
                    let source = self.palette_attached_window.or(self.main_window_id);
                    self.command_palette.close();
                    self.palette_attached_window = None;
                    // A closed or moved tab keeps its name; the title never lands on another tab.
                    self.edit_captured_tab(|tabs, tab| tabs.set_custom_title(tab, title));
                    self.request_redraw_for_overlay(source);
                    true
                }
                Key::Named(NamedKey::Backspace)
                    if !self.command_palette_modifiers().intersects(
                        ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER,
                    ) =>
                {
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
                        sonicterm_ui::command_palette::PaletteEntry::About => {
                            // When: About is selected, notify the captured palette owner rather than the current focus.
                            self.command_palette.close();
                            self.palette_attached_window = None;
                            if let Some(id) =
                                source_window.filter(|id| self.windows.contains_key(id))
                            {
                                self.show_notification_for_kind(
                                    self.kind_for(id),
                                    sonicterm_ui::overlays::NotificationLevel::Info,
                                    format!("SonicTerm {}", env!("CARGO_PKG_VERSION")),
                                );
                            }
                            self.request_redraw_for_overlay(source_window);
                            return true;
                        }
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
                    if action == Action::RenameWindow {
                        // When: action is RenameWindow, capture source_window before another focus event can redirect it.
                        if let Some(id) = source_window {
                            self.start_rename_window(id);
                        }
                        return true;
                    }
                    if matches!(action, sonicterm_cfg::keymap::Action::RenameTab) {
                        // When: matches finds RenameTab the palette stays open as
                        // a rename editor seeded with the active tab title.
                        let body = source_window
                            .and_then(|id| self.windows.get(&id))
                            .and_then(|window| window.tabs.active_title_body())
                            .unwrap_or_default();
                        self.command_palette.start_rename_tab(body);
                        self.capture_tab_edit_target(source_window);
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
                        self.capture_tab_edit_target(source_window);
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
                Key::Named(NamedKey::Backspace)
                    if !self.command_palette_modifiers().intersects(
                        ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER,
                    ) =>
                {
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
}

#[cfg(test)]
#[path = "palette_keys_tests.rs"]
mod palette_keys_tests;
