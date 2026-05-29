//! Color theme loader.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::AccessibilityConfig;

/// Light vs dark theme appearance hint, used to pick OS-level chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Light appearance.
    Light,
    /// Dark appearance.
    Dark,
}

/// Hex color string in `#rrggbb` form, as written in theme TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hex(pub String);

impl Hex {
    /// Parse `#rrggbb` into (r,g,b). Returns `None` for malformed values.
    pub fn rgb(&self) -> Option<(u8, u8, u8)> {
        let s = self.0.trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some((r, g, b))
    }

    /// Parse `#rrggbb` into opaque RGBA bytes. Returns `None` for malformed values.
    pub fn rgba(&self) -> Option<[u8; 4]> {
        let (r, g, b) = self.rgb()?;
        Some([r, g, b, 255])
    }
}

/// One of the eight standard ANSI palette colors (normal or bright).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AnsiColors {
    /// ANSI color 0 / 8.
    pub black: Hex,
    /// ANSI color 1 / 9.
    pub red: Hex,
    /// ANSI color 2 / 10.
    pub green: Hex,
    /// ANSI color 3 / 11.
    pub yellow: Hex,
    /// ANSI color 4 / 12.
    pub blue: Hex,
    /// ANSI color 5 / 13.
    pub magenta: Hex,
    /// ANSI color 6 / 14.
    pub cyan: Hex,
    /// ANSI color 7 / 15.
    pub white: Hex,
}

/// Tab-bar color slots.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TabColors {
    /// Background for the tab-bar chrome itself.
    pub bar_bg: Hex,
    /// Background for the active tab.
    pub active_bg: Hex,
    /// Foreground for the active tab label.
    pub active_fg: Hex,
    /// Background for inactive tabs.
    pub inactive_bg: Hex,
    /// Foreground for inactive tab labels.
    pub inactive_fg: Hex,
    /// Background for the tab under the mouse cursor.
    pub hover_bg: Hex,
    /// Foreground for the tab under the mouse cursor.
    #[serde(default = "default_hover_fg")]
    pub hover_fg: Hex,
    /// Foreground for the per-tab close (×) button.
    pub close_button_fg: Hex,
}

fn default_hover_fg() -> Hex {
    Hex("#d5c4a1".to_string())
}

/// Full color palette used by a theme.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Palette {
    /// Default terminal background.
    pub background: Hex,
    /// Default terminal foreground.
    pub foreground: Hex,
    /// Cursor block color.
    pub cursor: Hex,
    /// Foreground color used for the character under the cursor.
    pub cursor_text: Hex,
    /// Selection background.
    pub selection_bg: Hex,
    /// Selection foreground.
    pub selection_fg: Hex,
    /// Normal-intensity ANSI palette.
    pub ansi: AnsiColors,
    /// Bright-intensity ANSI palette.
    pub bright: AnsiColors,
    /// Tab-bar palette.
    pub tab: TabColors,
}

/// A complete loadable theme.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Theme {
    /// Human-readable theme name.
    pub name: String,
    /// Light/dark hint.
    pub appearance: Appearance,
    /// Color slots.
    pub colors: Palette,
}

impl Theme {
    /// Apply config-only accessibility presentation overrides after theme
    /// resolution and before renderers derive their cached colors.
    pub fn apply_accessibility(&mut self, a: &AccessibilityConfig) {
        if a.high_contrast {
            self.colors.foreground = Hex("#ffffff".to_string());
            self.colors.background = Hex("#000000".to_string());
        }
    }

    /// First eight normal-intensity ANSI colors in terminal palette order.
    pub fn palette_first_8(&self) -> [&Hex; 8] {
        [
            &self.colors.ansi.black,
            &self.colors.ansi.red,
            &self.colors.ansi.green,
            &self.colors.ansi.yellow,
            &self.colors.ansi.blue,
            &self.colors.ansi.magenta,
            &self.colors.ansi.cyan,
            &self.colors.ansi.white,
        ]
    }

    /// Load a theme from a TOML file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let t: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        Ok(t)
    }

    /// Import a theme TOML file into `user_theme_dir`, returning its canonical name.
    pub fn import_from_file(src: &Path, user_theme_dir: &Path) -> Result<String> {
        let text = std::fs::read_to_string(src).with_context(|| format!("read {src:?}"))?;
        let theme: Self = toml::from_str(&text).with_context(|| format!("parse {src:?}"))?;
        let canonical_name = canonical_theme_name(&theme.name);
        anyhow::ensure!(
            !canonical_name.is_empty(),
            "theme name must contain at least one ASCII alphanumeric character"
        );

        std::fs::create_dir_all(user_theme_dir)
            .with_context(|| format!("create theme dir {user_theme_dir:?}"))?;
        let dst = user_theme_dir.join(format!("{canonical_name}.toml"));
        std::fs::write(&dst, text).with_context(|| format!("write {dst:?}"))?;
        Ok(canonical_name)
    }

    /// Export this theme as TOML to `dst`.
    pub fn export_to_file(&self, dst: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serialize theme")?;
        std::fs::write(dst, text).with_context(|| format!("write {dst:?}"))?;
        Ok(())
    }
}

fn canonical_theme_name(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;

    for ch in name.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch);
            pending_dash = false;
        } else if !out.is_empty() {
            pending_dash = true;
        }
    }

    out
}
