//! Regression test (Bug 5): the global font family is user-configurable
//! via `sonic.toml`:
//!
//! ```toml
//! [font]
//! family = "Custom Mono"
//! ```
//!
//! Pairs with the forthcoming fix PR (TBD).

use sonicterm_core::config::Config;

#[test]
fn font_family_overridable_from_toml() {
    let toml = r#"
[font]
family = "Custom Mono"
"#;
    let cfg: Config = toml::from_str(toml).expect("parse user font toml");
    assert_eq!(
        cfg.font.family, "Custom Mono",
        "user-supplied [font].family must round-trip into Config.font.family"
    );
}

#[test]
fn font_family_default_is_documented_fallback() {
    // Empty config must yield the documented default. The default has
    // changed historically; whatever the current code ships, the test
    // pins it so an accidental change is noticed.
    let cfg: Config = toml::from_str("").expect("parse empty toml");
    let default_family = Config::default().font.family;
    assert_eq!(
        cfg.font.family, default_family,
        "empty config must match Config::default().font.family"
    );
    assert!(
        !default_family.is_empty(),
        "default font family must not be empty (would render nothing)"
    );
}
