//! Integration tests for the sonicterm-core config re-exports.

use sonicterm_core::config::*;

#[test]
fn default_theme_is_gruvbox_dark_hard() {
    // Default theme is gruvbox-dark-hard (per user direction). The default
    // keymap remains "wezterm" — themes and keymaps are decoupled.
    let cfg = Config::default();
    assert_eq!(cfg.theme, "gruvbox-dark-hard");
    assert_eq!(cfg.keymap, "wezterm");
}

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
    let path = dir.path().join("sonicterm.toml");
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

#[test]
fn defaults_match_wezterm_visual_parity() {
    // Regression: visual-parity targets cribbed from the user's wezterm.lua,
    // with Windows edge-touching fixed by using asymmetric default padding.
    let font = FontConfig::default();
    let window = WindowConfig::default();
    assert!(
        (font.line_height - 1.1).abs() < f32::EPSILON,
        "wezterm parity: line_height must be 1.1, got {}",
        font.line_height
    );
    for (name, got, want) in [
        ("padding_left", window.padding_left, 12.0),
        ("padding_right", window.padding_right, 12.0),
        ("padding_top", window.padding_top, 4.0),
        ("padding_bottom", window.padding_bottom, 4.0),
    ] {
        assert!((got - want).abs() < f32::EPSILON, "window {name} must be {want}, got {got}");
    }
}

/// Legacy `padding = N` in `sonicterm.toml` (single value) must splat onto
/// all four per-side fields after load — users who configured the old
/// shorthand should not have to migrate their config to keep the same
/// visual.
#[test]
fn legacy_padding_scalar_splats_to_all_sides() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonicterm.toml");
    std::fs::write(&path, "[window]\npadding = 12.0\n").unwrap();
    let cfg = Config::load_or_default(&path).unwrap();
    assert!((cfg.window.padding_left - 12.0).abs() < f32::EPSILON);
    assert!((cfg.window.padding_right - 12.0).abs() < f32::EPSILON);
    assert!((cfg.window.padding_top - 12.0).abs() < f32::EPSILON);
    assert!((cfg.window.padding_bottom - 12.0).abs() < f32::EPSILON);
    // The legacy convenience field is consumed during normalize so a
    // subsequent save writes only the canonical per-side fields.
    assert!(cfg.window.padding.is_none());
}

/// Per-side padding values from `sonicterm.toml` reach `WindowConfig`
/// untouched (no shadowing by the legacy field, no clobbering by
/// `normalize_padding`).
#[test]
fn per_side_padding_values_round_trip() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonicterm.toml");
    std::fs::write(
        &path,
        "[window]\npadding_left = 1.0\npadding_right = 2.0\n\
         padding_top = 3.0\npadding_bottom = 4.0\n",
    )
    .unwrap();
    let cfg = Config::load_or_default(&path).unwrap();
    assert!((cfg.window.padding_left - 1.0).abs() < f32::EPSILON);
    assert!((cfg.window.padding_right - 2.0).abs() < f32::EPSILON);
    assert!((cfg.window.padding_top - 3.0).abs() < f32::EPSILON);
    assert!((cfg.window.padding_bottom - 4.0).abs() < f32::EPSILON);
}
