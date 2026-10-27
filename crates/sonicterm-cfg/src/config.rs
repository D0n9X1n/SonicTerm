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
    /// Status line strip between the panes and the tab bar.
    #[serde(default)]
    pub status_line: crate::status_line::StatusLineConfig,
    /// Appearance and compositor backdrop settings.
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// Render-pipeline compatibility settings.
    #[serde(default)]
    pub render: RenderConfig,
    /// Deprecated compatibility key for the former tab close `×` button.
    /// The button is no longer drawn, so this value currently has no visual
    /// effect. It remains deserializable so older config files keep loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_close_button_color: Option<String>,
    /// Minimum width, in logical pixels, of a single tab, so a short title
    /// such as `A` still gets a comfortable tab while the bar has room. When
    /// the tabs do not fit, they shrink below it, down to the font-derived
    /// readable width, before the bar overflows. A value larger than
    /// `tab_max_width` wins over it. Defaults to `240.0`; a non-finite or
    /// non-positive value is ignored.
    #[serde(default = "default_tab_min_width")]
    pub tab_min_width: f32,
    /// Maximum width, in logical pixels, of a single tab. Each tab sizes to
    /// its content (the `#N` index and process icon, the privilege marker,
    /// the title and the command-status badge) between `tab_min_width` and
    /// this width, so a longer title shows whole up to this width. A title
    /// wider than this is cut with an ellipsis, and when the tabs do not fit
    /// the bar the widest tabs shrink first. Defaults to `320.0`; a
    /// non-finite or non-positive value is ignored. Raise it for roomier
    /// tabs, lower it to pack more in.
    #[serde(default = "default_tab_max_width")]
    pub tab_max_width: f32,
    /// Accepted for compatibility and ignored: SonicTerm exits when its last
    /// window closes, on every platform and whatever the value. The key stays
    /// so existing `sonicterm.toml` files that set it keep loading.
    #[serde(default = "default_quit_on_last_window_close")]
    pub quit_on_last_window_close: bool,
    /// Unknown top-level keys captured verbatim so that newer config keys
    /// (or user/plugin extensions) survive a load/save round-trip. Not
    /// considered when comparing two `Config`s for behavioural equality;
    /// see the manual `PartialEq` impl below.
    #[serde(flatten, default, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

/// LCD subpixel coverage presentation policy.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubpixelAaMode {
    /// Preserve grayscale antialiasing.
    #[default]
    Off,
    /// Map atlas red, green, and blue coverage to matching display channels.
    Rgb,
    /// Swap red and blue coverage for BGR stripe-order displays.
    Bgr,
}

