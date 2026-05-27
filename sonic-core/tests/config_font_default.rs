//! `FontConfig::default()` must resolve to the documented brand-default
//! family ("St Helens"). This pins the constant so a silent edit doesn't
//! quietly change every new user's font without an accompanying spec
//! change. The fallback path (St Helens not installed → renderer falls
//! through to system mono) is documented next to `DEFAULT_FONT_FAMILY`.

use sonic_core::config::{Config, FontConfig, DEFAULT_FONT_FAMILY};

#[test]
fn font_config_default_is_st_helens() {
    let f = FontConfig::default();
    assert_eq!(f.family, "St Helens", "brand-default font family must be St Helens");
    assert_eq!(
        f.family, DEFAULT_FONT_FAMILY,
        "FontConfig::default must match the exported constant"
    );
}

#[test]
fn config_default_threads_font_family() {
    let c = Config::default();
    assert_eq!(c.font.family, DEFAULT_FONT_FAMILY);
}

#[test]
fn font_family_is_user_overridable() {
    // The Family is a free-form String — round-trip through TOML to make
    // sure a user-supplied override is honored (the prefs UI dropdown is
    // a convenience over the same field).
    let toml = r#"
        theme = "tokyo-night"
        keymap = "wezterm"
        [font]
        family = "Menlo"
        size = 16.0
        line_height = 1.2
    "#;
    let cfg: Config = toml::from_str(toml).expect("config parses");
    assert_eq!(cfg.font.family, "Menlo");
}
