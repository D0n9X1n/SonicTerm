//! Map normalized keyboard chords to shared app-input editing operations.

use sonicterm_ui::text_edit::TextEdit;
use winit::{
    event::KeyEvent,
    keyboard::{Key, ModifiersState, NamedKey},
};

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

pub(super) fn search_text_edit_for_event(
    event: &KeyEvent,
    mods: ModifiersState,
) -> Option<TextEdit> {
    search_text_edit_for_key(&event.logical_key, mods)
}

/// Return printable OS-produced text without turning command chords into input.
pub(super) fn printable_event_text(event: &KeyEvent, mods: ModifiersState) -> Option<&str> {
    printable_text_for_parts(&event.logical_key, event.physical_key, event.text.as_deref(), mods)
}

fn printable_text_for_parts<'a>(
    logical_key: &Key,
    physical_key: winit::keyboard::PhysicalKey,
    event_text: Option<&'a str>,
    mods: ModifiersState,
) -> Option<&'a str> {
    if mods.super_key() {
        return None;
    }
    let text =
        event_text.or_else(|| matches!(logical_key, Key::Named(NamedKey::Space)).then_some(" "))?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    if !mods.control_key() {
        return Some(text);
    }
    if !mods.alt_key() {
        return None;
    }

    // AltGr commonly appears as Ctrl+Alt. Keep its produced glyph when it
    // differs from the unmodified physical key, but reject ordinary Ctrl+Alt.
    let produced = text.chars().next().map(super::key_encoding::unshift_ascii);
    let physical = super::key_encoding::physical_ascii(physical_key);
    (produced != physical).then_some(text)
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
