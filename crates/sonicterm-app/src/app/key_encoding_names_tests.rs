//! Extended key and keymap spelling contracts.

use super::super::key_encoding_tests::*;
use super::*;

/// Cocoa function-key text must not replace escape sequences or become Kitty associated text.
#[test]
fn cocoa_named_keys_ignore_native_function_text() {
    for (named, physical, native_text) in [
        (NamedKey::ArrowUp, KeyCode::ArrowUp, "\u{f700}"),
        (NamedKey::ArrowDown, KeyCode::ArrowDown, "\u{f701}"),
        (NamedKey::ArrowLeft, KeyCode::ArrowLeft, "\u{f702}"),
        (NamedKey::ArrowRight, KeyCode::ArrowRight, "\u{f703}"),
        (NamedKey::F1, KeyCode::F1, "\u{f704}"),
        (NamedKey::Home, KeyCode::Home, "\u{f729}"),
        (NamedKey::End, KeyCode::End, "\u{f72b}"),
        (NamedKey::PageUp, KeyCode::PageUp, "\u{f72c}"),
        (NamedKey::PageDown, KeyCode::PageDown, "\u{f72d}"),
        (NamedKey::Delete, KeyCode::Delete, "\u{f728}"),
    ] {
        let key = Key::Named(named);
        for flags in 0..32 {
            for app_cursor in [false, true] {
                for mods in
                    [ModifiersState::empty(), ModifiersState::SHIFT, ModifiersState::CONTROL]
                {
                    for (state, repeat) in [
                        (ElementState::Pressed, false),
                        (ElementState::Pressed, true),
                        (ElementState::Released, false),
                    ] {
                        let plain = event(
                            &key,
                            PhysicalKey::Code(physical),
                            None,
                            KeyLocation::Standard,
                            state,
                            repeat,
                        );
                        let mut cocoa = plain;
                        cocoa.text = Some(native_text);
                        cocoa.text_with_all_modifiers = Some(native_text);
                        let modes = KeyboardModes::new(app_cursor, false, false, false, 0);
                        assert_eq!(
                            encode_event(cocoa, mods, flags, modes),
                            encode_event(plain, mods, flags, modes),
                            "{named:?} flags={flags} app_cursor={app_cursor} mods={mods:?} state={state:?} repeat={repeat}",
                        );
                    }
                }
            }
        }
    }
}

