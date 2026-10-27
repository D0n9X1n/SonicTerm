//! Kitty negotiated input and key-event contracts.

use super::super::key_encoding_tests::*;
use super::*;

/// Tab variants must remain distinguishable in both legacy and Kitty modes.
#[test]
fn backtab_encodes_legacy_and_kitty_forms() {
    let tab = Key::Named(NamedKey::Tab);
    assert_eq!(encode_logical(&tab, ModifiersState::SHIFT, 0, false), Some(b"\x1b[Z".to_vec()),);
    assert_eq!(
        encode_logical(&tab, ModifiersState::SHIFT, KITTY_DISAMBIGUATE, false),
        Some(b"\x1b[9;2u".to_vec()),
    );
    assert_eq!(
        encode_logical(
            &tab,
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            KITTY_DISAMBIGUATE,
            false,
        ),
        Some(b"\x1b[9;6u".to_vec()),
    );
    assert_eq!(
        encode_logical(&tab, ModifiersState::SHIFT | ModifiersState::CONTROL, 0, false,),
        Some(b"\x1b[Z".to_vec()),
    );
}

/// Legacy Control aliases and an Alt prefix must survive the same key chord.
#[test]
fn legacy_control_aliases_and_alt_prefix_are_complete() {
    for (text, expected) in [
        (" ", 0),
        ("@", 0),
        ("2", 0),
        ("[", 27),
        ("3", 27),
        ("\\", 28),
        ("4", 28),
        ("]", 29),
        ("5", 29),
        ("^", 30),
        ("6", 30),
        ("_", 31),
        ("7", 31),
        ("?", 127),
        ("8", 127),
    ] {
        assert_eq!(
            encode_logical(&Key::Character(text.into()), ModifiersState::CONTROL, 0, false,),
            Some(vec![expected]),
            "Ctrl+{text}",
        );
    }
    assert_eq!(
        encode_logical(
            &Key::Character("c".into()),
            ModifiersState::CONTROL | ModifiersState::ALT,
            0,
            false,
        ),
        Some(vec![0x1b, 0x03]),
    );
}

/// Parsed VT modes must select Backspace, newline, keypad, and modifyOtherKeys output.
#[test]
fn terminal_keyboard_modes_change_legacy_encoding() {
    let backspace_key = Key::Named(NamedKey::Backspace);
    let backspace = event(
        &backspace_key,
        PhysicalKey::Code(KeyCode::Backspace),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(backspace, ModifiersState::empty(), 0, KeyboardModes::default()),
        Some(vec![0x7f]),
    );
    assert_eq!(
        encode_event(
            backspace,
            ModifiersState::empty(),
            0,
            KeyboardModes::new(false, false, true, false, 0),
        ),
        Some(vec![0x08]),
    );

    let enter_key = Key::Named(NamedKey::Enter);
    let enter = event(
        &enter_key,
        PhysicalKey::Code(KeyCode::Enter),
        Some("\r"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            enter,
            ModifiersState::empty(),
            0,
            KeyboardModes::new(false, false, false, true, 0),
        ),
        Some(b"\r\n".to_vec()),
    );

    let keypad_one_key = Key::Character("1".into());
    let keypad_one = event(
        &keypad_one_key,
        PhysicalKey::Code(KeyCode::Numpad1),
        Some("1"),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            keypad_one,
            ModifiersState::empty(),
            0,
            KeyboardModes::new(false, true, false, false, 0),
        ),
        Some(b"1".to_vec()),
    );

    let letter_key = Key::Character("a".into());
    let letter = event(
        &letter_key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("a"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            letter,
            ModifiersState::CONTROL,
            0,
            KeyboardModes::new(false, false, false, false, 2),
        ),
        Some(b"\x1b[27;5;97~".to_vec()),
    );
}

