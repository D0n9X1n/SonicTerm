use super::*;

#[test]
fn default_keymap_path_lives_under_dot_sonicterm() {
    let path = default_user_keymap_path().expect("home dir should exist in tests");
    assert!(path.starts_with(crate::config::default_config_dir().unwrap()));
    let expected_name = format!("{}.toml", platform_default_keymap_name());
    assert_eq!(path.file_name().and_then(|s| s.to_str()), Some(expected_name.as_str()));
    assert_eq!(path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()), Some("keymaps"));
}

#[test]
fn windows_default_leaves_alt_v_for_terminal_apps() {
    if !cfg!(target_os = "windows") {
        return;
    }
    let keymap = Keymap::bundled_default();

    assert_eq!(keymap.lookup("alt+v"), None);
    assert_eq!(keymap.lookup("ctrl+shift+v"), Some(&Action::PasteFromClipboard));
}
