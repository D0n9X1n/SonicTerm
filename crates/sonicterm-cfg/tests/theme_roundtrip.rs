use sonicterm_cfg::theme::Theme;

#[test]
fn theme_export_import_roundtrip_preserves_theme() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let src = root.join("assets/themes/tokyo-night.toml");
    let original = Theme::load(&src).expect("load tokyo-night");

    let temp = tempfile::tempdir().expect("tempdir");
    let exported = temp.path().join("tokyo-night-export.toml");
    original.export_to_file(&exported).expect("export theme");

    let user_theme_dir = temp.path().join("themes");
    let name = Theme::import_from_file(&exported, &user_theme_dir).expect("import theme");
    assert_eq!(name, "tokyo-night");

    let imported = Theme::load(&user_theme_dir.join("tokyo-night.toml")).expect("load imported");
    pretty_assertions::assert_eq!(original, imported);
}
