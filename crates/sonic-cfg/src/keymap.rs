//! Keymap: parsed binding table.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// Value types (Action, Direction, ScrollAction) live in `sonic-types` so any
// crate can match on an Action without pulling in toml/notify/etc. Re-exported
// for source compatibility: every existing
// `use sonic_core::keymap::{Action, Direction, ScrollAction}` keeps compiling.
pub use sonic_types::{Action, BroadcastScope, Direction, ScrollAction};

/// Platform-specific bundled default keymap name used to seed the editable
/// user keymap file.
pub const fn platform_default_keymap_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "wezterm-windows"
    } else {
        "wezterm"
    }
}

/// Platform-specific default user keymap path.
///
/// Windows uses `%APPDATA%\Sonic\keymap.toml`; macOS uses
/// `~/Library/Application Support/Sonic/keymap.toml`. Other platforms follow
/// Sonic's config-dir fallback so tests and non-shipping builds stay usable.
pub fn default_user_keymap_path() -> Option<std::path::PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::path::PathBuf::from(std::env::var_os("HOME")?)
            .join("Library/Application Support/Sonic")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)?.join("Sonic")
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config/sonic")
    };
    Some(base.join("keymap.toml"))
}

/// Ensure the editable user keymap exists, seeding it from the bundled
/// platform default if necessary.
pub fn ensure_user_keymap_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
    }
    let default_text = if cfg!(target_os = "windows") {
        include_str!("../../../assets/keymaps/wezterm-windows.toml")
    } else {
        include_str!("../../../assets/keymaps/wezterm.toml")
    };
    std::fs::write(path, default_text).with_context(|| format!("write {path:?}"))
}

/// Open `path` in the OS default editor/application.
#[cfg(target_os = "windows")]
pub fn open_in_default_app(path: &Path) -> Result<()> {
    std::process::Command::new("cmd")
        .arg("/c")
        .arg("start")
        .arg("")
        .arg(path)
        .spawn()
        .with_context(|| format!("open {path:?}"))?;
    Ok(())
}

/// Open `path` in the OS default editor/application.
#[cfg(target_os = "macos")]
pub fn open_in_default_app(path: &Path) -> Result<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .with_context(|| format!("open {path:?}"))?;
    Ok(())
}

/// Open `path` in the OS default editor/application.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn open_in_default_app(path: &Path) -> Result<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .with_context(|| format!("open {path:?}"))?;
    Ok(())
}

/// Ensure and open the platform user keymap file.
pub fn open_user_keymap_file() -> Result<std::path::PathBuf> {
    let path = default_user_keymap_path().ok_or_else(|| anyhow::anyhow!("no user keymap path"))?;
    ensure_user_keymap_file(&path)?;
    open_in_default_app(&path)?;
    Ok(path)
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
