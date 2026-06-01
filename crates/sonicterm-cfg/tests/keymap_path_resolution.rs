#[cfg(target_os = "windows")]
#[test]
fn keymap_path_resolves_under_appdata_on_windows() {
    let root = std::path::PathBuf::from(r"C:\Users\tester\AppData\Roaming");
    std::env::set_var("APPDATA", &root);

    let path = sonicterm_cfg::keymap::default_user_keymap_path().expect("keymap path");

    assert!(path.ends_with(std::path::Path::new(r"Sonic\keymap.toml")), "got {path:?}");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn keymap_path_resolution_smoke() {
    let path = sonicterm_cfg::keymap::default_user_keymap_path().expect("keymap path");
    assert!(path.ends_with("keymap.toml"), "got {path:?}");
}
