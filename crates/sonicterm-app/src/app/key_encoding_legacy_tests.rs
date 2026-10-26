//! Legacy byte and control-modifier keyboard contracts.

use super::super::key_encoding_tests::*;
use super::*;

#[test]
fn standalone_modifier_classification_excludes_locks_and_input_keys() {
    // Selection preservation covers standalone modifiers, not lock toggles or ordinary input.
    for key in [
        NamedKey::Shift,
        NamedKey::Control,
        NamedKey::Alt,
        NamedKey::Super,
        NamedKey::Hyper,
        NamedKey::Meta,
        NamedKey::AltGraph,
    ] {
        assert!(is_modifier_key(key), "{key:?} must preserve selection");
    }
    for key in [
        NamedKey::CapsLock,
        NamedKey::NumLock,
        NamedKey::ScrollLock,
        NamedKey::FnLock,
        NamedKey::Enter,
        NamedKey::Tab,
        NamedKey::ArrowLeft,
    ] {
        assert!(!is_modifier_key(key), "{key:?} is not a standalone modifier");
    }
}

#[test]
fn win32_shift_return_preserves_native_fields() {
    // Shift+Return retains native identity rather than collapsing into legacy CR.
    let key = Win32KeyEvent {
        virtual_key: 13,
        scan_code: 28,
        unicode: &[13],
        key_down: true,
        control_key_state: 16,
        repeat_count: 1,
    };
    assert_eq!(encode_win32_key(key), b"\x1b[13;28;13;1;16;1_");
}

#[test]
fn win32_release_without_text_emits_no_character_record() {
    // WM_KEYUP has no character payload; release encoding must not replay press text.
    let key = Win32KeyEvent {
        virtual_key: 65,
        scan_code: 30,
        unicode: &[],
        key_down: false,
        control_key_state: 0,
        repeat_count: 1,
    };
    assert_eq!(encode_win32_key(key), b"\x1b[65;30;0;0;0;1_");
}

#[test]
fn win32_side_specific_modifiers_and_lock_bits_survive_encoding() {
    // AltGr and lock bits are native console flags, not aggregate winit modifiers.
    let key = Win32KeyEvent {
        virtual_key: 81,
        scan_code: 16,
        unicode: &[64],
        key_down: true,
        control_key_state: 1 | 8 | 32 | 64 | 128,
        repeat_count: 1,
    };
    assert_eq!(encode_win32_key(key), b"\x1b[81;16;64;1;233;1_");
}

#[test]
fn win32_native_repeat_and_extended_state_survive_encoding() {
    // Native repeat counts and enhanced-key flags are not reconstructed from winit booleans.
    let key = Win32KeyEvent {
        virtual_key: 38,
        scan_code: 72,
        unicode: &[],
        key_down: true,
        control_key_state: 256,
        repeat_count: 7,
    };
    assert_eq!(encode_win32_key(key), b"\x1b[38;72;0;1;256;7_");
}

#[test]
fn win32_multi_unit_text_keeps_native_repeat_count_on_each_record() {
    // A repeated surrogate pair remains two native records, each carrying the original repeat count.
    let key = Win32KeyEvent {
        virtual_key: 0,
        scan_code: 0,
        unicode: &[0xd83d, 0xde80],
        key_down: true,
        control_key_state: 0,
        repeat_count: 7,
    };
    assert_eq!(encode_win32_key(key), b"\x1b[0;0;55357;1;0;7_\x1b[0;0;56960;1;0;7_");
}

#[test]
fn win32_raw_utf16_units_are_not_normalized_through_utf8() {
    // Each UTF-16 unit remains a record, including unmatched surrogates supplied by the OS.
    let key = Win32KeyEvent {
        virtual_key: 0,
        scan_code: 0,
        unicode: &[0xd83d, 0xde80, 0xd800],
        key_down: true,
        control_key_state: 0,
        repeat_count: 1,
    };
    assert_eq!(
        encode_win32_key(key),
        b"\x1b[0;0;55357;1;0;1_\x1b[0;0;56960;1;0;1_\x1b[0;0;55296;1;0;1_"
    );
}

