//! Regression: `tab_bar_position` defaults to `Bottom` (per the user
//! request that added the field) and round-trips `"top"` / `"bottom"`
//! through TOML.

use sonic_cfg::config::{Config, TabBarPosition};

#[test]
fn empty_config_defaults_to_bottom() {
    let cfg: Config = toml::from_str("").expect("empty TOML parses");
    assert_eq!(cfg.tab_bar_position, TabBarPosition::Bottom);
}

#[test]
fn explicit_top_parses() {
    let cfg: Config = toml::from_str(r#"tab_bar_position = "top""#).expect("parses");
    assert_eq!(cfg.tab_bar_position, TabBarPosition::Top);
}

#[test]
fn explicit_bottom_parses() {
    let cfg: Config = toml::from_str(r#"tab_bar_position = "bottom""#).expect("parses");
    assert_eq!(cfg.tab_bar_position, TabBarPosition::Bottom);
}

#[test]
fn default_impl_is_bottom() {
    assert_eq!(TabBarPosition::default(), TabBarPosition::Bottom);
    assert_eq!(Config::default().tab_bar_position, TabBarPosition::Bottom);
}

#[test]
fn as_str_round_trip() {
    assert_eq!(TabBarPosition::Top.as_str(), "top");
    assert_eq!(TabBarPosition::Bottom.as_str(), "bottom");
}
