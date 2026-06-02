use sonicterm_cfg::keymap::{Action, Keymap};

#[test]
fn wezterm_windows_ctrl_t_opens_new_tab() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let keymap = Keymap::load_strict(&root.join("assets/keymaps/sonicterm-windows.toml"))
        .expect("load bundled Windows keymap");

    assert_eq!(keymap.lookup("ctrl+t"), Some(&Action::NewTab));
}
