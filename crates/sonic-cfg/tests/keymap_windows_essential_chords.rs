use sonic_cfg::keymap::{Action, Keymap};

fn windows_keymap() -> Keymap {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/keymaps/wezterm-windows.toml");
    Keymap::load(&path).expect("load bundled Windows keymap")
}

#[test]
fn windows_essential_chords_are_bound() {
    let keymap = windows_keymap();

    assert_eq!(keymap.lookup("ctrl+t"), Some(&Action::NewTab));
    assert_eq!(keymap.lookup("ctrl+shift+p"), Some(&Action::OpenCommandPalette));
    assert_eq!(keymap.lookup("ctrl+shift+/"), Some(&Action::ShowKeymapCheatsheet));
    assert_eq!(keymap.lookup("ctrl+shift+d"), Some(&Action::SplitRight));
    assert_eq!(keymap.lookup("ctrl+shift+w"), Some(&Action::CloseTab));
}
