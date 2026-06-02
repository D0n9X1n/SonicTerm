//! Regression tests for #522 — `Config`/`Theme`/`Keymap` infallible loaders
//! must fall back to defaults on TOML parse errors instead of crashing the
//! app at startup. Hot-reload + test paths keep the strict (`Result`) variant.

use std::fs;

use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use tempfile::TempDir;
use tracing_test::traced_test;

const BAD_TOML: &str = "broken =\n";

// ---------- Config ----------

#[test]
#[traced_test]
fn config_load_or_default_falls_back_on_bad_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("sonicterm.toml");
    fs::write(&path, BAD_TOML).unwrap();

    let cfg = Config::load_or_default(&path);
    assert_eq!(cfg, Config::default(), "must return Config::default() on parse error");
    assert!(
        logs_contain("config TOML parse failed"),
        "expected tracing::warn! with fallback message"
    );
}

#[test]
fn config_load_strict_errors_on_bad_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("sonicterm.toml");
    fs::write(&path, BAD_TOML).unwrap();

    assert!(Config::load_strict(&path).is_err(), "strict load must surface parse error");
}

// ---------- Theme ----------

#[test]
#[traced_test]
fn theme_load_or_default_falls_back_on_bad_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("broken-theme.toml");
    fs::write(&path, BAD_TOML).unwrap();

    let theme = Theme::load_or_default(&path);
    assert_eq!(theme, Theme::default(), "must return bundled-default theme on parse error");
    assert!(
        logs_contain("theme TOML parse failed"),
        "expected tracing::warn! with theme fallback message"
    );
}

#[test]
fn theme_load_strict_errors_on_bad_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("broken-theme.toml");
    fs::write(&path, BAD_TOML).unwrap();

    assert!(Theme::load_strict(&path).is_err(), "strict load must surface parse error");
}

// ---------- Keymap ----------

#[test]
#[traced_test]
fn keymap_load_or_default_falls_back_on_bad_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("broken-keymap.toml");
    fs::write(&path, BAD_TOML).unwrap();

    let km = Keymap::load_or_default(&path);
    let default_km = Keymap::default();
    assert_eq!(
        km.bindings.len(),
        default_km.bindings.len(),
        "must return bundled-default keymap on parse error"
    );
    assert!(!km.bindings.is_empty(), "bundled default keymap must contain bindings");
    assert!(
        logs_contain("keymap TOML parse failed"),
        "expected tracing::warn! with keymap fallback message"
    );
}

#[test]
fn keymap_load_strict_errors_on_bad_toml() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("broken-keymap.toml");
    fs::write(&path, BAD_TOML).unwrap();

    assert!(Keymap::load_strict(&path).is_err(), "strict load must surface parse error");
}
