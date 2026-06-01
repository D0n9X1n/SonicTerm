use sonicterm_cfg::config::{BackdropKind, Config};

#[test]
fn appearance_defaults_to_opaque_full_opacity() {
    let cfg = Config::default();
    assert_eq!(cfg.appearance.backdrop, BackdropKind::Opaque);
    assert_eq!(cfg.appearance.opacity, 1.0);
}

#[test]
fn appearance_backdrop_and_opacity_roundtrip_toml() {
    let toml = r#"
[appearance]
backdrop = "mica"
opacity = 0.72
"#;
    let cfg: Config = toml::from_str(toml).expect("parse appearance config");
    assert_eq!(cfg.appearance.backdrop, BackdropKind::Mica);
    assert_eq!(cfg.appearance.opacity, 0.72);

    let serialized = cfg.to_toml().expect("serialize config");
    let roundtrip: Config = toml::from_str(&serialized).expect("roundtrip appearance config");
    assert_eq!(roundtrip.appearance.backdrop, BackdropKind::Mica);
    assert_eq!(roundtrip.appearance.opacity, 0.72);
}
