//! User configuration (TOML).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub terminal: TerminalConfig,
    pub theme: String,
    pub keymap: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WindowConfig {
    pub cols: u16,
    pub rows: u16,
    pub padding: f32,
    pub decorations: bool,
    pub opacity: f32,
    pub blur: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub shell: Option<String>,
    pub scrollback: usize,
    pub cursor_blink: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: FontConfig::default(),
            window: WindowConfig::default(),
            terminal: TerminalConfig::default(),
            theme: "tokyo-night".to_string(),
            keymap: "wezterm".to_string(),
        }
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "JetBrainsMono Nerd Font".to_string(), size: 14.0, line_height: 1.2 }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { cols: 100, rows: 30, padding: 8.0, decorations: true, opacity: 1.0, blur: false }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { shell: None, scrollback: 10_000, cursor_blink: true }
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
        let cfg: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        Ok(cfg)
    }

    /// Serialize to a TOML string.
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
