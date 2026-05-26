use sonic_core::config::*;

#[test]
fn defaults_roundtrip_through_toml() {
    let cfg = Config::default();
    let s = cfg.to_toml().unwrap();
    let back: Config = toml::from_str(&s).unwrap();
    assert_eq!(back.theme, cfg.theme);
    assert_eq!(back.font.size, cfg.font.size);
}

#[test]
fn missing_fields_get_defaults() {
    let cfg: Config = toml::from_str(r#"theme = "dracula""#).unwrap();
    assert_eq!(cfg.theme, "dracula");
    assert_eq!(cfg.window.cols, 100);
}

#[test]
fn unknown_keys_survive_load_save_roundtrip() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonic.toml");
    let original = "theme = \"x\"\n\
                    font_size = 14\n\
                    my_custom_plugin_key = \"preserved\"\n\
                    \n\
                    [experimental]\n\
                    foo = 1\n";
    std::fs::write(&path, original).unwrap();

    let cfg = Config::load_or_default(&path).unwrap();
    cfg.save(&path).unwrap();

    let reread = std::fs::read_to_string(&path).unwrap();
    assert!(
        reread.contains("my_custom_plugin_key = \"preserved\""),
        "lost top-level unknown key; file was:\n{reread}"
    );
    assert!(reread.contains("font_size = 14"), "lost unknown scalar; file was:\n{reread}");
    assert!(reread.contains("[experimental]"), "lost unknown section header; file was:\n{reread}");
    assert!(reread.contains("foo = 1"), "lost unknown nested key; file was:\n{reread}");
}
