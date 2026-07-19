use super::*;

#[test]
fn enter_encodes_carriage_return() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 0, false),
        Some(b"\r".to_vec())
    );
}

#[test]
fn shift_enter_encodes_escape_carriage_return() {
    // Legacy (no kitty flags): Shift+Enter falls back to ESC+CR.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 0, false),
        Some(b"\x1b\r".to_vec())
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
