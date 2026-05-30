use sonic_app::app::key_to_string;
use winit::keyboard::{Key, ModifiersState, SmolStr};

fn shortcut(key: &str, mods: ModifiersState) -> String {
    key_to_string(&Key::Character(SmolStr::new(key)), mods).unwrap()
}

#[test]
fn ctrl_alone_stays_ctrl_for_pty_control_keys() {
    for key in ["c", "t", "l", "d"] {
        let encoded = shortcut(key, ModifiersState::CONTROL);
        assert_eq!(encoded, format!("ctrl+{key}"));
        assert!(!encoded.starts_with("super+"));
    }
}

#[test]
fn real_super_still_encodes_as_super() {
    assert_eq!(shortcut("t", ModifiersState::SUPER), "super+t");
}

#[test]
fn broadcast_binding_keeps_all_real_modifiers() {
    let mods = ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(shortcut("b", mods), "super+ctrl+shift+b");
}

#[test]
fn windows_ctrl_shift_slash_uses_unshifted_key_name() {
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(shortcut("/", mods), "ctrl+shift+/");
}

#[test]
fn windows_ctrl_shift_letter_prefers_lowercase_unshifted_key_name() {
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(shortcut("P", mods), "ctrl+shift+p");
    assert_eq!(shortcut("D", mods), "ctrl+shift+d");
}
