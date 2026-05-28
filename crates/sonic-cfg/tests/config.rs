//! Tests for `Config::save/load_or_default`, defaults, and `CursorShape`.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/config.rs`.

use sonic_cfg::config::{
    Config, CursorShape, FontConfig, NotificationsConfig, TerminalConfig, WindowConfig,
};
use tempfile::TempDir;

#[test]
fn save_load_roundtrip_preserves_all_fields() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nested/dir/sonic.toml");
    let cfg = Config {
        theme: "tokyo-night".to_string(),
        keymap: "wezterm".to_string(),
        font: FontConfig { family: "Fira Code".to_string(), size: 16.0, line_height: 1.2 },
        window: WindowConfig { opacity: 0.85, ..Default::default() },
        terminal: TerminalConfig {
            shell: None,
            scrollback: 42_000,
            cursor_blink: false,
            cursor_shape: CursorShape::Bar,
        },
        extra: toml::Table::new(),
        accessibility: Default::default(),
        locale: String::new(),
        notifications: NotificationsConfig::default(),
        tab_close_button_color: Some("#ff5555".to_string()),
        logging: sonic_cfg::LoggingConfig::default(),
    };
    cfg.save(&path).unwrap();
    let reloaded = Config::load_or_default(&path).unwrap();
    assert_eq!(cfg, reloaded);
}

#[test]
fn save_is_atomic_no_tmp_left_behind() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonic.toml");
    Config::default().save(&path).unwrap();
    let mut tmp = path.clone();
    tmp.set_file_name("sonic.toml.tmp");
    assert!(!tmp.exists());
    assert!(path.exists());
}

/// WezTerm parity: `window_background_opacity` defaults to 1.0
/// (fully opaque) and `macos_window_background_blur` defaults to 0.
#[test]
fn window_defaults_match_wezterm_parity() {
    let w = WindowConfig::default();
    assert_eq!(w.opacity, 1.0, "default opacity must be fully opaque (WezTerm parity)");
    assert!(!w.blur, "default macOS blur must be off (WezTerm parity)");
    assert!(w.decorations, "default decorations must be on (rounded corners + shadow)");
}

#[test]
fn default_theme_is_gruvbox_dark_hard() {
    assert_eq!(Config::default().theme, "gruvbox-dark-hard");
}

#[test]
fn cursor_shape_parses_case_insensitive() {
    assert_eq!(CursorShape::from_str_ci("BLOCK"), Some(CursorShape::Block));
    assert_eq!(CursorShape::from_str_ci("Bar"), Some(CursorShape::Bar));
    assert_eq!(CursorShape::from_str_ci("underline"), Some(CursorShape::Underline));
    assert_eq!(CursorShape::from_str_ci("beam"), Some(CursorShape::Bar));
    assert_eq!(CursorShape::from_str_ci("nope"), None);
}

#[test]
fn save_writes_blink_false_into_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonic.toml");
    let cfg = Config {
        terminal: TerminalConfig {
            cursor_blink: false,
            cursor_shape: CursorShape::Underline,
            ..Default::default()
        },
        ..Default::default()
    };
    cfg.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("cursor_blink = false"));
    assert!(text.contains("cursor_shape = \"underline\""));
}
