//! Map normalized keyboard chords to shared app-input editing operations.

use sonicterm_ui::text_edit::TextEdit;
use winit::keyboard::{Key, ModifiersState};

pub(super) fn core_text_edit_for_key(key: &Key, mods: ModifiersState) -> Option<TextEdit> {
    let chord = super::key_encoding::key_to_string(key, mods)?;
    core_text_edit_for_chord(&chord)
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
        _ => return None,
    })
}

#[cfg(test)]
#[path = "text_edit_tests.rs"]
mod text_edit_tests;
