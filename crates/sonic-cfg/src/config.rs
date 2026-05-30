//! User configuration (TOML).

use std::path::{Path, PathBuf};

use crate::keymap::open_in_default_app;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Top-level user configuration loaded from `sonic.toml`.
pub struct Config {
    /// Font selection and metrics.
    pub font: FontConfig,
    /// Window geometry and chrome.
    pub window: WindowConfig,
    /// Terminal-engine knobs (shell, scrollback, cursor).
    pub terminal: TerminalConfig,
    /// Name of the active theme (looked up in `assets/themes/`).
    pub theme: String,
    /// Name of the active keymap (looked up in `assets/keymaps/`).
    pub keymap: String,
    /// Logging subsystem retention + level knobs (see
    /// [`sonic_logging::LoggingConfig`]).
    #[serde(default)]
    pub logging: sonic_logging::LoggingConfig,
    /// Accessibility presentation modes. Config-only for now.
    #[serde(default)]
    pub accessibility: AccessibilityConfig,
    /// User-selected UI locale (e.g. `"en"`, `"zh-CN"`, `"ja"`). Empty
    /// string (the default) means "negotiate from OS locale".
    #[serde(default)]
    pub locale: String,
    /// Desktop notification settings.
    #[serde(default)]
    pub notifications: NotificationsConfig,
    /// Appearance and compositor backdrop settings.
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// Optional override for the tab close `×` button color. When set
    /// (e.g. `"#ff5555"`), the close button is always visible in this
    /// color, matching WezTerm's `tab_close_button_color` setting.
    /// When `None` (the default), the close button follows WezTerm
    /// fancy-mode parity: hidden until the user hovers the tab, then
    /// drawn dim, brightening on hover of the × glyph itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_close_button_color: Option<String>,
    /// When `true`, the app exits as soon as the last window is closed
    /// (classic single-window-app behavior). When `false` (the default,
    /// matching Chrome/Firefox/Safari on macOS), the application process
    /// stays alive on macOS after the last window closes — keeping the
    /// dock icon active so the user can open a fresh window via
    /// `Cmd+N` or the dock menu without paying cold-start cost. On
    /// non-macOS platforms there is no dock concept and we always exit
    /// once the last window is gone regardless of this setting.
    #[serde(default = "default_quit_on_last_window_close")]
    pub quit_on_last_window_close: bool,
    /// Unknown top-level keys captured verbatim so that newer config keys
    /// (or user/plugin extensions) survive a load/save round-trip. Not
    /// considered when comparing two `Config`s for behavioural equality;
    /// see the manual `PartialEq` impl below.
    #[serde(flatten, default, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Font selection and rendering metrics.
pub struct FontConfig {
    /// Font family name (resolved by fontdb).
    pub family: String,
    /// Font size in points.
    pub size: f32,
    /// Line height multiplier applied to the metric ascent+descent.
    pub line_height: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Window geometry, padding, and chrome settings.
pub struct WindowConfig {
    /// Initial column count.
    pub cols: u16,
    /// Initial row count.
    pub rows: u16,
    /// Legacy single-value padding (logical px). When non-`None` on load,
    /// it is splatted onto all four per-side fields below as a backward-
    /// compatibility shim so existing `sonic.toml` files keep working.
    /// Always serialized as `None` on save; the per-side fields are the
    /// canonical surface.
    #[serde(default, skip_serializing)]
    pub padding: Option<f32>,
    /// Per-side window padding (logical px), matching WezTerm's
    /// `window_padding = { left, right, top, bottom }` knob. Defaults give
    /// the grid breathing room on Windows instead of touching the window edge.
    /// Logical-pixel padding on the left edge.
    pub padding_left: f32,
    /// Logical-pixel padding on the right edge.
    pub padding_right: f32,
    /// Logical-pixel padding on the top edge.
    pub padding_top: f32,
    /// Logical-pixel padding on the bottom edge.
    pub padding_bottom: f32,
    /// Whether to draw native window decorations (titlebar + traffic lights).
    pub decorations: bool,
    /// Window background opacity, 0.0–1.0.
    pub opacity: f32,
    /// Enable the macOS background blur effect (no-op on other platforms).
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

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
/// OS compositor backdrop behind the terminal surface.
pub enum BackdropKind {
    /// Draw an opaque Sonic background with no system material.
    #[default]
    Opaque,
    /// Windows 11 Mica material.
    Mica,
    /// Acrylic blur material.
    Acrylic,
    /// Windows 11 tabbed/titlebar material.
    Tabbed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Appearance and compositor backdrop settings.
pub struct AppearanceConfig {
    /// System compositor backdrop to request on supported platforms.
    pub backdrop: BackdropKind,
    /// Terminal background opacity in the range 0.0..=1.0.
    pub opacity: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self { backdrop: BackdropKind::Opaque, opacity: 1.0 }
    }
}
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Accessibility presentation modes.
pub struct AccessibilityConfig {
    /// Force terminal foreground/background to pure white-on-black.
    #[serde(default)]
    pub high_contrast: bool,
    /// Disable UI animation interpolation; snap controls to end states.
    #[serde(default)]
    pub reduced_motion: bool,
    /// Draw stronger focus indicators.
    #[serde(default)]
    pub strong_focus: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Terminal engine settings.
pub struct TerminalConfig {
    /// Shell to spawn (defaults to `$SHELL` / platform default when `None`).
    pub shell: Option<String>,
    /// Scrollback buffer depth, in rows.
    pub scrollback: usize,
    /// Blink the cursor.
    pub cursor_blink: bool,
    /// Cursor shape.
    pub cursor_shape: CursorShape,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
/// Desktop notification settings.
pub struct NotificationsConfig {
    /// Notify when a long-running command completes.
    #[serde(default = "default_notifications_enabled")]
    pub long_command: bool,
    /// Minimum command duration, in seconds, before completion can notify.
    #[serde(default = "default_threshold_secs")]
    pub threshold_secs: u64,
}

fn default_notifications_enabled() -> bool {
    false
}

fn default_threshold_secs() -> u64 {
    10
}

/// Default for [`Config::quit_on_last_window_close`]: `true`. This
/// matches traditional terminal-emulator behavior (Terminal.app,
/// iTerm2, Alacritty, WezTerm) — closing the last tab on the last
/// window quits the application. Users who prefer the
/// Chrome/Firefox/Safari dock-alive style can opt in by setting
/// `quit_on_last_window_close = false` in `sonic.toml`.
fn default_quit_on_last_window_close() -> bool {
    true
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            long_command: default_notifications_enabled(),
            threshold_secs: default_threshold_secs(),
        }
    }
}

/// Visual cursor shape. Mirrors the DECSCUSR set.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    /// Solid block cursor.
    #[default]
    Block,
    /// Vertical bar / beam cursor.
    Bar,
    /// Horizontal underline cursor.
    Underline,
}

impl CursorShape {
    /// All cursor-shape variants, useful for menu/preference iteration.
    pub const ALL: &'static [CursorShape] =
        &[CursorShape::Block, CursorShape::Bar, CursorShape::Underline];