#[test]
fn numeric_keypad_snapshot_retains_win32_request() {
    // A numeric preference only clears DECKPAM, never the independently negotiated native protocol.
    let modes = KeyboardModes::new(true, true, true, true, 2).with_win32_input(true);
    let numeric = keypad_modes(modes, KeypadMode::Numeric);
    assert!(numeric.win32_input());
    assert!(!numeric.application_keypad());
    assert!(numeric.application_cursor_keys());
    assert!(numeric.backarrow_key());
    assert!(numeric.newline());
    assert_eq!(numeric.modify_other_keys(), 2);
}

// Numeric OS key meaning survives ZLE's application-keypad request without bypassing modifier encoding.
#[test]
fn application_keypad_keeps_os_numeric_digits_as_text() {
    let modes = KeyboardModes::new(false, true, false, false, 0);
    for (number, physical) in [
        KeyCode::Numpad0,
        KeyCode::Numpad1,
        KeyCode::Numpad2,
        KeyCode::Numpad3,
        KeyCode::Numpad4,
        KeyCode::Numpad5,
        KeyCode::Numpad6,
        KeyCode::Numpad7,
        KeyCode::Numpad8,
        KeyCode::Numpad9,
    ]
    .into_iter()
    .enumerate()
    {
        let text = number.to_string();
        let logical = Key::Character(text.clone().into());
        let key = event(
            &logical,
            PhysicalKey::Code(physical),
            Some(&text),
            KeyLocation::Numpad,
            ElementState::Pressed,
            false,
        );
        for flags in [0, KITTY_REPORT_ALTERNATES, KITTY_REPORT_TEXT] {
            assert_eq!(
                encode_event(key, ModifiersState::empty(), flags, modes),
                Some(text.as_bytes().to_vec())
            );
        }
        assert_eq!(
            encode_event(key, ModifiersState::ALT, 0, modes),
            Some(format!("\x1b{text}").into_bytes())
        );
    }
}

#[test]
fn numeric_keypad_preference_is_opt_in_and_preserves_auto_bytes() {
    // Numeric preference overrides only DECKPAM; auto retains deliberate application-keypad mappings.
    for (physical, text, application) in [
        (KeyCode::NumpadDivide, "/", "\x1bOo"),
        (KeyCode::NumpadMultiply, "*", "\x1bOj"),
        (KeyCode::NumpadSubtract, "-", "\x1bOm"),
        (KeyCode::NumpadAdd, "+", "\x1bOk"),
        (KeyCode::NumpadDecimal, ".", "\x1bOn"),
        (KeyCode::NumpadEqual, "=", "\x1bOX"),
        (KeyCode::NumpadComma, ",", "\x1bOl"),
        (KeyCode::Numpad3, "3", "3"),
    ] {
        let logical = Key::Character(text.into());
        let mut key = event(
            &logical,
            PhysicalKey::Code(physical),
            Some(text),
            KeyLocation::Numpad,
            ElementState::Pressed,
            false,
        );
        key.unmodified_character = text.chars().next();
        for app_keypad in [false, true] {
            let modes = KeyboardModes::new(false, app_keypad, false, false, 0);
            for flags in [0, KITTY_REPORT_ALTERNATES, KITTY_REPORT_TEXT] {
                let auto = if app_keypad { application } else { text };
                assert_eq!(
                    encode_event_with_keypad_mode(
                        key,
                        ModifiersState::empty(),
                        flags,
                        modes,
                        KeypadMode::Auto
                    ),
                    Some(auto.as_bytes().to_vec())
                );
                assert_eq!(
                    encode_event_with_keypad_mode(
                        key,
                        ModifiersState::empty(),
                        flags,
                        modes,
                        KeypadMode::Numeric
                    ),
                    Some(text.as_bytes().to_vec()),
                    "physical={physical:?}, flags={flags}"
                );
            }
            for state in [ElementState::Pressed, ElementState::Released] {
                key.state = state;
                key.repeat = state == ElementState::Pressed;
                for flags in [
                    KITTY_DISAMBIGUATE,
                    KITTY_REPORT_EVENTS,
                    KITTY_REPORT_ALL,
                    KITTY_REPORT_ALL | KITTY_REPORT_EVENTS | KITTY_REPORT_TEXT,
                ] {
                    assert_eq!(
                        encode_event_with_keypad_mode(
                            key,
                            ModifiersState::empty(),
                            flags,
                            modes,
                            KeypadMode::Numeric
                        ),
                        encode_event_with_keypad_mode(
                            key,
                            ModifiersState::empty(),
                            flags,
                            modes,
                            KeypadMode::Auto
                        ),
                        "Kitty physical={physical:?}, flags={flags}, state={state:?}",
                    );
                }
            }
            key.state = ElementState::Pressed;
            key.repeat = false;
        }
    }
}

