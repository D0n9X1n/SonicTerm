use super::*;

#[test]
fn enter_encodes_carriage_return() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 0, false),
        Some(b"\r".to_vec())
    );
}

#[test]
fn shift_enter_retains_legacy_carriage_return() {
    // Legacy terminals cannot distinguish Shift+Enter, so modifiers must not
    // invent an escape sequence until an application negotiates one.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 0, false),
        Some(b"\r".to_vec())
    );
}

#[test]
fn shift_enter_kitty_encodes_csi_u() {
    // With kitty keyboard flags active, Shift+Enter is the disambiguated
    // CSI-u form so Copilot CLI / claude insert a newline.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 1, false),
        Some(b"\x1b[13;2u".to_vec())
    );
}

#[test]
fn plain_enter_stays_carriage_return_under_kitty() {
    // Plain Enter must remain CR even when kitty flags are active, so we
    // don't regress "submit" in apps that accept bare CR.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 1, false),
        Some(b"\r".to_vec())
    );
}

#[test]
fn core_control_editing_keys_remain_terminal_control_bytes() {
    for (ch, byte) in [
        ('a', 0x01),
        ('b', 0x02),
        ('d', 0x04),
        ('e', 0x05),
        ('f', 0x06),
        ('h', 0x08),
        ('k', 0x0b),
        ('u', 0x15),
        ('w', 0x17),
    ] {
        assert_eq!(
            encode_logical(
                &Key::Character(ch.to_string().into()),
                ModifiersState::CONTROL,
                0,
                false,
            ),
            Some(vec![byte]),
            "Ctrl+{ch} must retain terminal encoding when no app text field owns input",
        );
    }
}

#[test]
fn alt_character_encodes_legacy_meta_prefix() {
    assert_eq!(
        encode_logical(&Key::Character("v".into()), ModifiersState::ALT, 0, false),
        Some(b"\x1bv".to_vec())
    );
}

// ---: DECCKM (application cursor keys) for arrows + Home/End ---

/// Encode an unmodified named key in the given DECCKM state.
fn ck(named: NamedKey, app_cursor: bool) -> Vec<u8> {
    encode_logical(&Key::Named(named), ModifiersState::empty(), 0, app_cursor)
        .expect("cursor key should encode")
}

#[test]
fn home_end_use_csi_when_decckm_off() {
    // Normal cursor-keys mode: CSI introducer. This is the default and what
    // bare bash/readline accepts.
    assert_eq!(ck(NamedKey::Home, false), b"\x1b[H".to_vec());
    assert_eq!(ck(NamedKey::End, false), b"\x1b[F".to_vec());
}

#[test]
fn home_end_use_ss3_when_decckm_on() {
    // under DECCKM the introducer is SS3 (ESC O), which is what
    // terminfo khome/kend resolve to under smkx — so zsh ZLE / readline /
    // vim / less actually recognize Home and End.
    assert_eq!(ck(NamedKey::Home, true), b"\x1bOH".to_vec());
    assert_eq!(ck(NamedKey::End, true), b"\x1bOF".to_vec());
}

#[test]
fn arrows_track_decckm_introducer() {
    // Arrows follow the same DECCKM rule (locks WezTerm/xterm parity).
    assert_eq!(ck(NamedKey::ArrowUp, false), b"\x1b[A".to_vec());
    assert_eq!(ck(NamedKey::ArrowDown, false), b"\x1b[B".to_vec());
    assert_eq!(ck(NamedKey::ArrowRight, false), b"\x1b[C".to_vec());
    assert_eq!(ck(NamedKey::ArrowLeft, false), b"\x1b[D".to_vec());
    assert_eq!(ck(NamedKey::ArrowUp, true), b"\x1bOA".to_vec());
    assert_eq!(ck(NamedKey::ArrowDown, true), b"\x1bOB".to_vec());
    assert_eq!(ck(NamedKey::ArrowRight, true), b"\x1bOC".to_vec());
    assert_eq!(ck(NamedKey::ArrowLeft, true), b"\x1bOD".to_vec());
}

#[test]
fn ctrl_end_preserves_control_for_terminal_applications() {
    for kitty_flags in [0, 1] {
        assert_eq!(
            encode_logical(&Key::Named(NamedKey::End), ModifiersState::CONTROL, kitty_flags, false,),
            Some(b"\x1b[1;5F".to_vec())
        );
    }
}