    /// Lowercase string name matching the TOML serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Bar => "bar",
            CursorShape::Underline => "underline",
        }
    }

    /// Parse a cursor-shape name case-insensitively. Returns `None` for
    /// unrecognized values.
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
            logging: sonic_logging::LoggingConfig::default(),
            accessibility: AccessibilityConfig::default(),
            locale: String::new(),
            notifications: NotificationsConfig::default(),
            appearance: AppearanceConfig::default(),
            tab_close_button_color: None,
            quit_on_last_window_close: default_quit_on_last_window_close(),
            extra: toml::Table::new(),
        }
    }
}

/// Default font family. "Rec Mono St.Helens" is the brand default the project
/// ships with. As of the Windows-build-unblock PR, all four variants
/// (Regular, Italic, Bold, BoldItalic) are bundled in `assets/fonts/` as
/// `RecMonoSt.Helens-*.ttf`, built from MOSconfig/recursive-code-config
/// v1.2.2 and distributed under the SIL Open Font License 1.1. The
/// font-family string registered by fontdb is `"Rec Mono St.Helens"`
/// (with the dot) — that's the exact name to use here. When the family is
/// missing the renderer falls through to the system mono chain;
/// `JetBrainsMono Nerd Font` is also bundled and serves as the implicit
/// fallback. Users can override via `[font] family = "..."` in `sonic.toml`.
pub const DEFAULT_FONT_FAMILY: &str = "Rec Mono St.Helens";

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
            padding_left: 12.0,
            padding_right: 12.0,
            padding_top: 4.0,
            padding_bottom: 4.0,
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

    /// Ensure the editable user config exists, creating a small commented
    /// starter file if necessary.
    pub fn ensure_user_config_file(path: &Path) -> Result<()> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
            }
        }
        const HEADER: &str =
            "# Sonic config — see https://github.com/D0n9X1n/sonic for configuration examples.\n";
        std::fs::write(path, HEADER).with_context(|| format!("write {path:?}"))
    }

    /// Ensure and open the platform user config file.
    pub fn open_user_config_file() -> Result<PathBuf> {
        let path = Self::default_path().ok_or_else(|| anyhow::anyhow!("no user config path"))?;
        Self::ensure_user_config_file(&path)?;
        open_in_default_app(&path)?;
        Ok(path)
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

// Unit tests live in `tests/config.rs`.
