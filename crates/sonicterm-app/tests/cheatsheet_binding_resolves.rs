use sonicterm_core::keymap::{Action, Keymap};
use winit::keyboard::{Key, ModifiersState};

#[test]
fn shifted_question_mark_super_binding_opens_cheatsheet() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/keymaps/wezterm.toml");
    let keymap = Keymap::load(&path).expect("load default wezterm keymap");

    let mut mods = ModifiersState::empty();
    mods.set(ModifiersState::SUPER, true);
    mods.set(ModifiersState::SHIFT, true);

    let mut parts = Vec::new();
    if mods.super_key() || mods.control_key() {
        parts.push("super".to_string());
    }
    if mods.alt_key() {
        parts.push("alt".to_string());
    }
    if mods.shift_key() {
        parts.push("shift".to_string());
    }
    parts.push(
        sonicterm_app::app::key_name(&Key::Character("?".into())).unwrap().as_str().to_string(),
    );
    let key_event_string = parts.join("+").to_ascii_lowercase();

    assert_eq!(key_event_string, "super+shift+?");
    assert!(matches!(keymap.lookup(&key_event_string), Some(Action::ShowKeymapCheatsheet)));
}
