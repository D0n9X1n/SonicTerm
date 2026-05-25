//! Keymap: parsed binding table.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Direction for split/focus actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Scroll target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollAction {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    ToTop,
    ToBottom,
}

/// All actions a binding may trigger. The renaming makes the TOML pretty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    // Tabs
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    ActivateTab(usize),
    ActivateLastTab,

    // Splits
    SplitRight,
    SplitDown,
    ClosePane,
    FocusPane(Direction),
    ResizePane { dir: Direction, amount: u16 },

    // Clipboard
    CopyToClipboard,
    PasteFromClipboard,

    // Font
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,

    // Window
    NewWindow,
    ToggleFullscreen,

    // Search / palette
    OpenSearch,
    OpenCommandPalette,
    OpenPreferences,

    // Scroll
    Scroll(ScrollAction),

    // Config
    ReloadConfig,
}

impl<'de> Deserialize<'de> for ActionWrapper {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Accept either bare string `action = "new_tab"` or table `action = { ... }`
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Bare(String),
            Typed(Action),
        }
        match Either::deserialize(de)? {
            Either::Typed(a) => Ok(ActionWrapper(a)),
            Either::Bare(s) => {
                let a: Action = serde_plain::from_str(&s).map_err(serde::de::Error::custom)?;
                Ok(ActionWrapper(a))
            }
        }
    }
}

/// Newtype so we can write a custom deserializer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionWrapper(pub Action);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Binding {
    pub keys: String,
    pub action: ActionWrapper,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Keymap {
    pub meta: Meta,
    #[serde(default, rename = "binding")]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Meta {
    pub name: String,
    pub version: String,
}

impl Keymap {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let km: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        Ok(km)
    }

    /// Look up the first action bound to `keys` (case-insensitive). Returns
    /// `None` if no binding matches.
    pub fn lookup(&self, keys: &str) -> Option<&Action> {
        let needle = keys.to_ascii_lowercase();
        self.bindings.iter().find(|b| b.keys.to_ascii_lowercase() == needle).map(|b| &b.action.0)
    }
}