impl SubpixelAaMode {
    /// Lowercase name used by the TOML schema and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Rgb => "rgb",
            Self::Bgr => "bgr",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
/// Font selection and rendering metrics.
pub struct FontConfig {
    /// Font family name.
    pub family: String,
    /// Font size in points.
    pub size: f32,
    /// Line height multiplier applied to the metric ascent+descent.
    pub line_height: f32,
    /// Post-selection coverage multiplier for all monochrome text. `1.0` preserves native weight.
    pub weight_scale: f32,
    /// LCD subpixel coverage order. Effective only on eligible Windows targets.
    pub subpixel_aa: SubpixelAaMode,
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
    /// Legacy window-opacity value retained for config compatibility. Current
    /// rendering uses [`AppearanceConfig::opacity`] instead.
    pub opacity: f32,
    /// Legacy macOS blur value retained for config compatibility. Current
    /// platform setup uses [`AppearanceConfig::backdrop`] instead.
    pub blur: bool,
    /// Requested hidden child-window pool for tab tear-out. Zero disables it;
    /// hardware honors 1..=5 and software adapters cap nonzero values at one.
    pub warm_window_pool: u8,
}

impl WindowConfig {
    /// Apply the legacy `padding` convenience field (if set) to all four
    /// per-side padding fields, then clear it. Call this after
    /// deserialization so the rest of the engine only ever has to look
    /// at `padding_left / right / top / bottom`.
    pub fn normalize_padding(&mut self) {
        if let Some(padding) = self.padding.take() {
            self.padding_left = padding;
            self.padding_right = padding;
            self.padding_top = padding;
            self.padding_bottom = padding;
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
/// Scrollbar visibility policy.
pub enum ScrollbarMode {
    /// Show only on hover / active scroll (default). Auto-hide logic
    /// lives in the render layer; the model treats Auto the
    /// same as Always — the caller decides whether to draw the
    /// returned geometry.
    #[default]
    Auto,
    /// Don't auto-hide; still suppressed when there's nothing to scroll.
    Always,
    /// Never show the scrollbar.
    Never,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
/// How to handle the no-GPU / software-rasterizer case.
///
/// When wgpu falls back to a CPU rasterizer (WARP, llvmpipe, SwiftShader —
/// common under RDP / VMs / VDI), every frame is drawn on the CPU. SonicTerm
/// can detect this and degrade (lower frame cap, no per-frame fade animation)
/// to stay usable.
pub enum SoftwareRenderMode {
    /// Detect software rendering automatically and degrade when found
    /// (default). Hardware-GPU machines are unaffected.
    #[default]
    Auto,
    /// Always treat rendering as software-bound (force the degrade path even
    /// if a GPU is reported — useful over remote sessions that look like a
    /// GPU but aren't).
    Force,
    /// Never degrade; always use the full per-frame path regardless of the
    /// adapter.
    Off,
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
    /// No-GPU / software-rasterizer handling.
    pub software_render_mode: SoftwareRenderMode,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            backdrop: BackdropKind::Opaque,
            opacity: 1.0,
            scrollbar: ScrollbarMode::default(),
            panel_padding: 2.0,
            software_render_mode: SoftwareRenderMode::default(),
        }
    }
}

/// Render-pipeline compatibility settings.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct RenderConfig {
    /// Reserved legacy selector for alt-screen background behavior.
    ///
    /// The current renderer does not read this value; it remains deserializable
    /// so existing files continue to load.
    pub alt_screen_bg_fill: RenderImpl,
}

/// Lowercase TOML selector retained for legacy render configuration.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RenderImpl {
    /// Legacy selector value.
    V1,
    /// Default selector value.
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
    /// `TERM_PROGRAM` value passed to child PTYs.
    pub term_program: String,
    /// Scrollback buffer depth, in rows.
    pub scrollback: usize,
    /// Legacy keypad preference; negotiated Kitty input is unaffected.
    pub keypad_mode: KeypadMode,
    /// Whether native absolute and explicit-relative filesystem targets are clickable.
    pub clickable_local_targets: bool,
    /// Whether whole bare components may resolve against trusted pane CWD.
    pub clickable_bare_names: bool,
    /// Longest auto-detected path considered, in Unicode scalars; see [`MAX_CLICKABLE_PATH_CHARS`].
    pub clickable_path_max_chars: usize,
    /// Blink the cursor.
    pub cursor_blink: bool,
    /// Cursor shape.
    pub cursor_shape: CursorShape,
}

/// User preference for legacy keypad input, independent of hardware NumLock.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeypadMode {
    /// Preserve negotiated keypad behavior and OS-resolved digit text.
    #[default]
    Auto,
    /// Use normal keypad text and Return rules regardless of DECKPAM.
    Numeric,
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
    /// Where OSC 9 and OSC 777 program notifications go.
    pub osc9: Osc9Delivery,
    /// Mirror `OSC 9;4` progress to the taskbar, Dock or launcher.
    #[serde(default = "default_taskbar_progress")]
    pub taskbar_progress: bool,
}

/// Where OSC 9 and OSC 777 program notifications go.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Osc9Delivery {
    /// SonicTerm's in-app notification in the pane's window.
    #[default]
    App,
    /// The in-app notification, plus a desktop notification on Windows while the window is not
    /// focused; macOS and Linux behave as `app`.
    Desktop,
    /// Ignore program notifications.
    Off,
}

fn default_taskbar_progress() -> bool {
    true
}

fn default_notifications_enabled() -> bool {
    false
}

fn default_threshold_secs() -> u64 {
    10
}

/// Default for [`Config::quit_on_last_window_close`]: `true`. The value is
/// ignored; SonicTerm exits when its last window closes on every platform.
fn default_quit_on_last_window_close() -> bool {
    true
}

/// Default minimum width of a single tab, in logical pixels. It matches the
/// renderer's built-in `TAB_MIN_WIDTH`, so a layout before and after the
/// config loads gives a one-letter title the same comfortable tab.
fn default_tab_min_width() -> f32 {
    240.0
}

/// Default maximum width of a single tab, in logical pixels. It matches the
/// renderer's built-in `TAB_MAX_WIDTH`, so a layout before and after the
/// config loads caps a long title at the same width.
fn default_tab_max_width() -> f32 {
    320.0
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            long_command: default_notifications_enabled(),
            threshold_secs: default_threshold_secs(),
            osc9: Osc9Delivery::default(),
            taskbar_progress: default_taskbar_progress(),
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
    pub fn from_str_ci(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
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
            status_line: crate::status_line::StatusLineConfig::default(),
            appearance: AppearanceConfig::default(),
            render: RenderConfig::default(),
            tab_close_button_color: None,
            tab_min_width: default_tab_min_width(),
            tab_max_width: default_tab_max_width(),
            quit_on_last_window_close: default_quit_on_last_window_close(),
            extra: toml::Table::new(),
        }
    }
}

