//! Color theme loader.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnsiColors {
    pub black: Hex,
    pub red: Hex,
    pub green: Hex,
    pub yellow: Hex,
    pub blue: Hex,
    pub magenta: Hex,
    pub cyan: Hex,
    pub white: Hex,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TabColors {
    pub bar_bg: Hex,
    pub active_bg: Hex,
    pub active_fg: Hex,
    pub inactive_bg: Hex,
    pub inactive_fg: Hex,
    pub hover_bg: Hex,
    pub close_button_fg: Hex,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Palette {
    pub background: Hex,
    pub foreground: Hex,
    pub cursor: Hex,
    pub cursor_text: Hex,
    pub selection_bg: Hex,
    pub selection_fg: Hex,
    pub ansi: AnsiColors,
    pub bright: AnsiColors,
    pub tab: TabColors,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub colors: Palette,
}

impl Theme {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let t: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        Ok(t)
    }
}
