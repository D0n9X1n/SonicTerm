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