/// modifyOtherKeys level 1 must retain Shift/Control compatibility but encode Alt.
#[test]
fn modify_other_keys_levels_have_distinct_compatibility_behavior() {
    let ctrl = ModifiersState::CONTROL;
    let c_key = Key::Character("c".into());
    let c_event = event(
        &c_key,
        PhysicalKey::Code(KeyCode::KeyC),
        Some("c"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(c_event, ctrl, 0, KeyboardModes::new(false, false, false, false, 1)),
        Some(vec![0x03]),
    );
    assert_eq!(
        encode_event(c_event, ctrl, 0, KeyboardModes::new(false, false, false, false, 2)),
        Some(b"\x1b[27;5;99~".to_vec()),
    );

    let a_key = Key::Character("a".into());
    let a_event = event(
        &a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("a"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(a_event, ctrl, 0, KeyboardModes::new(false, false, false, false, 1)),
        Some(vec![0x01]),
    );
    assert_eq!(
        encode_event(
            a_event,
            ModifiersState::ALT,
            0,
            KeyboardModes::new(false, false, false, false, 1)
        ),
        Some(b"\x1b[27;3;97~".to_vec()),
    );

    let semicolon_key = Key::Character(";".into());
    let semicolon = event(
        &semicolon_key,
        PhysicalKey::Code(KeyCode::Semicolon),
        Some(";"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(semicolon, ctrl, 0, KeyboardModes::new(false, false, false, false, 1)),
        Some(b"\x1b[27;5;59~".to_vec()),
    );

    let tab_key = Key::Named(NamedKey::Tab);
    let tab = event(
        &tab_key,
        PhysicalKey::Code(KeyCode::Tab),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    let level_one = KeyboardModes::new(false, false, false, false, 1);
    let level_two = KeyboardModes::new(false, false, false, false, 2);
    assert_eq!(encode_event(tab, ModifiersState::SHIFT, 0, level_one), Some(b"\x1b[Z".to_vec()),);
    assert_eq!(
        encode_event(tab, ModifiersState::ALT, 0, level_one),
        Some(b"\x1b[27;3;9~".to_vec()),
    );
    assert_eq!(
        encode_event(tab, ModifiersState::CONTROL | ModifiersState::SHIFT, 0, level_one,),
        Some(b"\x1b[27;6;9~".to_vec()),
    );
    assert_eq!(
        encode_event(tab, ModifiersState::SHIFT, 0, level_two),
        Some(b"\x1b[27;2;9~".to_vec()),
    );

    let backspace_key = Key::Named(NamedKey::Backspace);
    let backspace = event(
        &backspace_key,
        PhysicalKey::Code(KeyCode::Backspace),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(backspace, ModifiersState::ALT, 0, level_one),
        Some(b"\x1b\x7f".to_vec()),
    );
    assert_eq!(
        encode_event(backspace, ModifiersState::ALT, 0, level_two),
        Some(b"\x1b[27;3;127~".to_vec()),
    );

    let escape_key = Key::Named(NamedKey::Escape);
    let escape = event(
        &escape_key,
        PhysicalKey::Code(KeyCode::Escape),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(escape, ModifiersState::ALT, 0, level_one),
        Some(b"\x1b[27;3;27~".to_vec()),
    );
    assert_eq!(
        encode_event(escape, ModifiersState::ALT, 0, level_two),
        Some(b"\x1b[27;3;27~".to_vec()),
    );

    let enter_key = Key::Named(NamedKey::Enter);
    let enter = event(
        &enter_key,
        PhysicalKey::Code(KeyCode::Enter),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(enter, ModifiersState::ALT, 0, level_one),
        Some(b"\x1b[27;3;13~".to_vec()),
    );
}

/// modifyOtherKeys level 1 must preserve Tab compatibility while level 2 encodes modifiers.
#[test]
fn modify_other_keys_preserves_tab_compatibility_at_level_one() {
    let tab_key = Key::Named(NamedKey::Tab);
    let tab = event(
        &tab_key,
        PhysicalKey::Code(KeyCode::Tab),
        Some("\t"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );

    for (level, modifiers, expected) in [
        (0, ModifiersState::empty(), b"\t".as_slice()),
        (1, ModifiersState::empty(), b"\t".as_slice()),
        (2, ModifiersState::empty(), b"\t".as_slice()),
        (0, ModifiersState::SHIFT, b"\x1b[Z".as_slice()),
        (1, ModifiersState::SHIFT, b"\x1b[Z".as_slice()),
        (2, ModifiersState::SHIFT, b"\x1b[27;2;9~".as_slice()),
        (0, ModifiersState::CONTROL, b"\t".as_slice()),
        (1, ModifiersState::CONTROL, b"\x1b[27;5;9~".as_slice()),
        (2, ModifiersState::CONTROL, b"\x1b[27;5;9~".as_slice()),
    ] {
        assert_eq!(
            encode_event(tab, modifiers, 0, KeyboardModes::new(false, false, false, false, level),),
            Some(expected.to_vec()),
            "modifyOtherKeys={level}, modifiers={modifiers:?}",
        );
    }
}

/// DECKPAM uses the physical keypad key, while normal mode follows NumLock's logical key.
#[test]
fn application_keypad_preserves_physical_digit_identity() {
    let down_key = Key::Named(NamedKey::ArrowDown);
    let numpad_two = event(
        &down_key,
        PhysicalKey::Code(KeyCode::Numpad2),
        None,
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(numpad_two, ModifiersState::empty(), 0, KeyboardModes::default()),
        Some(b"\x1b[B".to_vec()),
    );
    assert_eq!(
        encode_event(
            numpad_two,
            ModifiersState::empty(),
            0,
            KeyboardModes::new(false, true, false, false, 0),
        ),
        Some(b"\x1bOr".to_vec()),
    );
}

// Form-only and unknown Kitty flags preserve OS-resolved numeric keypad text.
#[test]
fn optional_kitty_fields_do_not_enable_kitty_key_encoding() {
    let one_key = Key::Character("1".into());
    let numpad_one = event(
        &one_key,
        PhysicalKey::Code(KeyCode::Numpad1),
        Some("1"),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    let modes = KeyboardModes::new(false, true, false, false, 0);

    for flags in [KITTY_REPORT_ALTERNATES, KITTY_REPORT_TEXT, 1 << 7] {
        assert_eq!(
            encode_event(numpad_one, ModifiersState::empty(), flags, modes),
            Some(b"1".to_vec()),
            "flags={flags}",
        );
    }
}

/// Text-producing keypad keys must preserve modifiers when DECKPAM is disabled.
#[test]
fn normal_keypad_text_preserves_modifiers() {
    let add_key = Key::Character("+".into());
    let add = event(
        &add_key,
        PhysicalKey::Code(KeyCode::NumpadAdd),
        Some("+"),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(add, ModifiersState::ALT, 0, KeyboardModes::default()),
        Some(b"\x1b+".to_vec()),
    );
    assert_eq!(
        encode_event(add, ModifiersState::SUPER, 0, KeyboardModes::default()),
        Some(b"\x1b[43;9u".to_vec()),
    );
}

/// Extended function-key identity must cover the full winit F1–F35 range.
#[test]
fn extended_function_keys_have_legacy_or_csi_u_encodings() {
    assert_eq!(function_key(NamedKey::F13, ModifiersState::empty()), b"\x1b[25~".to_vec());
    assert_eq!(function_key(NamedKey::F24, ModifiersState::empty()), b"\x1b[45~".to_vec());
    assert_eq!(function_key(NamedKey::F25, ModifiersState::empty()), b"\x1b[57388u".to_vec());
    assert_eq!(function_key(NamedKey::F35, ModifiersState::empty()), b"\x1b[57398u".to_vec());
}

/// Enhanced Kitty function keys use the canonical non-conflicting forms.
#[test]
fn kitty_function_keys_follow_the_protocol_table() {
    for (key, expected) in [
        (NamedKey::F1, b"\x1b[P".as_slice()),
        (NamedKey::F3, b"\x1b[13~".as_slice()),
        (NamedKey::F13, b"\x1b[57376u".as_slice()),
        (NamedKey::F24, b"\x1b[57387u".as_slice()),
        (NamedKey::F35, b"\x1b[57398u".as_slice()),
    ] {
        assert_eq!(
            encode_logical(&Key::Named(key), ModifiersState::empty(), KITTY_DISAMBIGUATE, false,),
            Some(expected.to_vec()),
            "{key:?}",
        );
    }
    assert_eq!(function_key(NamedKey::F3, ModifiersState::empty()), b"\x1bOR".to_vec());
}

/// Layout-aware unmodified keys drive Kitty primary codes; PC-101 positions
/// appear only in the optional base-layout subfield.
#[test]
fn kitty_primary_and_alternate_codes_keep_layout_roles_distinct() {
    let shifted = Key::Character("(".into());
    let mut german_eight = event(
        &shifted,
        PhysicalKey::Code(KeyCode::Digit8),
        Some("("),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    german_eight.unmodified_character = Some('8');
    assert_eq!(
        encode_event(
            german_eight,
            ModifiersState::SHIFT,
            KITTY_REPORT_ALTERNATES | KITTY_REPORT_ALL | KITTY_REPORT_TEXT,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[56:40;2;40u".to_vec()),
    );

    let cyrillic = Key::Character("с".into());
    let mut cyrillic_c = event(
        &cyrillic,
        PhysicalKey::Code(KeyCode::KeyC),
        Some("с"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    cyrillic_c.unmodified_character = Some('с');
    assert_eq!(
        encode_event(
            cyrillic_c,
            ModifiersState::CONTROL,
            KITTY_DISAMBIGUATE | KITTY_REPORT_ALTERNATES,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[1089::99;5u".to_vec()),
    );

    let shifted_a_key = Key::Character("A".into());
    let shifted_a = event(
        &shifted_a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        None,
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            shifted_a,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            KITTY_REPORT_ALTERNATES,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[97:65;6u".to_vec()),
    );
}

/// Legacy C0 keys retain the established compatibility table until a client
/// negotiates Kitty disambiguation or xterm modifyOtherKeys.
#[test]
fn legacy_c0_modifier_table_does_not_invent_csi_u_sequences() {
    for (key, mods, expected) in [
        (NamedKey::Enter, ModifiersState::CONTROL, b"\r".as_slice()),
        (NamedKey::Escape, ModifiersState::SHIFT, b"\x1b".as_slice()),
        (NamedKey::Backspace, ModifiersState::SHIFT, b"\x7f".as_slice()),
        (NamedKey::Tab, ModifiersState::CONTROL, b"\t".as_slice()),
        (NamedKey::Tab, ModifiersState::ALT | ModifiersState::SHIFT, b"\x1b\x1b[Z".as_slice()),
    ] {
        assert_eq!(encode_logical(&Key::Named(key), mods, 0, false), Some(expected.to_vec()));
    }
    assert_eq!(
        encode_logical(
            &Key::Named(NamedKey::Space),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            0,
            false,
        ),
        Some(vec![0]),
    );
}

/// Kitty event, alternate-key, text, and keypad fields must use protocol-defined forms.
#[test]
fn kitty_progressive_flags_encode_complete_event_data() {
    let shifted_a_key = Key::Character("A".into());
    let shifted_a = event(
        &shifted_a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("A"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            shifted_a,
            ModifiersState::SHIFT,
            KITTY_REPORT_ALTERNATES | KITTY_REPORT_ALL | KITTY_REPORT_TEXT,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[97:65;2;65u".to_vec()),
    );

    let plain_a_key = Key::Character("a".into());
    let plain_a = event(
        &plain_a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("a"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            plain_a,
            ModifiersState::empty(),
            KITTY_REPORT_ALL | KITTY_REPORT_TEXT,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[97;;97u".to_vec()),
    );

    let repeated_a_key = Key::Character("a".into());
    let repeated_a = event(
        &repeated_a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("a"),
        KeyLocation::Standard,
        ElementState::Pressed,
        true,
    );
    assert_eq!(
        encode_event(
            repeated_a,
            ModifiersState::empty(),
            KITTY_REPORT_EVENTS | KITTY_REPORT_ALL,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[97;1:2u".to_vec()),
    );

    let released_a_key = Key::Character("a".into());
    let released_a = event(
        &released_a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        None,
        KeyLocation::Standard,
        ElementState::Released,
        false,
    );
    assert_eq!(
        encode_event(
            released_a,
            ModifiersState::empty(),
            KITTY_REPORT_EVENTS | KITTY_REPORT_ALL,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[97;1:3u".to_vec()),
    );
    assert_eq!(
        encode_event(
            released_a,
            ModifiersState::empty(),
            KITTY_REPORT_EVENTS,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[97;1:3u".to_vec()),
    );

    let keypad_one_key = Key::Character("1".into());
    let keypad_one = event(
        &keypad_one_key,
        PhysicalKey::Code(KeyCode::Numpad1),
        Some("1"),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            keypad_one,
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE,
            KeyboardModes::default(),
        ),
        Some(b"1".to_vec()),
    );
    assert_eq!(
        encode_event(
            keypad_one,
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE | KITTY_REPORT_ALL,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[57400u".to_vec()),
    );

    let keypad_end_key = Key::Named(NamedKey::End);
    let keypad_end = event(
        &keypad_end_key,
        PhysicalKey::Code(KeyCode::Numpad1),
        None,
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            keypad_end,
            ModifiersState::empty(),
            KITTY_DISAMBIGUATE,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[57424u".to_vec()),
    );

    let shift_key = Key::Named(NamedKey::Shift);
    let right_shift = event(
        &shift_key,
        PhysicalKey::Code(KeyCode::ShiftRight),
        None,
        KeyLocation::Right,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(
            right_shift,
            ModifiersState::SHIFT,
            KITTY_REPORT_ALL,
            KeyboardModes::default(),
        ),
        Some(b"\x1b[57447;2u".to_vec()),
    );
    assert_eq!(
        encode_event(right_shift, ModifiersState::SHIFT, 0, KeyboardModes::default(),),
        None,
    );
}
