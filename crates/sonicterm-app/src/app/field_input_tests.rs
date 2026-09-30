use super::*;

/// Ctrl+A selects the whole field only on Windows; macOS and Linux keep the readline move-to-start.
#[test]
fn ctrl_a_selects_all_only_on_windows() {
    for text in ["a", "A"] {
        let command = field_command_for_key(&Key::Character(text.into()), ModifiersState::CONTROL);
        if cfg!(target_os = "windows") {
            assert_eq!(command, Some(FieldCommand::SelectAll), "{text}");
        } else {
            assert_eq!(command, Some(FieldCommand::Edit(TextEdit::MoveStart)), "{text}");
        }
    }
    // Any extra modifier leaves Ctrl+A outside the select-all convention.
    let extra = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_ne!(
        field_command_for_key(&Key::Character("a".into()), extra),
        Some(FieldCommand::SelectAll)
    );
}

/// Exactly Shift extends Left, Right, Home, and End; other modifier sets keep their plain mapping.
#[test]
fn shift_navigation_extends_and_other_modifiers_stay_exact() {
    for (key, edit) in [
        (NamedKey::ArrowLeft, TextEdit::MoveBackward),
        (NamedKey::ArrowRight, TextEdit::MoveForward),
        (NamedKey::Home, TextEdit::MoveStart),
        (NamedKey::End, TextEdit::MoveEnd),
    ] {
        assert_eq!(
            field_command_for_key(&Key::Named(key), ModifiersState::SHIFT),
            Some(FieldCommand::Extend(edit)),
            "{key:?}"
        );
        assert_eq!(
            field_command_for_key(&Key::Named(key), ModifiersState::empty()),
            Some(FieldCommand::Edit(edit)),
            "{key:?}"
        );
        // Shift with another modifier is not an extension and is not a plain move either.
        assert_eq!(
            field_command_for_key(&Key::Named(key), ModifiersState::SHIFT | ModifiersState::ALT),
            None,
            "{key:?}"
        );
    }
    // Shift+Delete is not navigation, so it neither extends nor becomes a plain deletion.
    assert_eq!(field_command_for_key(&Key::Named(NamedKey::Delete), ModifiersState::SHIFT), None);
}

/// Plain moves and deletions keep the shared core mapping, now applied selection-aware by the field.
#[test]
fn plain_edits_keep_the_core_mapping() {
    for (chord_key, modifiers, edit) in [
        (Key::Named(NamedKey::Delete), ModifiersState::empty(), TextEdit::DeleteForward),
        (Key::Character("k".into()), ModifiersState::CONTROL, TextEdit::DeleteToEnd),
        (Key::Character("w".into()), ModifiersState::CONTROL, TextEdit::DeletePreviousWord),
    ] {
        assert_eq!(
            field_command_for_key(&chord_key, modifiers),
            Some(FieldCommand::Edit(edit)),
            "{chord_key:?}"
        );
    }
}
