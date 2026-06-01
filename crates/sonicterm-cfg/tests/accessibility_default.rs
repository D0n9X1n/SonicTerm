use sonicterm_cfg::config::{AccessibilityConfig, Config};

#[test]
fn accessibility_defaults_are_all_false() {
    let a = Config::default().accessibility;
    assert!(!a.high_contrast);
    assert!(!a.reduced_motion);
    assert!(!a.strong_focus);
}

#[test]
fn accessibility_toml_roundtrip_preserves_flags() {
    let cfg = Config {
        accessibility: AccessibilityConfig {
            high_contrast: true,
            reduced_motion: true,
            strong_focus: true,
        },
        ..Config::default()
    };

    let text = cfg.to_toml().expect("serialize config");
    assert!(text.contains("[accessibility]"));
    assert!(text.contains("high_contrast = true"));
    assert!(text.contains("reduced_motion = true"));
    assert!(text.contains("strong_focus = true"));

    let roundtrip: Config = toml::from_str(&text).expect("parse config");
    assert_eq!(roundtrip.accessibility, cfg.accessibility);
}