/// Default font family: the platform's own monospace face, so an unconfigured install renders
/// with a family the host already provides. SonicTerm bundles no text face.
///
/// macOS uses Menlo. Windows asks for Cascadia Mono, and the font stack falls back to Consolas
/// where Cascadia Mono is not installed. Linux and other Unix systems ask fontconfig for its
/// `monospace` alias. Matches `sonicterm_engine::DEFAULT_FONT_FAMILY`; this crate does not
/// depend on the font stack, so the value is repeated here. Powerline and Nerd Font icons come
/// from the bundled `assets/fonts/SymbolsNerdFontMono-Regular.ttf` in every fallback chain.
/// Users override the family with `[font] family = "..."` in `sonicterm.toml`.
#[cfg(target_os = "macos")]
pub const DEFAULT_FONT_FAMILY: &str = "Menlo";
/// Default font family: the platform's own monospace face (Cascadia Mono on Windows).
#[cfg(windows)]
pub const DEFAULT_FONT_FAMILY: &str = "Cascadia Mono";
/// Default font family: the face fontconfig resolves for its `monospace` alias.
#[cfg(not(any(target_os = "macos", windows)))]
pub const DEFAULT_FONT_FAMILY: &str = "monospace";

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: DEFAULT_FONT_FAMILY.to_string(),
            size: 13.0,
            line_height: 1.3,
            weight_scale: 1.0,
            subpixel_aa: SubpixelAaMode::Off,
        }
    }
}

impl FontConfig {
    /// Return a safe monochrome-text weight scale. Invalid values preserve the
    /// native font coverage instead of reaching raster math.
    pub fn effective_weight_scale(&self) -> f32 {
        if self.weight_scale.is_finite() && (0.5..=5.0).contains(&self.weight_scale) {
            self.weight_scale
        } else {
            // When: `weight_scale` is nonfinite or outside the supported range, preserve native coverage with identity.
            1.0
        }
    }

    fn normalize(&mut self) {
        self.weight_scale = self.effective_weight_scale();
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
            padding_top: 8.0,
            padding_bottom: 4.0,
            decorations: true,
            opacity: 1.0,
            blur: false,
            warm_window_pool: 1,
        }
    }
}

/// Default for `terminal.clickable_path_max_chars` when it is not configured: the supported maximum.
pub const DEFAULT_CLICKABLE_PATH_CHARS: usize = MAX_CLICKABLE_PATH_CHARS;

/// Largest supported `terminal.clickable_path_max_chars`: 1024 four-byte scalars fill the
/// scanner's 4 KiB target cap.
pub const MAX_CLICKABLE_PATH_CHARS: usize = 1024;

impl TerminalConfig {
    /// The configured path length cap clamped to `1..=MAX_CLICKABLE_PATH_CHARS`.
    #[must_use]
    pub fn effective_clickable_path_max_chars(&self) -> usize {
        self.clickable_path_max_chars.clamp(1, MAX_CLICKABLE_PATH_CHARS)
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: None,
            term_program: "SonicTerm".to_string(),
            scrollback: 1_000,
            keypad_mode: KeypadMode::default(),
            clickable_local_targets: true,
            clickable_bare_names: true,
            clickable_path_max_chars: DEFAULT_CLICKABLE_PATH_CHARS,
            cursor_blink: false,
            cursor_shape: CursorShape::default(),
        }
    }
}

