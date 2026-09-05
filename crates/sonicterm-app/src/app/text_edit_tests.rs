use super::*;

#[test]
fn core_control_chords_map_to_terminal_style_edits() {
    let cases = [
        ("ctrl+a", TextEdit::MoveStart),
        ("ctrl+e", TextEdit::MoveEnd),
        ("ctrl+b", TextEdit::MoveBackward),
        ("ctrl+f", TextEdit::MoveForward),
        ("ctrl+h", TextEdit::DeleteBackward),
        ("ctrl+d", TextEdit::DeleteForward),
        ("ctrl+w", TextEdit::DeletePreviousWord),
        ("ctrl+u", TextEdit::DeleteToStart),
        ("ctrl+k", TextEdit::DeleteToEnd),
    ];

    for (chord, expected) in cases {
        assert_eq!(core_text_edit_for_chord(chord), Some(expected), "{chord}");
    }
}

#[test]
fn only_exact_core_control_chords_are_mapped() {
    for chord in ["a", "ctrl+shift+a", "ctrl+alt+w", "super+ctrl+k", "ctrl+c"] {
        assert_eq!(core_text_edit_for_chord(chord), None, "{chord}");
    }
}

#[test]
fn search_named_keys_map_only_without_modifiers() {
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cases = [
        (NamedKey::ArrowLeft, TextEdit::MoveBackward),
        (NamedKey::ArrowRight, TextEdit::MoveForward),
        (NamedKey::Home, TextEdit::MoveStart),
        (NamedKey::End, TextEdit::MoveEnd),
        (NamedKey::Delete, TextEdit::DeleteForward),
    ];
    for (key, expected) in cases {
        assert_eq!(
            search_text_edit_for_key(&Key::Named(key), ModifiersState::empty()),
            Some(expected),
        );
        assert_eq!(search_text_edit_for_key(&Key::Named(key), ModifiersState::SUPER), None,);
    }
}

#[test]
fn logical_keys_and_modifier_state_use_the_same_exact_mapping() {
    use winit::keyboard::{Key, ModifiersState};

    assert_eq!(
        core_text_edit_for_key(&Key::Character("W".into()), ModifiersState::CONTROL),
        Some(TextEdit::DeletePreviousWord),
    );
    assert_eq!(
        core_text_edit_for_key(
            &Key::Character("w".into()),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        ),
        None,
    );
}

/// App text fields accept produced glyphs but never type ordinary command chords.
#[test]
fn printable_text_policy_distinguishes_altgr_from_command_modifiers() {
    let q = Key::Character("@".into());
    let unmodified_q = Key::Character("q".into());
    assert_eq!(
        printable_text_for_parts(
            &q,
            &unmodified_q,
            Some("@"),
            ModifiersState::CONTROL | ModifiersState::ALT,
        ),
        Some("@"),
    );

    let x = Key::Character("x".into());
    let unmodified_x = Key::Character("x".into());
    for modifiers in [
        ModifiersState::ALT,
        ModifiersState::ALT | ModifiersState::SHIFT,
        ModifiersState::CONTROL | ModifiersState::ALT,
        ModifiersState::SUPER,
    ] {
        assert_eq!(
            printable_text_for_parts(&x, &unmodified_x, Some("x"), modifiers),
            None,
            "{modifiers:?}",
        );
    }
    assert_eq!(
        printable_text_for_parts(
            &Key::Character("å".into()),
            &Key::Character("a".into()),
            Some("å"),
            ModifiersState::ALT,
        ),
        Some("å"),
    );

    for unmodified in
        [Key::Dead(Some('^')), Key::Unidentified(winit::keyboard::NativeKey::Unidentified)]
    {
        assert_eq!(
            printable_text_for_parts(
                &Key::Character("^".into()),
                &unmodified,
                Some("^"),
                ModifiersState::ALT,
            ),
            None,
            "{unmodified:?}",
        );
    }
}
