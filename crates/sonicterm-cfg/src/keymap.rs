//! Keymap: parsed binding table.

use std::path::{Path, PathBuf};

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
        // When: target_os is windows, so the seeded user keymap carries the
        // Windows binding set.
        "sonicterm-windows"
    } else if cfg!(target_os = "macos") {
        // When: target_os is macos, so the seeded user keymap carries the
        // Apple binding set.
        "sonicterm-macos"
    } else {
        // When: cfg matched neither Windows nor Apple, so the Linux binding
        // set is the remaining default.
        "sonicterm-linux"
    }
}

/// Default user keymap path under `~/.sonicterm/keymaps/`.
pub fn default_user_keymap_path() -> Option<std::path::PathBuf> {
    let base = crate::config::default_config_dir()?;
    Some(base.join("keymaps").join(format!("{}.toml", platform_default_keymap_name())))
}

/// Ensure the editable user keymap exists, seeding it from the bundled
/// platform default if necessary.
pub fn ensure_user_keymap_file(path: &Path) -> Result<()> {
    if path.exists() {
        // When: path already exists, so seeding is skipped and the user's own
        // edits survive.
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
    }
    let default_text = Keymap::bundled_default_text();
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

/// Intermediate keymap shape used by [`Keymap::parse_resilient`].
///
/// Identical to [`Keymap`] except each binding's `action` stays an
/// unresolved [`toml::Value`], so the structural parse of the whole
/// document never fails on a single unknown/removed action variant. Each
/// action is resolved (and individually skipped on failure) in a second
/// pass.
#[derive(Debug, Clone, Deserialize)]
struct RawKeymap {
    meta: Meta,
    #[serde(default, rename = "binding")]
    binding: Vec<RawBinding>,
}

/// One binding with its action left unresolved. See [`RawKeymap`].
#[derive(Debug, Clone, Deserialize)]
struct RawBinding {
    keys: String,
    action: toml::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
/// Keymap metadata block.
pub struct Meta {
    /// Keymap name.
    pub name: String,
    /// Keymap schema version.
    pub version: String,
}

/// True when `keymap` is an explicit filesystem path rather than a logical name.
///
/// Drive-letter and separator shapes are recognized on every host so a portable
/// config classifies identically cross-platform. Only a case-insensitive `.toml`
/// suffix makes an extension a path, keeping names such as `sonicterm-v1.2` logical.
fn keymap_looks_like_path(keymap: &str) -> bool {
    let bytes = keymap.as_bytes();
    Path::new(keymap).is_absolute()
        || keymap.contains(['/', '\\'])
        || bytes.get(1) == Some(&b':') && bytes.first().is_some_and(u8::is_ascii_alphabetic)
        || keymap.to_ascii_lowercase().ends_with(".toml")
}

impl Keymap {
    /// Resolve a keymap name or path, creating the portable `user` alias if needed.
    ///
    /// Resolution order:
    /// 1. `user`: the editable platform-default keymap under the user config dir,
    /// 2. explicit path: absolute, separator-bearing, drive-qualified, or `.toml`,
    /// 3. user config dir: `<config-dir>/keymaps/<keymap>.toml`,
    /// 4. bundled assets: `<asset-dir>/keymaps/<keymap>.toml`.
    pub fn resolve_path(keymap: &str, asset_dir: &Path) -> Result<PathBuf> {
        Self::resolve_path_with(keymap, asset_dir, crate::config::default_config_dir().as_deref())
    }

    fn resolve_path_with(
        keymap: &str,
        asset_dir: &Path,
        user_config_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        if keymap == "user" {
            // When: `keymap == "user"`, resolve the portable alias to this
            // host's editable platform-default file and create it if absent.
            let config_dir = user_config_dir
                .ok_or_else(|| anyhow::anyhow!("no user config directory for keymap alias"))?;
            let path =
                config_dir.join("keymaps").join(format!("{}.toml", platform_default_keymap_name()));
            ensure_user_keymap_file(&path)
                .with_context(|| format!("seed user keymap alias at {}", path.display()))?;
            return Ok(path);
        }

        let raw = Path::new(keymap);
        if keymap_looks_like_path(keymap) {
            // When: `keymap_looks_like_path(keymap)` is true, preserve the
            // explicit path and its process-CWD anchor when relative.
            return Ok(raw.to_path_buf());
        }
        if let Some(user) = user_config_dir
            .map(|dir| dir.join("keymaps").join(format!("{keymap}.toml")))
            .filter(|path| path.exists())
        {
            // When: the user keymap exists, it overrides the bundled file with
            // the same logical name.
            return Ok(user);
        }
        Ok(asset_dir.join("keymaps").join(format!("{keymap}.toml")))
    }

    /// Strict load from a keymap name or path using [`Self::resolve_path`].
    pub fn load_name_or_path(keymap: &str, asset_dir: &Path) -> Result<Self> {
        let path = Self::resolve_path(keymap, asset_dir)?;
        Self::load_strict(&path)
    }

    /// Infallible name/path loader. Falls back to bundled platform default.
    pub fn load_name_or_default(keymap: &str, asset_dir: &Path) -> Self {
        match Self::load_name_or_path(keymap, asset_dir) {
            Ok(keymap) => keymap,
            Err(error) => {
                tracing::warn!(
                    target: "sonicterm-cfg",
                    "keymap {keymap:?} failed: {error:#}; falling back to platform defaults"
                );
                Self::default()
            }
        }
    }

    /// Load a keymap from a TOML file at `path`, resiliently.
    ///
    /// The parse is per-binding: each binding whose action cannot be resolved
    /// is skipped with a `WARN` that names the offending `keys`, and the
    /// remaining bindings are kept, so one unknown action variant never
    /// discards the user's other customizations. `Err` is returned only for a
    /// *structural* problem (invalid TOML, missing `[meta]`), which is the case
    /// that warrants [`Self::load_or_default`]'s bundled-default fallback.
    pub fn load_strict(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        Self::parse_resilient(&text, &format!("{path:?}"))
    }

    /// Parse keymap TOML, dropping (with a warning) only the bindings whose
    /// action fails to resolve, instead of failing the whole document.
    ///
    /// `source` is a human label used in warnings/errors (typically the
    /// quoted path). Returns `Err` only for structural TOML errors — a bad
    /// action in one binding never poisons the others. Performs no filesystem
    /// access, so it is unit-testable on literal document text.
    pub fn parse_resilient(text: &str, source: &str) -> Result<Self> {
        // First pass: structural parse with the action left as a raw value
        // so an unknown action variant does NOT fail the whole document.
        let raw: RawKeymap = toml::from_str(text).with_context(|| format!("parse {source}"))?;
        let mut bindings = Vec::with_capacity(raw.binding.len());
        for rb in raw.binding {
            // Second pass: resolve each action individually, reusing the
            // existing `ActionWrapper` deserializer (string or table form).
            let resolved: std::result::Result<ActionWrapper, toml::de::Error> =
                rb.action.try_into();
            match resolved {
                Ok(action) => bindings.push(Binding { keys: rb.keys, action }),
                Err(e) => {
                    tracing::warn!(
                        target: "sonicterm-cfg",
                        "skipping keymap binding keys={:?} in {source}: {e}",
                        rb.keys,
                    );
                }
            }
        }
        Ok(Keymap { meta: raw.meta, bindings })
    }

    /// Infallible loader. On any error, logs a warning at
    /// `target = "sonicterm-cfg"` and returns [`Self::default`].
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
        toml::from_str(Self::bundled_default_text()).expect("bundled keymap must parse")
    }

    fn bundled_default_text() -> &'static str {
        if cfg!(target_os = "windows") {
            // When: target_os is windows, so the Windows defaults are embedded
            // at compile time.
            include_str!("../../../assets/keymaps/sonicterm-windows.toml")
        } else if cfg!(target_os = "macos") {
            // When: target_os is macos, so the Apple defaults are embedded at
            // compile time.
            include_str!("../../../assets/keymaps/sonicterm-macos.toml")
        } else {
            // When: cfg matched neither Windows nor Apple, so the Linux
            // defaults are embedded at compile time.
            include_str!("../../../assets/keymaps/sonicterm-linux.toml")
        }
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

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod keymap_tests;
