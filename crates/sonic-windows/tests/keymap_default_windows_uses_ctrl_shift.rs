use sonic_core::{
    config::Config,
    keymap::{Action, Keymap},
};

fn windows_default_config_for_test() -> Config {
    Config { keymap: "wezterm-windows".to_string(), ..Config::default() }
}

#[test]
fn keymap_default_windows_uses_ctrl_shift() {
    let cfg = windows_default_config_for_test();
    assert_eq!(cfg.keymap, "wezterm-windows");

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/keymaps/wezterm-windows.toml");
    let keymap = Keymap::load(&path).expect("load Windows default keymap");

    assert!(
        keymap.bindings.iter().all(|binding| !binding.keys.to_ascii_lowercase().contains("super")),
        "Windows keymap must not contain super bindings"
    );
    assert_eq!(keymap.lookup("ctrl+t"), Some(&Action::NewTab));
    assert_eq!(
        keymap.lookup("ctrl+shift+/"),
        Some(&Action::ShowKeymapCheatsheet),
        "VK_OEM_2 + Ctrl + Shift is encoded as ctrl+shift+/; Shift already carries the question mark"
    );
    assert_eq!(keymap.lookup("ctrl+shift+w"), Some(&Action::CloseTab));
    assert!(
        keymap
            .bindings
            .iter()
            .any(|binding| binding.keys.to_ascii_lowercase().starts_with("ctrl+shift+")),
        "Windows keymap should use Ctrl+Shift chords"
    );

    assert!(
        keymap.bindings.iter().all(|binding| {
            let keys = binding.keys.to_ascii_lowercase();
            !keys.contains("shift+shift")
                && !keys.contains("ctrl+ctrl")
                && !keys.contains("alt+alt")
        }),
        "Windows keymap must not contain duplicate modifiers matching (shift|ctrl|alt)+\\1"
    );
}
