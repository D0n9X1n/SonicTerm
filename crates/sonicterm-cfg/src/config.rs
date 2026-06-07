//! User configuration (TOML).

use std::path::{Path, PathBuf};

use crate::keymap::open_in_default_app;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Top-level user configuration loaded from `sonicterm.toml`.
pub struct Config {
    /// Font selection and metrics.
    pub font: FontConfig,
    /// Window geometry and chrome.
    pub window: WindowConfig,
    /// Terminal-engine knobs (shell, scrollback, cursor).
    pub terminal: TerminalConfig,
    /// Active theme. Accepts a bundled/user theme name or a direct `.toml`
    /// path. Named themes resolve from `<config-dir>/themes/<name>.toml`
    /// first, then bundled `assets/themes/<name>.toml`.
    pub theme: String,
    /// Active keymap. Accepts a bundled keymap name or a direct `.toml`
    /// path. Named keymaps resolve from `<config-dir>/keymaps/<name>.toml`
    /// first, then bundled `assets/keymaps/<name>.toml`.
    pub keymap: String,
    /// Logging subsystem retention + level knobs (see
    /// [`sonicterm_logging::LoggingConfig`]).
    #[serde(default)]
    pub logging: sonicterm_logging::LoggingConfig,
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
    /// Render pipeline implementation toggles (#621 v0.9.2 wezterm-parity).
    /// Each knob selects between v1 (legacy) and v2 (wezterm-parity) impls.
    /// Defaults to v2 everywhere; flip to v1 to revert per-symptom on a
    /// regression report. See `crates/sonicterm-text/src/swash_rasterizer.rs`
    /// and `crates/sonicterm-gpu/src/core.rs`.
    #[serde(default)]
    pub render: RenderConfig,
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
    /// compatibility shim so existing `sonicterm.toml` files keep working.
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
    /// Draw an opaque SonicTerm background with no system material.
    #[default]
    Opaque,
    /// Windows 11 Mica material.
    Mica,
    /// Acrylic blur material.
    Acrylic,
    /// Windows 11 tabbed/titlebar material.
    Tabbed,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
/// Scrollbar visibility policy (PR-A of #386).
pub enum ScrollbarMode {
    /// Show only on hover / active scroll (default). Auto-hide logic
    /// lives in the render layer (PR-B/D); the model treats Auto the
    /// same as Always — the caller decides whether to draw the
    /// returned geometry.
    #[default]
    Auto,
    /// Don't auto-hide; still suppressed when there's nothing to scroll.
    Always,
    /// Never show the scrollbar.
    Never,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Appearance and compositor backdrop settings.
pub struct AppearanceConfig {
    /// System compositor backdrop to request on supported platforms.
    pub backdrop: BackdropKind,
    /// Terminal background opacity in the range 0.0..=1.0.
    pub opacity: f32,
    /// Scrollbar visibility policy.
    pub scrollbar: ScrollbarMode,
    /// Padding between overlay panel chrome and its inner content, in logical px.
    pub panel_padding: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            backdrop: BackdropKind::Opaque,
            opacity: 1.0,
            scrollbar: ScrollbarMode::default(),
            panel_padding: 2.0,
        }
    }
}

/// Render pipeline knobs (#621 v0.9.2 wezterm-parity).
///
/// Each field selects between `v1` (legacy) and `v2` (wezterm-parity) impls.
/// Defaults are `v2`, matching wezterm's behavior. Set a field to `v1` in
/// `sonicterm.toml` under `[render]` to revert that specific symptom path
/// if a regression appears in the field. The two flags are independent so
/// glyph-fit and alt-screen-bg rollback are decoupled.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct RenderConfig {
    /// Glyph-fit pipeline used by `sonicterm-text::swash_rasterizer::apply_symbol_fit`.
    /// `v2` (default) plumbs `num_cells` through shaping → rasterizer and
    /// allows `cell_width * (num_cells + 0.25)` headroom; `v1` is the
    /// pre-#621 clamp-to-advance behavior that left Powerline triangles at
    /// 1-cell width and over-scaled Nerd-Font PUA icons.
    pub glyph_fit: RenderImpl,
    /// Alt-screen background fill mode used by `sonicterm-gpu::core` paint.
    /// `v2` (default) keeps the wgpu clear-color opaque and emits a single
    /// full-viewport translucent bottom-layer quad carrying `bg_opacity`;
    /// `v1` is the pre-#621 behavior that baked `bg_opacity` into the
    /// clear and produced a pale wash in vim/nvim alt-screen because
    /// default-bg cells skip their quad and reveal the translucent clear.
    pub alt_screen_bg_fill: RenderImpl,
}

