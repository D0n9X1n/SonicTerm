use sonic_cfg::keymap::{Action, Keymap};

#[test]
fn wezterm_windows_ctrl_shift_p_opens_command_palette() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let keymap = Keymap::load(&root.join("assets/keymaps/wezterm-windows.toml"))
        .expect("load bundled Windows keymap");

    assert_eq!(keymap.lookup("ctrl+shift+p"), Some(&Action::OpenCommandPalette));
}
