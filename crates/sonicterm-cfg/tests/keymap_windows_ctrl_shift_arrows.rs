use sonicterm_cfg::keymap::{Action, Keymap};

fn windows_keymap() -> Keymap {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/keymaps/sonicterm-windows.toml");
    Keymap::load_strict(&path).expect("load bundled Windows keymap")
}

#[test]
fn default_windows_keymap_binds_ctrl_shift_arrows_to_tab_switch() {
    let keymap = windows_keymap();
    assert_eq!(keymap.lookup("ctrl+shift+left"), Some(&Action::PrevTab));
    assert_eq!(keymap.lookup("ctrl+shift+right"), Some(&Action::NextTab));
}

#[test]
fn default_windows_keymap_binds_ctrl_shift_w_to_close_active_pane_or_tab() {
    let keymap = windows_keymap();
    assert_eq!(
        keymap.lookup("ctrl+shift+w"),
        Some(&Action::CloseActivePaneOrTab),
        "ctrl+shift+w must mirror Mac Cmd+W (close pane, fall through to tab)"
    );
}
