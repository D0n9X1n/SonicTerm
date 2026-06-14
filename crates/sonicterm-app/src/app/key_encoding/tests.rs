
use super::*;

#[test]
fn enter_encodes_carriage_return() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 0),
        Some(b"\r".to_vec())
    );
}

#[test]
fn shift_enter_encodes_escape_carriage_return() {
    // Legacy (no kitty flags): Shift+Enter falls back to ESC+CR.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 0),
        Some(b"\x1b\r".to_vec())
    );
}

#[test]
fn shift_enter_kitty_encodes_csi_u() {
    // With kitty keyboard flags active, Shift+Enter is the disambiguated
    // CSI-u form so Copilot CLI / claude insert a newline.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 1),
        Some(b"\x1b[13;2u".to_vec())
    );
}

#[test]
fn plain_enter_stays_carriage_return_under_kitty() {
    // Plain Enter must remain CR even when kitty flags are active, so we
    // don't regress "submit" in apps that accept bare CR.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 1),
        Some(b"\r".to_vec())
    );
}

fn fk(named: NamedKey, mods: ModifiersState) -> Vec<u8> {
    encode_logical(&Key::Named(named), mods, 0).expect("function key should encode")
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
