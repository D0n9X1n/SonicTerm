//! Keymap: parsed binding table.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// Value types (Action, Direction, ScrollAction) live in `sonicterm-types` so any
// crate can match on an Action without pulling in toml/notify/etc. Re-exported
// for source compatibility: every existing
// `use sonicterm_cfg::keymap::{Action, Direction, ScrollAction}` keeps compiling.
pub use sonicterm_types::{Action, BroadcastScope, Direction, ScrollAction};

/// Platform-specific bundled default keymap name used to seed the editable
/// user keymap file.
pub const fn platform_default_keymap_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "sonicterm-windows"
    } else {
        "sonicterm"
    }
}

/// Platform-specific default user keymap path.
///
/// Windows uses `%APPDATA%\SonicTerm\keymap.toml`; macOS uses
/// `~/Library/Application Support/SonicTerm/keymap.toml`. Other platforms follow
/// SonicTerm's config-dir fallback so tests and non-shipping builds stay usable.
pub fn default_user_keymap_path() -> Option<std::path::PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::path::PathBuf::from(std::env::var_os("HOME")?)
            .join("Library/Application Support/SonicTerm")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)?.join("SonicTerm")
    } else {
        std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config/sonicterm")
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
        include_str!("../../../assets/keymaps/sonicterm-windows.toml")
    } else {
        include_str!("../../../assets/keymaps/sonicterm.toml")
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
    /// Strict load of a keymap from a TOML file at `path`.
    pub fn load_strict(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let km: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        Ok(km)
    }

    /// Infallible loader. On any error, logs a warning at
    /// `target = "sonicterm-cfg"` and returns [`Self::default`] — see #522.
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load_strict(path) {
            Ok(km) => km,
            Err(e) => {
                tracing::warn!(
                    target: "sonicterm-cfg",
                    "keymap TOML parse failed at {}: {e}; falling back to defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Bundled default keymap, embedded at compile time and used by
    /// [`Self::load_or_default`] as the infallible fallback. On Windows we
    /// embed the windows-specific defaults; everywhere else the unix map.
    pub fn bundled_default() -> Self {
        #[cfg(target_os = "windows")]
        const BUNDLED: &str = include_str!("../../../assets/keymaps/sonicterm-windows.toml");
        #[cfg(not(target_os = "windows"))]
        const BUNDLED: &str = include_str!("../../../assets/keymaps/sonicterm.toml");
        toml::from_str(BUNDLED).expect("bundled keymap must parse")
    }

    /// Look up the first action bound to `keys` (case-insensitive). Returns
    /// `None` if no binding matches.
    pub fn lookup(&self, keys: &str) -> Option<&Action> {
        let needle = keys.to_ascii_lowercase();
        self.bindings.iter().find(|b| b.keys.to_ascii_lowercase() == needle).map(|b| &b.action.0)
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::bundled_default()
    }
}
