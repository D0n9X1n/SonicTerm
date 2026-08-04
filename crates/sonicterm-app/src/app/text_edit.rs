//! Map normalized keyboard chords to shared app-input editing operations.

use sonicterm_ui::text_edit::TextEdit;
use winit::keyboard::{Key, ModifiersState};

pub(super) fn core_text_edit_for_key(key: &Key, mods: ModifiersState) -> Option<TextEdit> {
    let chord = super::key_encoding::key_to_string(key, mods)?;
    core_text_edit_for_chord(&chord)
}

pub(super) fn search_text_edit_for_key(key: &Key, mods: ModifiersState) -> Option<TextEdit> {
    if let Some(edit) = core_text_edit_for_key(key, mods) {
        // When: `core_text_edit_for_key` recognized a control chord, preserve that command before considering unmodified navigation keys.
        return Some(edit);
    }
    if !mods.is_empty() {
        // When: `mods` remains nonempty after core-chord lookup, do not reinterpret a modified key as plain search-field navigation.
        return None;
    }
    Some(match key {
        Key::Named(winit::keyboard::NamedKey::ArrowLeft) => TextEdit::MoveBackward,
        Key::Named(winit::keyboard::NamedKey::ArrowRight) => TextEdit::MoveForward,
        Key::Named(winit::keyboard::NamedKey::Home) => TextEdit::MoveStart,
        Key::Named(winit::keyboard::NamedKey::End) => TextEdit::MoveEnd,
        Key::Named(winit::keyboard::NamedKey::Delete) => TextEdit::DeleteForward,
        _ => {
            // When: `key` is not a supported unmodified editing key, leave it available to other input handling.
            return None;
        }
    })
}

pub(super) fn core_text_edit_for_chord(chord: &str) -> Option<TextEdit> {
    Some(match chord {
        "ctrl+a" => TextEdit::MoveStart,
        "ctrl+e" => TextEdit::MoveEnd,
        "ctrl+b" => TextEdit::MoveBackward,
        "ctrl+f" => TextEdit::MoveForward,
        "ctrl+h" => TextEdit::DeleteBackward,
        "ctrl+d" => TextEdit::DeleteForward,
        "ctrl+w" => TextEdit::DeletePreviousWord,
        "ctrl+u" => TextEdit::DeleteToStart,
        "ctrl+k" => TextEdit::DeleteToEnd,
        _ => {
            // When: `chord` is outside the shared editing set, report no command rather than consuming it.
            return None;
        }
    })
}

#[cfg(test)]
#[path = "text_edit_tests.rs"]
mod text_edit_tests;