/// Render-pipeline impl selector for [`RenderConfig`]. Lowercase TOML.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RenderImpl {
    /// Legacy pre-#621 impl. Use if v2 regresses something in your env.
    V1,
    /// wezterm-parity impl (default). Glyph-fit plumbing or opaque-clear
    /// + viewport-quad alt-screen bg, depending on which field this is on.
    #[default]
    V2,
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

/// Default for [`Config::quit_on_last_window_close`]: `true`.
/// Traditional terminal behavior: closing the last window quits the
/// app. Set `quit_on_last_window_close = false` in `sonicterm.toml` for
/// Chrome/Firefox/Safari-style dock-alive behavior where the process
/// stays running after the last window closes (macOS only — other
/// platforms always exit since they have no dock concept).
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
            theme: "wezterm".to_string(),
            keymap: crate::keymap::platform_default_keymap_name().to_string(),
            logging: sonicterm_logging::LoggingConfig::default(),
            accessibility: AccessibilityConfig::default(),
            locale: String::new(),
            notifications: NotificationsConfig::default(),
            appearance: AppearanceConfig::default(),
            render: RenderConfig::default(),
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
/// missing the renderer falls through to the system mono chain. The
/// bundled `Rec Mono St.Helens` TTFs are Nerd-Font-patched, so Powerline
/// (U+E0B0–U+E0BF) and Nerd Font PUA (U+E000–U+F8FF) glyphs resolve in
/// the primary slot without needing a system Nerd Font install. Users can
/// override via `[font] family = "..."` in `sonicterm.toml`.
pub const DEFAULT_FONT_FAMILY: &str = "Rec Mono St.Helens";

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: DEFAULT_FONT_FAMILY.to_string(), size: 13.0, line_height: 1.3 }
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
            padding_top: 10.0,
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
    /// Where the user's config lives.
    ///
    /// SonicTerm 1.0 uses a single cross-platform directory:
    /// `~/.snoicterm/sonicterm.toml`.
    pub fn default_path() -> Option<PathBuf> {
        let base = default_config_dir()?;
        Some(base.join("sonicterm.toml"))
    }

    /// Ensure the editable user config exists, creating a small commented
    /// starter file if necessary.
    pub fn ensure_user_config_file(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
                std::fs::create_dir_all(parent.join("themes"))
                    .with_context(|| format!("create {:?}", parent.join("themes")))?;
                std::fs::create_dir_all(parent.join("keymaps"))
                    .with_context(|| format!("create {:?}", parent.join("keymaps")))?;
                seed_user_examples(parent)?;
            }
        }
        if path.exists() {
            return Ok(());
        }
        std::fs::write(path, default_config_template()).with_context(|| format!("write {path:?}"))
    }

    /// Ensure and open the platform user config file.
    pub fn open_user_config_file() -> Result<PathBuf> {
        let path = Self::default_path().ok_or_else(|| anyhow::anyhow!("no user config path"))?;
        Self::ensure_user_config_file(&path)?;
        open_in_default_app(&path)?;
        Ok(path)
    }

    /// Strict load from `path`. Returns defaults if the file is absent, but
    /// surfaces an `Err` when the file exists and fails to read or parse.
    /// Hot-reload paths and tests use this so a malformed user edit is
    /// visible rather than silently masked.
    pub fn load_strict(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let mut cfg: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        cfg.window.normalize_padding();
        Ok(cfg)
    }

    /// Infallible startup loader. Calls [`Self::load_strict`] and, on any
    /// error, logs a warning at `target = "sonicterm-cfg"` and falls back
    /// to [`Self::default`] so the app can still launch with a broken
    /// user config — see issue #522.
    pub fn load_or_default(path: &Path) -> Self {
        let mut warnings = Vec::new();
        let cfg = Self::load_or_default_collecting(path, &mut warnings);
        for w in warnings {
            tracing::warn!(target: "sonicterm-cfg", "{w}");
        }
        cfg
    }

    /// Same as [`Self::load_or_default`] but, instead of emitting
    /// `tracing::warn!` directly, appends any fallback message to
    /// `warnings`. Use this from `main` BEFORE `sonicterm_logging::init`
    /// has run — otherwise the parse-failure warn is dropped because no
    /// subscriber is installed yet (Haiku review of PR #534).
    ///
    /// Callers MUST drain `warnings` after logging is initialised, e.g.
    /// ```ignore
    /// let mut cfg_warnings = Vec::new();
    /// let config = Config::load_or_default_collecting(&path, &mut cfg_warnings);
    /// let _g = sonicterm_logging::init(&config.logging.clone()).ok();
    /// for w in cfg_warnings { tracing::warn!(target: "sonicterm-cfg", "{w}"); }
    /// ```
    pub fn load_or_default_collecting(path: &Path, warnings: &mut Vec<String>) -> Self {
        match Self::load_strict(path) {
            Ok(cfg) => cfg,
            Err(e) => {
                warnings.push(format!(
                    "config TOML parse failed at {}: {e}; falling back to defaults",
                    path.display()
                ));
                Self::default()
            }
        }
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
            .unwrap_or_else(|| std::ffi::OsString::from("sonicterm.toml"));
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

/// SonicTerm 1.0 config directory (the on-disk layout).
pub fn default_config_dir() -> Option<PathBuf> {
    Some(dirs_home()?.join(".snoicterm"))
}

fn seed_user_examples(config_dir: &Path) -> Result<()> {
    let theme_dir = config_dir.join("themes");
    let keymap_dir = config_dir.join("keymaps");
    write_if_missing(
        &theme_dir.join("wezterm.toml"),
        include_str!("../../../assets/themes/wezterm.toml"),
    )?;
    write_if_missing(
        &keymap_dir.join("sonicterm-macos.toml"),
        include_str!("../../../assets/keymaps/sonicterm-macos.toml"),
    )?;
    write_if_missing(
        &keymap_dir.join("sonicterm-windows.toml"),
        include_str!("../../../assets/keymaps/sonicterm-windows.toml"),
    )?;
    write_if_missing(
        &keymap_dir.join("sonicterm-linux.toml"),
        include_str!("../../../assets/keymaps/sonicterm-linux.toml"),
    )?;
    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {path:?}"))
}

/// Starter config written when the user config file does not exist.
pub fn default_config_template() -> String {
    let cfg = Config::default();
    format!(
        r#"# SonicTerm configuration.
# Path: {config_dir}/sonicterm.toml
# Edit this file and use Reload Config from the command palette to apply changes.
#
# Custom themes:
#   - Put named themes in "{config_dir}/themes/<name>.toml" and set theme = "<name>"
#   - Or set theme = "/absolute/path/to/theme.toml"
#
# Custom keymaps:
#   - Put named keymaps in "{config_dir}/keymaps/<name>.toml" and set keymap = "<name>"
#   - Or set keymap = "/absolute/path/to/keymap.toml"

# Active color theme. Bundled themes live in assets/themes/.
theme = "{theme}"

# Active keymap. The default is platform-specific:
#   macOS   -> sonicterm-macos
#   Windows -> sonicterm-windows
#   Linux   -> sonicterm-linux
keymap = "{keymap}"

# UI language. Empty string means auto-detect from the OS.
locale = ""

# If true, closing the last tab exits the app. If false, SonicTerm keeps the app
# alive so a new window can be opened from the dock/taskbar/menu.
quit_on_last_window_close = true

[font]
# Font family and metrics. SonicTerm ships "Rec Mono St.Helens" by default.
family = "{font_family}"
# Font size in logical pixels.
size = {font_size}
# Line-height multiplier. 1.1 is close to WezTerm's default terminal spacing.
line_height = {line_height}

[window]
# Default terminal grid size for NEW windows. These are character cells, not px.
# The first window is roughly:
#   width  = cols * cell_width  + padding_left + padding_right
#   height = rows * cell_height + padding_top  + padding_bottom + tab/title UI
#
# Window layout (terminal content):
#
#   +--------------------------------------------------+
#   | tab/title UI                                     |
#   +--------------------------------------------------+
#   | padding_top                                      |
#   |  +--------------------------------------------+  |
#   |  | terminal grid: cols x rows                 |  |
#   |  |                                            |  |
#   |  +--------------------------------------------+  |
#   | padding_bottom                                   |
#   +--------------------------------------------------+
#      ^ padding_left                 padding_right ^
cols = {cols}
rows = {rows}

# Padding around the terminal text area, in physical pixels. This affects the
# distance between terminal content and the window/pane edge.
padding_left = {padding_left}
padding_right = {padding_right}
padding_top = {padding_top}
padding_bottom = {padding_bottom}

# Native OS window decorations. Changing this usually requires restart.
decorations = true

# Legacy window opacity knob. Prefer [appearance].opacity for new config; this
# remains for compatibility with older config files.
opacity = 1.0

# Legacy macOS blur knob. Prefer [appearance].backdrop for new config.
blur = false

[terminal]
# Scrollback line limit per pane.
scrollback = {scrollback}

# Cursor behavior. Shape: "block", "bar", or "underline".
cursor_blink = true
cursor_shape = "block"

[logging]
# Logs are written to {config_dir}/logs/sonicterm.log.
# SonicTerm automatically removes logs and crash dumps older than 2 days.
level = "info"
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2

[appearance]
# Window/backdrop/compositor appearance. This is separate from [window] padding:
# - [window].padding_* controls terminal text margins.
# - panel_padding controls the inside padding of pop-up panels (command palette,
#   cheatsheet, etc.).
#
# Panel layout (for command palette / cheatsheet):
#
#   +---------------- floating panel ----------------+
#   | panel_padding                                  |
#   |  +------------------------------------------+  |
#   |  | panel content: search box, rows, hints   |  |
#   |  +------------------------------------------+  |
#   | panel_padding                                  |
#   +------------------------------------------------+
backdrop = "opaque"
opacity = 1.0
scrollbar = "auto"
panel_padding = {panel_padding}

[render]
# Renderer behavior switches. Keep "v2" unless bisecting a rendering regression.
glyph_fit = "v2"
alt_screen_bg_fill = "v2"

[accessibility]
# Presentation preferences.
high_contrast = false
reduced_motion = false
strong_focus = false

[notifications]
# Desktop notification for long-running commands.
long_command = false
threshold_secs = 10
"#,
        config_dir = default_config_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<config-dir>".to_string()),
        theme = cfg.theme,
        keymap = cfg.keymap,
        font_family = cfg.font.family,
        font_size = cfg.font.size,
        line_height = cfg.font.line_height,
        cols = cfg.window.cols,
        rows = cfg.window.rows,
        padding_left = cfg.window.padding_left,
        padding_right = cfg.window.padding_right,
        padding_top = cfg.window.padding_top,
        padding_bottom = cfg.window.padding_bottom,
        panel_padding = cfg.appearance.panel_padding,
        scrollback = cfg.terminal.scrollback,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_paths_live_under_dot_sonicterm() {
        let dir = default_config_dir().expect("home dir should exist in tests");
        assert!(dir.ends_with(".snoicterm"));
        assert_eq!(Config::default_path().unwrap(), dir.join("sonicterm.toml"));
    }

    #[test]
    fn seeding_user_examples_writes_theme_and_platform_keymaps() {
        let dir = std::env::temp_dir().join(format!(
            "sonicterm-config-seed-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        seed_user_examples(&dir).unwrap();
        assert!(dir.join("themes/wezterm.toml").exists());
        assert!(dir.join("keymaps/sonicterm-macos.toml").exists());
        assert!(dir.join("keymaps/sonicterm-windows.toml").exists());
        assert!(dir.join("keymaps/sonicterm-linux.toml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
