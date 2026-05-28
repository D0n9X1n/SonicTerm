//! Keymap: parsed binding table.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// Value types (Action, Direction, ScrollAction) live in `sonic-types` so any
// crate can match on an Action without pulling in toml/notify/etc. Re-exported
// for source compatibility: every existing
// `use sonic_core::keymap::{Action, Direction, ScrollAction}` keeps compiling.
pub use sonic_types::{Action, Direction, ScrollAction};

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
/// A single key binding: keystroke string → action.
pub struct Binding {
    /// Keystroke specification, e.g. `"super+t"`.
    pub keys: String,
    /// Action to dispatch when the keystroke fires.
    pub action: ActionWrapper,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
/// A loaded keymap document.
pub struct Keymap {
    /// Metadata block (`[meta]` in the TOML).
    pub meta: Meta,
    /// All `[[binding]]` entries.
    #[serde(default, rename = "binding")]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
/// Keymap metadata block.
pub struct Meta {
    /// Keymap name.
    pub name: String,
    /// Keymap schema version.
    pub version: String,
}

impl Keymap {
    /// Load a keymap from a TOML file at `path`.
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