#[test]
fn numeric_keypad_enter_keeps_return_modes_and_kitty_identity() {
    // Numpad Enter uses Return rules in numeric mode, not Cocoa's native U+0003 text.
    let logical = Key::Named(NamedKey::Enter);
    let mut key = event(
        &logical,
        PhysicalKey::Code(KeyCode::NumpadEnter),
        Some("\r"),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    key.text_with_all_modifiers = Some("\u{3}");
    for (mods, newline, mok, expected) in [
        (ModifiersState::empty(), false, 0, b"\r".as_slice()),
        (ModifiersState::empty(), true, 0, b"\r\n".as_slice()),
        (ModifiersState::ALT, true, 0, b"\x1b\r\n".as_slice()),
        (ModifiersState::SHIFT, false, 2, b"\x1b[27;2;13~".as_slice()),
        (ModifiersState::CONTROL, false, 1, b"\x1b[27;5;13~".as_slice()),
    ] {
        let modes = KeyboardModes::new(true, true, true, newline, mok);
        assert_eq!(
            encode_event_with_keypad_mode(key, mods, 0, modes, KeypadMode::Numeric),
            Some(expected.to_vec())
        );
        assert!(modes.application_keypad(), "the parser snapshot is unchanged");
    }
    let modes = KeyboardModes::new(false, true, false, false, 0);
    assert_eq!(
        encode_event_with_keypad_mode(key, ModifiersState::empty(), 0, modes, KeypadMode::Auto),
        Some(b"\x1bOM".to_vec())
    );
    for state in [ElementState::Pressed, ElementState::Released] {
        key.state = state;
        for flags in [KITTY_DISAMBIGUATE, KITTY_REPORT_ALL, KITTY_REPORT_EVENTS | KITTY_REPORT_ALL]
        {
            assert_eq!(
                encode_event_with_keypad_mode(
                    key,
                    ModifiersState::SHIFT,
                    flags,
                    modes,
                    KeypadMode::Numeric
                ),
                encode_event_with_keypad_mode(
                    key,
                    ModifiersState::SHIFT,
                    flags,
                    modes,
                    KeypadMode::Auto
                )
            );
        }
    }
    assert_eq!(
        encode_event_with_keypad_mode(key, ModifiersState::empty(), 0, modes, KeypadMode::Numeric),
        None
    );
}

#[test]
fn numeric_keypad_preference_preserves_other_modes_and_modifiers() {
    // Keypad policy must not change layout text, cursor modes, or a recipient's modified-key protocol.
    let logical = Key::Named(NamedKey::ArrowDown);
    let key = event(
        &logical,
        PhysicalKey::Code(KeyCode::Numpad2),
        None,
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    let modes = KeyboardModes::new(true, true, true, true, 2);
    assert_eq!(
        encode_event_with_keypad_mode(key, ModifiersState::empty(), 0, modes, KeypadMode::Auto),
        Some(b"\x1bOr".to_vec())
    );
    assert_eq!(
        encode_event_with_keypad_mode(key, ModifiersState::empty(), 0, modes, KeypadMode::Numeric),
        Some(b"\x1bOB".to_vec())
    );
    let add = Key::Character("+".into());
    let mut key = event(
        &add,
        PhysicalKey::Code(KeyCode::NumpadAdd),
        Some("+"),
        KeyLocation::Numpad,
        ElementState::Pressed,
        false,
    );
    key.unmodified_character = Some('+');
    for (mods, mok, expected) in [
        (ModifiersState::ALT, 0, "\x1b+"),
        (ModifiersState::SUPER, 0, "\x1b[43;9u"),
        (ModifiersState::ALT, 2, "\x1b[27;3;43~"),
    ] {
        assert_eq!(
            encode_event_with_keypad_mode(
                key,
                mods,
                0,
                KeyboardModes::new(false, true, false, false, mok),
                KeypadMode::Numeric
            ),
            Some(expected.as_bytes().to_vec())
        );
    }
}

#[test]
fn modify_other_keys_uses_layout_character_and_xterm_shift_rules() {
    // MOK reports layout-selected characters; Shift-only symbols and control-producing keys differ.
    for (base, shifted, physical, level_two) in [
        ('1', '!', KeyCode::Digit1, "!"),
        ('2', '@', KeyCode::Digit2, "@"),
        ('3', '#', KeyCode::Digit3, "#"),
        ('6', '^', KeyCode::Digit6, "^"),
        ('=', '+', KeyCode::Equal, "+"),
        ('/', '?', KeyCode::Slash, "?"),
        ('[', '{', KeyCode::BracketLeft, "\x1b[27;2;123~"),
        ('\\', '|', KeyCode::Backslash, "\x1b[27;2;124~"),
        ('`', '~', KeyCode::Backquote, "\x1b[27;2;126~"),
        ('a', 'A', KeyCode::KeyA, "\x1b[27;2;65~"),
        (' ', ' ', KeyCode::Space, "\x1b[27;2;32~"),
        ('8', '(', KeyCode::Digit8, "("),
        ('é', '2', KeyCode::Digit2, "2"),
        ('é', 'é', KeyCode::Digit2, "é"),
        ('ä', 'Ä', KeyCode::Quote, "Ä"),
    ] {
        let text = shifted.to_string();
        let logical = Key::Character(text.clone().into());
        let mut key = event(
            &logical,
            PhysicalKey::Code(physical),
            Some(&text),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        key.unmodified_character = Some(base);
        for level in 0..=2 {
            let expected = if level == 2 { level_two } else { &text };
            assert_eq!(
                encode_event(
                    key,
                    ModifiersState::SHIFT,
                    0,
                    KeyboardModes::new(false, false, false, false, level)
                ),
                Some(expected.as_bytes().to_vec()),
                "base={base:?}, shifted={shifted:?}, level={level}",
            );
        }
    }
}

#[test]
fn modify_other_keys_shifted_chords_keep_layout_codes_and_control_aliases() {
    // Symbol identity is not reconstructible from an unshifted digit and Shift alone.
    for (base, shifted, level_one) in [
        ('3', '#', b"\x1b[27;6;35~".as_slice()),
        ('=', '+', b"\x1b[27;6;43~".as_slice()),
        ('/', '?', b"\x1b[27;6;63~".as_slice()),
        ('2', '@', b"\0".as_slice()),
        ('6', '^', b"\x1e".as_slice()),
        ('[', '{', b"\x1b".as_slice()),
        ('\\', '|', b"\x1c".as_slice()),
        ('`', '~', b"\x1e".as_slice()),
        ('a', 'A', b"\x01".as_slice()),
    ] {
        let text = shifted.to_string();
        let logical = Key::Character(text.clone().into());
        let mut key = event(
            &logical,
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            Some(&text),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        key.unmodified_character = Some(base);
        key.text_with_all_modifiers = Some("\0");
        let modifiers = ModifiersState::SHIFT | ModifiersState::CONTROL;
        assert_eq!(
            encode_event(key, modifiers, 0, KeyboardModes::new(false, false, false, false, 1)),
            Some(level_one.to_vec()),
            "Ctrl+Shift+{base}",
        );
        assert_eq!(
            encode_event(key, modifiers, 0, KeyboardModes::new(false, false, false, false, 2)),
            Some(format!("\x1b[27;6;{}~", u32::from(shifted)).into_bytes()),
            "Ctrl+Shift+{base}",
        );
        key.text_with_all_modifiers = Some(&text);
        for level in [1, 2] {
            assert_eq!(
                encode_event(
                    key,
                    ModifiersState::SHIFT | ModifiersState::ALT,
                    0,
                    KeyboardModes::new(false, false, false, false, level)
                ),
                Some(format!("\x1b[27;4;{}~", u32::from(shifted)).into_bytes()),
                "Alt+Shift+{base}, level={level}",
            );
        }
    }
}

#[test]
fn modify_other_keys_does_not_split_composed_text_or_replay_releases() {
    // A composed string is text, not one modified-key codepoint; Kitty keeps its own key identities.
    for text in ["SS", "e\u{301}"] {
        let logical = Key::Character(text.into());
        let mut key = event(
            &logical,
            PhysicalKey::Code(KeyCode::KeyE),
            Some(text),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        );
        key.unmodified_character = Some('e');
        let modes = KeyboardModes::new(false, false, false, false, 2);
        assert_eq!(
            encode_event(key, ModifiersState::SHIFT, 0, modes),
            Some(text.as_bytes().to_vec())
        );
        key.state = ElementState::Released;
        assert_eq!(encode_event(key, ModifiersState::SHIFT, 0, modes), None);
    }
    let logical = Key::Character("#".into());
    let key = event(
        &logical,
        PhysicalKey::Code(KeyCode::Digit3),
        Some("#"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    let modes = KeyboardModes::new(false, false, false, false, 2);
    assert_eq!(
        encode_event(key, ModifiersState::SHIFT, KITTY_DISAMBIGUATE, modes),
        Some(b"#".to_vec())
    );
    assert_eq!(
        encode_event(key, ModifiersState::SHIFT, KITTY_REPORT_ALL, modes),
        Some(b"\x1b[51;2u".to_vec())
    );
    let mut option = key;
    option.unmodified_character = Some('q');
    assert_eq!(encode_event(option, ModifiersState::ALT, 0, modes), Some(b"#".to_vec()));
}

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
    for (character, byte) in [
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
                &Key::Character(character.to_string().into()),
                ModifiersState::CONTROL,
                0,
                false,
            ),
            Some(vec![byte]),
            "Ctrl+{character} must retain terminal encoding when no app text field owns input",
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

#[test]
fn home_end_use_csi_when_decckm_off() {
    // Normal cursor-keys mode: CSI introducer. This is the default and what
    // bare bash/readline accepts.
    assert_eq!(cursor_key(NamedKey::Home, false), b"\x1b[H".to_vec());
    assert_eq!(cursor_key(NamedKey::End, false), b"\x1b[F".to_vec());
}

#[test]
fn home_end_use_ss3_when_decckm_on() {
    // under DECCKM the introducer is SS3 (ESC O), which is what
    // terminfo khome/kend resolve to under smkx — so zsh ZLE / readline /
    // vim / less actually recognize Home and End.
    assert_eq!(cursor_key(NamedKey::Home, true), b"\x1bOH".to_vec());
    assert_eq!(cursor_key(NamedKey::End, true), b"\x1bOF".to_vec());
}

#[test]
fn arrows_track_decckm_introducer() {
    // Arrows follow the same DECCKM rule (locks WezTerm/xterm parity).
    assert_eq!(cursor_key(NamedKey::ArrowUp, false), b"\x1b[A".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowDown, false), b"\x1b[B".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowRight, false), b"\x1b[C".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowLeft, false), b"\x1b[D".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowUp, true), b"\x1bOA".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowDown, true), b"\x1bOB".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowRight, true), b"\x1bOC".to_vec());
    assert_eq!(cursor_key(NamedKey::ArrowLeft, true), b"\x1bOD".to_vec());
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

#[test]
fn unmodified_function_keys_cover_f1_through_f12() {
    // F1–F4 use the legacy SS3 forms; F5–F12 use the xterm CSI tilde forms
    // (note the historical gaps at codes 16 and 22).
    let none = ModifiersState::empty();
    assert_eq!(function_key(NamedKey::F1, none), b"\x1bOP".to_vec());
    assert_eq!(function_key(NamedKey::F2, none), b"\x1bOQ".to_vec());
    assert_eq!(function_key(NamedKey::F3, none), b"\x1bOR".to_vec());
    assert_eq!(function_key(NamedKey::F4, none), b"\x1bOS".to_vec());
    assert_eq!(function_key(NamedKey::F5, none), b"\x1b[15~".to_vec());
    assert_eq!(function_key(NamedKey::F6, none), b"\x1b[17~".to_vec());
    assert_eq!(function_key(NamedKey::F7, none), b"\x1b[18~".to_vec());
    assert_eq!(function_key(NamedKey::F8, none), b"\x1b[19~".to_vec());
    assert_eq!(function_key(NamedKey::F9, none), b"\x1b[20~".to_vec());
    assert_eq!(function_key(NamedKey::F10, none), b"\x1b[21~".to_vec());
    assert_eq!(function_key(NamedKey::F11, none), b"\x1b[23~".to_vec());
    assert_eq!(function_key(NamedKey::F12, none), b"\x1b[24~".to_vec());
}

#[test]
fn modified_f1_through_f4_use_csi_with_modifier_param() {
    // F1–F4 switch from SS3 to CSI 1 ; <mod> <final> when a modifier is held.
    // Ctrl bit = 4, so modifier param = 1 + 4 = 5.
    assert_eq!(function_key(NamedKey::F2, ModifiersState::CONTROL), b"\x1b[1;5Q".to_vec());
    // Shift bit = 1 => param 2.
    assert_eq!(function_key(NamedKey::F1, ModifiersState::SHIFT), b"\x1b[1;2P".to_vec());
    // Alt bit = 2 => param 3.
    assert_eq!(function_key(NamedKey::F4, ModifiersState::ALT), b"\x1b[1;3S".to_vec());
}

#[test]
fn modified_f5_through_f12_use_csi_tilde_with_modifier_param() {
    // Shift+F5: code 15, modifier param 2.
    assert_eq!(function_key(NamedKey::F5, ModifiersState::SHIFT), b"\x1b[15;2~".to_vec());
    // Ctrl+Shift+F12: code 24, bitmask 1|4 = 5 => param 6.
    assert_eq!(
        function_key(NamedKey::F12, ModifiersState::CONTROL | ModifiersState::SHIFT),
        b"\x1b[24;6~".to_vec()
    );
    // Super/Meta+F9: code 20, bit 8 => param 9.
    assert_eq!(function_key(NamedKey::F9, ModifiersState::SUPER), b"\x1b[20;9~".to_vec());
}

#[test]
fn function_key_modifier_bitmask_combines_all_modifiers() {
    // All four modifiers: 1|2|4|8 = 15 => param 16. Exercises the full mask
    // on an F5–F12 key (F7 = code 18).
    let all = ModifiersState::SHIFT
        | ModifiersState::ALT
        | ModifiersState::CONTROL
        | ModifiersState::SUPER;
    assert_eq!(function_key(NamedKey::F7, all), b"\x1b[18;16~".to_vec());
}
