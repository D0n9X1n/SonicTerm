//! Selection commands and clipboard ownership shared by the command-palette and search fields.
//!
//! A field that owns a window's input also owns that window's configured copy and
//! paste, ahead of READONLY and terminal routing, so field text never reaches a PTY
//! and a field copy never falls back to the terminal selection underneath it.

use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_ui::command_palette::CommandPaletteMode;
use sonicterm_ui::text_edit::TextEdit;
use winit::{
    event::KeyEvent,
    keyboard::{Key, ModifiersState, NamedKey},
    platform::modifier_supplement::KeyEventExtModifierSupplement,
    window::WindowId,
};

use super::{key_encoding::key_event_to_strings, App};

/// One editing command a focused single-line field applies to its query and selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldCommand {
    /// Plain move or deletion: moves collapse a selection and deletions remove it first.
    Edit(TextEdit),
    /// Navigation that moves the caret while keeping the selection anchor.
    Extend(TextEdit),
    /// Select the whole query.
    SelectAll,
}

/// Map a layout-resolved key and the source window's modifiers to a field command.
///
/// Windows Ctrl+A selects the whole field; macOS and Linux keep Ctrl+A as the
/// readline move-to-start. Shift with Left, Right, Home, or End extends the
/// selection on every platform; any other modifier set keeps the core mapping.
pub(super) fn field_command_for_key(key: &Key, mods: ModifiersState) -> Option<FieldCommand> {
    if cfg!(target_os = "windows")
        && mods == ModifiersState::CONTROL
        && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("a"))
    {
        // When: cfg!(target_os = "windows") holds and mods is exactly CONTROL on key a, select the whole field.
        return Some(FieldCommand::SelectAll);
    }
    if mods == ModifiersState::SHIFT {
        // When: mods is exactly SHIFT, a navigation key extends the selection instead of moving the caret.
        let extend = match key {
            Key::Named(NamedKey::ArrowLeft) => Some(TextEdit::MoveBackward),
            Key::Named(NamedKey::ArrowRight) => Some(TextEdit::MoveForward),
            Key::Named(NamedKey::Home) => Some(TextEdit::MoveStart),
            Key::Named(NamedKey::End) => Some(TextEdit::MoveEnd),
            _ => None,
        };
        if let Some(edit) = extend {
            // When: extend names a navigation edit for key, move the caret around a kept anchor.
            return Some(FieldCommand::Extend(edit));
        }
    }
    super::text_edit::search_text_edit_for_key(key, mods).map(FieldCommand::Edit)
}

/// Map a native key event through its unmodified layout key, as search editing does.
pub(super) fn field_command_for_event(
    event: &KeyEvent,
    mods: ModifiersState,
) -> Option<FieldCommand> {
    field_command_for_key(&event.key_without_modifiers(), mods)
}

/// Configured binding a focused field sees for one native key event.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FieldBinding {
    /// No alias of the event is bound.
    Unbound,
    /// The first bound alias is copy or paste; the chord keeps passthrough checks exact.
    Clipboard { chord: String, action: Action },
    /// The first bound alias is another action; `primary` is true when it is the first alias.
    Other { chord: String, action: Action, primary: bool },
}

/// Resolve the first bound alias of `event`, so a later clipboard alias never overrides an earlier binding.
pub(super) fn first_field_binding(
    keymap: &Keymap,
    event: &KeyEvent,
    mods: ModifiersState,
) -> FieldBinding {
    for (index, chord) in key_event_to_strings(event, mods).into_iter().enumerate() {
        let Some(action) = keymap.lookup(&chord).cloned() else {
            // When: chord is unbound, the next layout alias may still carry the configured binding.
            continue;
        };
        if matches!(action, Action::CopyToClipboard | Action::PasteFromClipboard) {
            // When: action matches CopyToClipboard or PasteFromClipboard, the field owns it whichever chord alias matched first.
            return FieldBinding::Clipboard { chord, action };
        }
        return FieldBinding::Other { chord, action, primary: index == 0 };
    }
    FieldBinding::Unbound
}

impl App {
    /// Consume configured copy or paste for `window_id`'s editable field before READONLY or terminal routing.
    ///
    /// The palette owner is checked first, then the window's active-tab search.
    /// Returns `false` for any other action, or when no field owns the window, so
    /// the action keeps that window's normal route.
    pub(super) fn run_field_clipboard_action(
        &mut self,
        action: &Action,
        window_id: WindowId,
    ) -> bool {
        let copy = match action {
            Action::CopyToClipboard => true,
            Action::PasteFromClipboard => false,
            _ => {
                // When: action is not a clipboard action, fields leave it to the ordinary dispatcher.
                return false;
            }
        };
        let Some(window) = self.windows.get(&window_id) else {
            // When: window_id is gone, no field can own its clipboard action.
            return false;
        };
        if self.command_palette_owns_input(window_id) {
            // When: the visible palette belongs to window_id, it consumes the action even when it cannot edit.
            self.palette_field_clipboard(copy, window_id);
            return true;
        }
        let Some(search) =
            window.tab_states.get(window.tabs.active_index()).and_then(|tab| tab.search.as_ref())
        else {
            // When: the active tab has no search, window_id has no field and keeps its terminal route.
            return false;
        };
        if window.hidden || window.ime.is_composing() {
            // When: the owning window is hidden or ime is composing, consume the action with no clipboard or query change.
            return true;
        }
        if copy {
            // When: copy is requested, only the search query's own selection may reach the clipboard.
            if let Some(text) = search.selected_text().map(str::to_owned) {
                // When: selected_text is nonempty, write it and keep the selection; a failed write changes nothing.
                let _ = self.set_clipboard_text(text);
            }
        } else if let Some(text) = self.read_clipboard_text() {
            // When: the clipboard produced text, commit it once; a missing pane keeps the query unchanged.
            let _ = self.search_handle_ime_commit(window_id, &text);
        }
        true
    }

    /// Apply field copy or paste to the palette, doing nothing for a modal, stale, hidden, or composing editor.
    fn palette_field_clipboard(&mut self, copy: bool, window_id: WindowId) {
        let editable = match self.command_palette.mode() {
            CommandPaletteMode::Commands => true,
            CommandPaletteMode::RenameTab => self.tab_edit_target_window() == Some(window_id),
            CommandPaletteMode::RenameWindow => {
                self.window_rename_target.and_then(|key| self.window_keys.resolve(key))
                    == Some(window_id)
            }
            CommandPaletteMode::TabColor => false,
        };
        if !editable
            || self.windows.get(&window_id).is_none_or(|window| window.hidden)
            || self.palette_ime_is_composing()
        {
            // When: the editor is modal, its captured target is stale or hidden, or IME composes, consume without effect.
            return;
        }
        if copy {
            // When: copy is requested, only the palette query's own selection may reach the clipboard.
            if let Some(text) = self.command_palette.selected_text().map(str::to_owned) {
                // When: selected_text is nonempty, write it and keep the selection; a failed write changes nothing.
                let _ = self.set_clipboard_text(text);
            }
            return;
        }
        let Some(text) = self.read_clipboard_text() else {
            // When: the clipboard read failed, the field keeps its text, caret, and anchor.
            return;
        };
        // RenameWindow validates the whole replacement; other modes strip controls once.
        self.command_palette.input_str(&text);
        self.update_command_palette_ime_cursor_area();
        self.request_redraw_for_overlay(self.palette_attached_window);
    }
}

#[cfg(test)]
#[path = "field_input_tests.rs"]
mod field_input_tests;