#[test]
fn modified_cursor_keys_preserve_modifiers_regardless_of_decckm() {
    let keys = [
        (NamedKey::ArrowUp, 'A'),
        (NamedKey::ArrowDown, 'B'),
        (NamedKey::ArrowRight, 'C'),
        (NamedKey::ArrowLeft, 'D'),
        (NamedKey::Home, 'H'),
        (NamedKey::End, 'F'),
    ];
    let modifiers = [
        (ModifiersState::SHIFT, 2),
        (ModifiersState::ALT, 3),
        (ModifiersState::CONTROL, 5),
        (ModifiersState::SUPER, 9),
        (ModifiersState::SHIFT | ModifiersState::CONTROL, 6),
        (
            ModifiersState::SHIFT
                | ModifiersState::ALT
                | ModifiersState::CONTROL
                | ModifiersState::SUPER,
            16,
        ),
    ];

    for (named, final_char) in keys {
        for (mods, modifier) in modifiers {
            let expected = format!("\x1b[1;{modifier}{final_char}").into_bytes();
            for app_cursor in [false, true] {
                assert_eq!(
                    encode_logical(&Key::Named(named), mods, 0, app_cursor),
                    Some(expected.clone()),
                    "{named:?} with modifier {modifier} and app_cursor={app_cursor}",
                );
            }
        }
    }
}

#[test]
fn modified_tilde_keys_preserve_modifiers() {
    let keys = [(NamedKey::PageUp, 5), (NamedKey::PageDown, 6), (NamedKey::Delete, 3)];
    let modifiers = [
        (ModifiersState::SHIFT, 2),
        (ModifiersState::ALT, 3),
        (ModifiersState::CONTROL, 5),
        (ModifiersState::SUPER, 9),
        (ModifiersState::SHIFT | ModifiersState::CONTROL, 6),
    ];

    for (named, code) in keys {
        assert_eq!(
            encode_logical(&Key::Named(named), ModifiersState::empty(), 0, false),
            Some(format!("\x1b[{code}~").into_bytes()),
        );
        for (mods, modifier) in modifiers {
            assert_eq!(
                encode_logical(&Key::Named(named), mods, 0, false),
                Some(format!("\x1b[{code};{modifier}~").into_bytes()),
                "{named:?} with modifier {modifier}",
            );
        }
    }
}

fn fk(named: NamedKey, mods: ModifiersState) -> Vec<u8> {
    encode_logical(&Key::Named(named), mods, 0, false).expect("function key should encode")
}

#[test]
fn unmodified_function_keys_cover_f1_through_f12() {
    // F1–F4 use the legacy SS3 forms; F5–F12 use the xterm CSI tilde forms
    // (note the historical gaps at codes 16 and 22).
    let none = ModifiersState::empty();
    assert_eq!(fk(NamedKey::F1, none), b"\x1bOP".to_vec());
    assert_eq!(fk(NamedKey::F2, none), b"\x1bOQ".to_vec());
    assert_eq!(fk(NamedKey::F3, none), b"\x1bOR".to_vec());
    assert_eq!(fk(NamedKey::F4, none), b"\x1bOS".to_vec());
    assert_eq!(fk(NamedKey::F5, none), b"\x1b[15~".to_vec());
    assert_eq!(fk(NamedKey::F6, none), b"\x1b[17~".to_vec());
    assert_eq!(fk(NamedKey::F7, none), b"\x1b[18~".to_vec());
    assert_eq!(fk(NamedKey::F8, none), b"\x1b[19~".to_vec());
    assert_eq!(fk(NamedKey::F9, none), b"\x1b[20~".to_vec());
    assert_eq!(fk(NamedKey::F10, none), b"\x1b[21~".to_vec());
    assert_eq!(fk(NamedKey::F11, none), b"\x1b[23~".to_vec());
    assert_eq!(fk(NamedKey::F12, none), b"\x1b[24~".to_vec());
}

