//! User configuration (TOML).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub terminal: TerminalConfig,
    pub theme: String,
    pub keymap: String,
    /// User-selected UI locale (e.g. `"en"`, `"zh-CN"`, `"ja"`). Empty
    /// string (the default) means "negotiate from OS locale".
    #[serde(default)]
    pub locale: String,
    /// Optional override for the tab close `×` button color. When set
    /// (e.g. `"#ff5555"`), the close button is always visible in this
    /// color, matching WezTerm's `tab_close_button_color` setting.
    /// When `None` (the default), the close button follows WezTerm
    /// fancy-mode parity: hidden until the user hovers the tab, then
    /// drawn dim, brightening on hover of the × glyph itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_close_button_color: Option<String>,
    /// Unknown top-level keys captured verbatim so that newer config keys
    /// (or user/plugin extensions) survive a load/save round-trip. Not
    /// considered when comparing two `Config`s for behavioural equality;
    /// see the manual `PartialEq` impl below.
    #[serde(flatten, default, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WindowConfig {
    pub cols: u16,
    pub rows: u16,
    /// Legacy single-value padding (logical px). When non-`None` on load,
    /// it is splatted onto all four per-side fields below as a backward-
    /// compatibility shim so existing `sonic.toml` files keep working.
    /// Always serialized as `None` on save; the per-side fields are the
    /// canonical surface.
    #[serde(default, skip_serializing)]
    pub padding: Option<f32>,
    /// Per-side window padding (logical px), matching WezTerm's
    /// `window_padding = { left, right, top, bottom }` knob. Defaults are
    /// cribbed from the user's `wezterm.lua` (8 px on every side).
    pub padding_left: f32,
    pub padding_right: f32,
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub decorations: bool,
    pub opacity: f32,
    pub blur: bool,
}

impl WindowConfig {
    /// Apply the legacy `padding` convenience field (if set) to all four
    /// per-side padding fields, then clear it. Call this after
    /// deserialization so the rest of the engine only ever has to look
    /// at `padding_left / right / top / bottom`.
    pub fn normalize_padding(&mut self) {
        if let Some(p) = self.padding.take() {
            self.padding_left = p;
            self.padding_right = p;
            self.padding_top = p;
            self.padding_bottom = p;
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct TerminalConfig {
    pub shell: Option<String>,
    pub scrollback: usize,
    pub cursor_blink: bool,
    pub cursor_shape: CursorShape,
}

/// Visual cursor shape. Mirrors the DECSCUSR set.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    #[default]
    Block,
    Bar,
    Underline,
}

impl CursorShape {
    pub const ALL: &'static [CursorShape] =
        &[CursorShape::Block, CursorShape::Bar, CursorShape::Underline];

    pub fn as_str(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Bar => "bar",
            CursorShape::Underline => "underline",
        }
    }

    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "block" => Some(CursorShape::Block),
            "bar" | "beam" => Some(CursorShape::Bar),
            "underline" | "underscore" => Some(CursorShape::Underline),
            _ => None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: FontConfig::default(),
            window: WindowConfig::default(),
            terminal: TerminalConfig::default(),
            theme: "gruvbox-dark-hard".to_string(),
            keymap: "wezterm".to_string(),
            locale: String::new(),
            tab_close_button_color: None,
            extra: toml::Table::new(),
        }
    }
}

/// Default font family. "St Helens" is the brand default the project ships
/// with; it is NOT bundled in `assets/fonts/` (no SIL OFL / Apache-licensed
/// variant is available as of this writing) so end users must install it
/// system-wide for it to apply. When the family is missing the renderer
/// falls through to the system mono chain — `JetBrainsMono Nerd Font` is
/// bundled in `assets/fonts/` and serves as the implicit fallback. Users
/// can override via `[font] family = "..."` in `sonic.toml` or via the
/// Font tab of the prefs window.
pub const DEFAULT_FONT_FAMILY: &str = "St Helens";

impl Default for FontConfig {
    fn default() -> Self {
        // line_height 1.1 matches WezTerm's default (visual-parity target).
        Self { family: DEFAULT_FONT_FAMILY.to_string(), size: 14.0, line_height: 1.1 }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            cols: 100,
            rows: 30,
            padding: None,
            padding_left: 8.0,
            padding_right: 8.0,
            padding_top: 8.0,
            padding_bottom: 8.0,
            decorations: true,
            opacity: 1.0,
            blur: false,
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: None,
            scrollback: 10_000,
            cursor_blink: true,
            cursor_shape: CursorShape::default(),
        }
    }
}

impl Config {
    /// Where the user's config lives, by platform convention.
    pub fn default_path() -> Option<PathBuf> {
        let base = if cfg!(target_os = "macos") {
            dirs_home()?.join("Library/Application Support/Sonic")
        } else if cfg!(target_os = "windows") {
            std::env::var_os("APPDATA").map(PathBuf::from)?.join("Sonic")
        } else {
            dirs_home()?.join(".config/sonic")
        };
        Some(base.join("sonic.toml"))
    }

    /// Load from `path`, or return defaults if the file does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let mut cfg: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        cfg.window.normalize_padding();
        Ok(cfg)
    }

    /// Serialize to a TOML string.
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Atomically write this config to `path`, creating parent dirs if
    /// needed. Writes to `<path>.tmp` and renames over the destination so
    /// a crash mid-write cannot corrupt the existing file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
            }
        }
        let toml = self.to_toml()?;
        let mut tmp = path.to_path_buf();
        let file_name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("sonic.toml"));
        let mut tmp_name = file_name;
        tmp_name.push(".tmp");
        tmp.set_file_name(tmp_name);
        std::fs::write(&tmp, toml).with_context(|| format!("write {tmp:?}"))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
        Ok(())
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            locale: String::new(),
            tab_close_button_color: Some("#ff5555".to_string()),
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
    /// Native window decorations (titlebar + traffic lights, with the
    /// OS-supplied rounded corners and drop shadow) are kept on.
    /// Regression test for the window-opacity-decoration-parity change:
    /// shipping anything less than opaque-by-default lets the desktop
    /// bleed through and breaks color fidelity vs WezTerm.
    #[test]
    fn window_defaults_match_wezterm_parity() {
        let w = WindowConfig::default();
        assert_eq!(w.opacity, 1.0, "default opacity must be fully opaque (WezTerm parity)");
        assert!(!w.blur, "default macOS blur must be off (WezTerm parity)");
        assert!(w.decorations, "default decorations must be on (rounded corners + shadow)");
    }

    /// User direction: default theme is `gruvbox-dark-hard`.
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
}