/// Genuine private-use character input remains text; only named function-key payloads are excluded.
#[test]
fn private_use_character_input_remains_text() {
    let key = Key::Character("\u{f700}".into());
    let input = event(
        &key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("\u{f700}"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(input, ModifiersState::empty(), 0, KeyboardModes::default()),
        Some("\u{f700}".as_bytes().to_vec())
    );
    assert_eq!(associated_text(input, KITTY_REPORT_ALL | KITTY_REPORT_TEXT), Some(vec![0xf700]));
}

/// Event reporting canonicalizes functional keys but keeps reset-key repeats usable.
#[test]
fn kitty_event_reporting_preserves_reset_key_compatibility() {
    let up_key = Key::Named(NamedKey::ArrowUp);
    let up = event(
        &up_key,
        PhysicalKey::Code(KeyCode::ArrowUp),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    let app_cursor = KeyboardModes::new(true, false, false, false, 0);
    assert_eq!(
        encode_event(up, ModifiersState::empty(), KITTY_REPORT_EVENTS, app_cursor),
        Some(b"\x1b[A".to_vec()),
    );

    for (named, physical, expected) in [
        (NamedKey::Enter, KeyCode::Enter, b"\r".as_slice()),
        (NamedKey::Tab, KeyCode::Tab, b"\t".as_slice()),
        (NamedKey::Backspace, KeyCode::Backspace, b"\x7f".as_slice()),
    ] {
        let key = Key::Named(named);
        let repeat = event(
            &key,
            PhysicalKey::Code(physical),
            None,
            KeyLocation::Standard,
            ElementState::Pressed,
            true,
        );
        assert_eq!(
            encode_event(
                repeat,
                ModifiersState::empty(),
                KITTY_REPORT_EVENTS,
                KeyboardModes::default(),
            ),
            Some(expected.to_vec()),
            "{named:?} repeat",
        );

        let release = event(
            &key,
            PhysicalKey::Code(physical),
            None,
            KeyLocation::Standard,
            ElementState::Released,
            false,
        );
        assert_eq!(
            encode_event(
                release,
                ModifiersState::empty(),
                KITTY_REPORT_EVENTS,
                KeyboardModes::default(),
            ),
            None,
            "{named:?} release",
        );
    }
}

/// Event reporting alone must make Escape repeats and releases unambiguous.
#[test]
fn kitty_event_reporting_covers_escape_without_report_all() {
    let escape_key = Key::Named(NamedKey::Escape);
    for (state, repeat, expected) in [
        (ElementState::Pressed, false, b"\x1b".as_slice()),
        (ElementState::Pressed, true, b"\x1b[27;1:2u".as_slice()),
        (ElementState::Released, false, b"\x1b[27;1:3u".as_slice()),
    ] {
        let escape = event(
            &escape_key,
            PhysicalKey::Code(KeyCode::Escape),
            None,
            KeyLocation::Standard,
            state,
            repeat,
        );
        assert_eq!(
            encode_event(
                escape,
                ModifiersState::empty(),
                KITTY_REPORT_EVENTS,
                KeyboardModes::default(),
            ),
            Some(expected.to_vec()),
        );
    }
}

/// Event reporting keeps text presses/repeats as UTF-8 but identifies releases.
#[test]
fn kitty_event_reporting_preserves_text_compatibility() {
    let a_key = Key::Character("a".into());
    for (state, repeat, text, expected) in [
        (ElementState::Pressed, false, Some("a"), b"a".as_slice()),
        (ElementState::Pressed, true, Some("a"), b"a".as_slice()),
        (ElementState::Released, false, None, b"\x1b[97;1:3u".as_slice()),
    ] {
        let a_event = event(
            &a_key,
            PhysicalKey::Code(KeyCode::KeyA),
            text,
            KeyLocation::Standard,
            state,
            repeat,
        );
        assert_eq!(
            encode_event(
                a_event,
                ModifiersState::empty(),
                KITTY_REPORT_EVENTS,
                KeyboardModes::default(),
            ),
            Some(expected.to_vec()),
        );
    }
}

/// Legacy encoding preserves consumed AltGr text but never drops Control from
/// a non-ASCII command chord that has no C0 representation.
#[test]
fn legacy_layout_text_separates_altgr_from_control_chords() {
    let at_key = Key::Character("@".into());
    let mut altgr_at = event(
        &at_key,
        PhysicalKey::Code(KeyCode::KeyQ),
        Some("@"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    altgr_at.unmodified_character = Some('q');
    assert_eq!(
        encode_event(
            altgr_at,
            ModifiersState::CONTROL | ModifiersState::ALT,
            0,
            KeyboardModes::default(),
        ),
        Some(b"@".to_vec()),
    );

    let cyrillic_key = Key::Character("с".into());
    let cyrillic_control = event(
        &cyrillic_key,
        PhysicalKey::Code(KeyCode::KeyC),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(cyrillic_control, ModifiersState::CONTROL, 0, KeyboardModes::default(),),
        Some(b"\x1b[1089;5u".to_vec()),
    );

    let dead_key = Key::Dead(Some('^'));
    let dead = event(
        &dead_key,
        PhysicalKey::Code(KeyCode::Backquote),
        Some("^"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(dead, ModifiersState::empty(), 0, KeyboardModes::default()),
        Some(b"^".to_vec()),
    );
    assert_eq!(encode_event(dead, ModifiersState::SUPER, 0, KeyboardModes::default()), None,);
}

/// Disambiguation must not turn an unmodified text-producing Space into a key escape.
#[test]
fn kitty_disambiguation_keeps_plain_space_as_text() {
    assert_eq!(
        encode_logical(
            &Key::Named(NamedKey::Space),
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE,
            false,
        ),
        Some(b" ".to_vec()),
    );
}

/// Shifted punctuation aliases and all declared function keys must reach keymaps.
#[test]
fn keymap_names_cover_shifted_punctuation_and_extended_named_keys() {
    assert_eq!(
        key_to_strings(&Key::Character("{".into()), ModifiersState::SHIFT),
        ["shift+[", "shift+{"],
    );
    assert_eq!(
        key_strings_for_parts(
            &Key::Character("[".into()),
            &Key::Character("{".into()),
            ModifiersState::SHIFT,
        ),
        ["shift+[", "shift+{"],
    );
    assert_eq!(
        key_to_string(&Key::Named(NamedKey::Insert), ModifiersState::empty()).as_deref(),
        Some("insert"),
    );
    assert_eq!(
        key_to_string(&Key::Named(NamedKey::F11), ModifiersState::empty()).as_deref(),
        Some("f11"),
    );
    assert_eq!(
        key_to_string(&Key::Named(NamedKey::F35), ModifiersState::empty()).as_deref(),
        Some("f35"),
    );
}