#[test]
fn modified_f1_through_f4_use_csi_with_modifier_param() {
    // F1–F4 switch from SS3 to CSI 1 ; <mod> <final> when a modifier is held.
    // Ctrl bit = 4, so modifier param = 1 + 4 = 5.
    assert_eq!(fk(NamedKey::F2, ModifiersState::CONTROL), b"\x1b[1;5Q".to_vec());
    // Shift bit = 1 => param 2.
    assert_eq!(fk(NamedKey::F1, ModifiersState::SHIFT), b"\x1b[1;2P".to_vec());
    // Alt bit = 2 => param 3.
    assert_eq!(fk(NamedKey::F4, ModifiersState::ALT), b"\x1b[1;3S".to_vec());
}

#[test]
fn modified_f5_through_f12_use_csi_tilde_with_modifier_param() {
    // Shift+F5: code 15, modifier param 2.
    assert_eq!(fk(NamedKey::F5, ModifiersState::SHIFT), b"\x1b[15;2~".to_vec());
    // Ctrl+Shift+F12: code 24, bitmask 1|4 = 5 => param 6.
    assert_eq!(
        fk(NamedKey::F12, ModifiersState::CONTROL | ModifiersState::SHIFT),
        b"\x1b[24;6~".to_vec()
    );
    // Super/Meta+F9: code 20, bit 8 => param 9.
    assert_eq!(fk(NamedKey::F9, ModifiersState::SUPER), b"\x1b[20;9~".to_vec());
}

#[test]
fn function_key_modifier_bitmask_combines_all_modifiers() {
    // All four modifiers: 1|2|4|8 = 15 => param 16. Exercises the full mask
    // on an F5–F12 key (F7 = code 18).
    let all = ModifiersState::SHIFT
        | ModifiersState::ALT
        | ModifiersState::CONTROL
        | ModifiersState::SUPER;
    assert_eq!(fk(NamedKey::F7, all), b"\x1b[18;16~".to_vec());
}

fn event<'a>(
    logical_key: &'a Key,
    physical_key: PhysicalKey,
    text: Option<&'a str>,
    location: KeyLocation,
    state: ElementState,
    repeat: bool,
) -> KeyEventView<'a> {
    KeyEventView {
        logical_key,
        unmodified_character: key_character(logical_key).map(unshift_ascii),
        physical_key,
        text,
        text_with_all_modifiers: text,
        location,
        state,
        repeat,
    }
}

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
        Some(b"\x1bOq".to_vec()),
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
    let c = event(
        &c_key,
        PhysicalKey::Code(KeyCode::KeyC),
        Some("c"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(c, ctrl, 0, KeyboardModes::new(false, false, false, false, 1)),
        Some(vec![0x03]),
    );
    assert_eq!(
        encode_event(c, ctrl, 0, KeyboardModes::new(false, false, false, false, 2)),
        Some(b"\x1b[27;5;99~".to_vec()),
    );

    let a_key = Key::Character("a".into());
    let a = event(
        &a_key,
        PhysicalKey::Code(KeyCode::KeyA),
        Some("a"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    assert_eq!(
        encode_event(a, ctrl, 0, KeyboardModes::new(false, false, false, false, 1)),
        Some(vec![0x01]),
    );
    assert_eq!(
        encode_event(a, ModifiersState::ALT, 0, KeyboardModes::new(false, false, false, false, 1)),
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

/// Form-only or unknown Kitty flags preserve the underlying DECKPAM encoding.
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
            Some(b"\x1bOq".to_vec()),
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
    assert_eq!(fk(NamedKey::F13, ModifiersState::empty()), b"\x1b[25~".to_vec());
    assert_eq!(fk(NamedKey::F24, ModifiersState::empty()), b"\x1b[45~".to_vec());
    assert_eq!(fk(NamedKey::F25, ModifiersState::empty()), b"\x1b[57388u".to_vec());
    assert_eq!(fk(NamedKey::F35, ModifiersState::empty()), b"\x1b[57398u".to_vec());
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
    assert_eq!(fk(NamedKey::F3, ModifiersState::empty()), b"\x1bOR".to_vec());
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
        let a = event(
            &a_key,
            PhysicalKey::Code(KeyCode::KeyA),
            text,
            KeyLocation::Standard,
            state,
            repeat,
        );
        assert_eq!(
            encode_event(a, ModifiersState::empty(), KITTY_REPORT_EVENTS, KeyboardModes::default(),),
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