impl Config {
    /// Where the user's config lives.
    ///
    /// SonicTerm 1.0 uses a single cross-platform directory:
    /// `~/.sonicterm/sonicterm.toml`.
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
            // When: `path` already exists, preserve the user's config and only ensure companion directories/examples.
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
            // When: `path` is absent, use built-in defaults rather than treating first launch as a load error.
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
        let mut cfg: Self = toml::from_str(&text).with_context(|| format!("parse {path:?}"))?;
        cfg.window.normalize_padding();
        cfg.font.normalize();
        cfg.status_line.validate().with_context(|| format!("validate {path:?}"))?;
        Ok(cfg)
    }

    /// Infallible startup loader. Calls [`Self::load_strict`] and, on any
    /// error, logs a warning at `target = "sonicterm-cfg"` and falls back
    /// to [`Self::default`] so the app can still launch with a broken
    /// user config.
    pub fn load_or_default(path: &Path) -> Self {
        let mut warnings = Vec::new();
        let cfg = Self::load_or_default_collecting(path, &mut warnings);
        for warning in warnings {
            tracing::warn!(target: "sonicterm-cfg", "{warning}");
        }
        cfg
    }

    /// Same as [`Self::load_or_default`] but, instead of emitting
    /// `tracing::warn!` directly, appends any fallback message to
    /// `warnings`. Use this from `main` BEFORE `sonicterm_logging::init`
    /// has run — otherwise the parse-failure warn is dropped because no
    /// subscriber is installed yet.
    ///
    /// Callers MUST drain `warnings` after logging is initialised, e.g.
    /// ```ignore
    /// let mut cfg_warnings = Vec::new();
    /// let config = Config::load_or_default_collecting(&path, &mut cfg_warnings);
    /// let _g = sonicterm_logging::init(&config.logging.clone()).ok();
    /// for warning in cfg_warnings { tracing::warn!(target: "sonicterm-cfg", "{warning}"); }
    /// ```
    pub fn load_or_default_collecting(path: &Path, warnings: &mut Vec<String>) -> Self {
        // When: `load_strict(path)` errors, preserve startup with defaults and retain its diagnostic for later logging.
        match Self::load_strict(path) {
            Ok(cfg) => {
                warnings.extend(cfg.status_line.unknown_keys_warning());
                cfg
            }
            Err(error) => {
                warnings.push(format!(
                    "config TOML parse failed at {}: {error}; falling back to defaults",
                    path.display()
                ));
                Self::default()
            }
        }
    }

    /// Same as [`Self::load_or_default_collecting`], also returning the strict load's error, so
    /// startup can report its fallback without reading the file again.
    pub fn load_or_default_reporting(
        path: &Path,
        warnings: &mut Vec<String>,
    ) -> (Self, Option<anyhow::Error>) {
        // When: `load_strict(path)` errors, startup runs on defaults and keeps the error to show.
        match Self::load_strict(path) {
            Ok(cfg) => {
                warnings.extend(cfg.status_line.unknown_keys_warning());
                (cfg, None)
            }
            Err(error) => {
                warnings.push(format!(
                    "config TOML parse failed at {}: {error}; falling back to defaults",
                    path.display()
                ));
                (Self::default(), Some(error))
            }
        }
    }

    /// Whether config `text` names `[font] family`. Text that does not parse counts as naming one,
    /// so a broken file is never taken for a fresh install whose family may be chosen for it.
    pub fn sets_font_family(text: &str) -> bool {
        match text.parse::<toml::Table>() {
            Ok(table) => table
                .get("font")
                .and_then(toml::Value::as_table)
                .is_some_and(|font| font.contains_key("family")),
            Err(_) => true,
        }
    }

    /// Serialize the typed config to canonical TOML.
    ///
    /// Source comments, ordering, decoration, and unknown keys nested inside
    /// typed tables are not represented and are therefore omitted. Top-level
    /// unknown keys in `extra` remain serializable. Use the format-preserving
    /// persistence methods for edits to an existing user document.
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Rewrite `path` from the typed config, creating parent directories if needed.
    ///
    /// This intentionally writes canonical TOML and does not preserve source
    /// comments, ordering, decoration, or unknown keys nested inside typed
    /// tables. The same-directory temporary rename prevents partial
    /// process-written contents but does not claim power-loss durability.
    #[deprecated(
        since = "1.2.9",
        note = "rewrites the document and drops source-only TOML; use a format-preserving persistence method"
    )]
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
            .map(|name| name.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("sonicterm.toml"));
        let mut tmp_name = file_name;
        tmp_name.push(".tmp");
        tmp.set_file_name(tmp_name);
        std::fs::write(&tmp, toml).with_context(|| format!("write {tmp:?}"))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
        Ok(())
    }
}

/// Environment variable that replaces the whole config directory.
///
/// Diagnostic runs set it to a scratch copy of the config so they never edit
/// the user's files, while `HOME` stays unchanged for the shell inside the
/// terminal. Themes and keymaps resolve under it like under `~/.sonicterm`.
pub const CONFIG_DIR_ENV: &str = "SONICTERM_CONFIG_DIR";

/// SonicTerm 1.0 config directory (the on-disk layout): [`CONFIG_DIR_ENV`]
/// when set and non-empty, else `~/.sonicterm`.
pub fn default_config_dir() -> Option<PathBuf> {
    config_dir_with(|name| std::env::var_os(name))
}

/// Resolve the config directory from an injected environment lookup.
///
/// Separate from [`default_config_dir`] so tests cover each branch without
/// mutating the process environment.
fn config_dir_with(lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(dir) = lookup(CONFIG_DIR_ENV).filter(|value| !value.is_empty()) {
        // When: `CONFIG_DIR_ENV` names a directory, it replaces `~/.sonicterm` entirely.
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(lookup("HOME").or_else(|| lookup("USERPROFILE"))?).join(".sonicterm"))
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
        // When: `path` already exists, preserve the user's example override instead of reseeding it.
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {path:?}"))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;

#[path = "config_persist.rs"]
mod persist;
#[path = "config_status_save.rs"]
mod status_save;
#[path = "config_template.rs"]
mod template;

pub use persist::FamilySave;
#[cfg(test)]
use persist::LineEnding;
use status_save::patch_status_line_values;
pub use template::default_config_template;
