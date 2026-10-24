//! Status line configuration: the segment catalog, presets, and the
//! `[status_line]` table resolved into the lists the strip draws.
//!
//! This module is the config layer only. Formatting, fitting and drawing live
//! in `sonicterm-ui::status_line` and the renderer; nothing here depends on
//! them, so a customization resolver can use the catalog without the UI.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sonicterm_types::Action;

/// One status line segment in the catalog. Every id has a metadata row in
/// [`StatusSegmentId::meta`], even before its segment is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSegmentId {
    /// Other tabs that rang a bell, notified or finished while unseen.
    Alerts,
    /// Reload errors, software rendering and GPU recovery.
    Health,
    /// SonicTerm's own process memory.
    Mem,
    /// Copy mode, broadcast input and mouse capture.
    Mode,
    /// Input SonicTerm refused.
    Input,
    /// A program wrote the clipboard.
    Clip,
    /// Slow background path checks.
    PathCheck,
    /// What link or path detection found under the pointer.
    Link,
    /// How a paste will be delivered.
    Paste,
    /// The current selection and its copy result.
    Sel,
    /// Input-method composition state.
    Ime,
    /// How far the focused pane is scrolled back.
    Scroll,
    /// SonicTerm's CPU use.
    Cpu,
    /// Frame time and presenter.
    Frame,
    /// Output throughput of the focused pane.
    Out,
    /// Terminal state SonicTerm keeps for the focused tab.
    TabMem,
    /// Characters on screen that no font covers.
    Glyphs,
    /// Scrollback SonicTerm dropped for its budget.
    History,
    /// The font in use when it is not the configured one.
    Font,
    /// Font zoom away from the configured size.
    Zoom,
    /// The grid size, shown after a resize.
    Size,
}

/// Customization-checklist group of a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentGroup {
    /// The core segments every preset builds on.
    Core,
    /// What happens to keyboard and paste input.
    Input,
    /// What the pointer is over or has selected.
    Pointer,
    /// What other tabs did while unseen.
    OtherTabs,
    /// What a program reported through the terminal.
    ProgramReported,
    /// Resource and timing figures for investigation.
    Diagnostics,
    /// Fonts, glyphs, zoom and history display state.
    Display,
}

/// Which list a segment belongs to when enabled without an explicit list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusSide {
    /// The focused pane's facts.
    Left,
    /// Pointer context, centered in the window.
    Center,
    /// App and window facts.
    Right,
}

/// When a configured segment occupies the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowMode {
    /// Keep the slot and show idle text, or `n/a`, when there is no value.
    Always,
    /// Show only while the segment has something to say.
    Active,
}

/// Which parts of a segment `hide_sensitive` removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    /// Nothing in the segment is sensitive.
    None,
    /// The whole segment is sensitive and disappears.
    Whole,
    /// Only runs flagged sensitive disappear.
    Parts,
}

/// What survives longest when the strip is too narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Dropped in plain priority order.
    None,
    /// Always dropped after unprotected segments.
    Always,
    /// Protected only while the segment reports an error.
    Errors,
    /// Protected only while the broadcast chip is shown.
    BroadcastChip,
    /// The destination host is protected; the path shortens first.
    DestinationHost,
}

/// Catalog metadata of one segment id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusSegmentMeta {
    /// The segment this row describes.
    pub id: StatusSegmentId,
    /// Customization-checklist group.
    pub group: SegmentGroup,
    /// List used when the segment is enabled without an explicit list.
    pub home_side: StatusSide,
    /// Default narrow-strip priority, `0..=1000`; higher survives longer.
    pub default_priority: u16,
    /// `show` used when the config does not set one.
    pub default_show: ShowMode,
    /// Whether `show = "always"` is accepted (`active` always is).
    pub allows_always: bool,
    /// Whether the segment reacts to the pointer at all.
    pub clickable: bool,
    /// Which parts `hide_sensitive` removes.
    pub sensitivity: Sensitivity,
    /// What survives longest when the strip is narrow.
    pub protection: Protection,
    /// Whether this build formats the segment; presets list only these.
    pub implemented: bool,
    /// i18n key of the plain-words spoken name.
    pub spoken_name_key: &'static str,
}

impl StatusSegmentId {
    /// Every catalog id, in catalog (descending default priority) order.
    pub const ALL: [StatusSegmentId; 21] = [
        Self::Alerts,
        Self::Health,
        Self::Mem,
        Self::Mode,
        Self::Input,
        Self::Clip,
        Self::PathCheck,
        Self::Link,
        Self::Paste,
        Self::Sel,
        Self::Ime,
        Self::Scroll,
        Self::Cpu,
        Self::Frame,
        Self::Out,
        Self::TabMem,
        Self::Glyphs,
        Self::History,
        Self::Font,
        Self::Zoom,
        Self::Size,
    ];

    /// The config spelling of the id (`tab_mem`, `path_check`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alerts => "alerts",
            Self::Health => "health",
            Self::Mem => "mem",
            Self::Mode => "mode",
            Self::Input => "input",
            Self::Clip => "clip",
            Self::PathCheck => "path_check",
            Self::Link => "link",
            Self::Paste => "paste",
            Self::Sel => "sel",
            Self::Ime => "ime",
            Self::Scroll => "scroll",
            Self::Cpu => "cpu",
            Self::Frame => "frame",
            Self::Out => "out",
            Self::TabMem => "tab_mem",
            Self::Glyphs => "glyphs",
            Self::History => "history",
            Self::Font => "font",
            Self::Zoom => "zoom",
            Self::Size => "size",
        }
    }

    /// Parse a config spelling; `None` for a name outside the catalog.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == name)
    }

    /// The catalog row of this id. The `match` is exhaustive, so a new id
    /// does not compile until it has metadata.
    pub fn meta(self) -> StatusSegmentMeta {
        use Protection as Prot;
        use SegmentGroup as Group;
        use StatusSide::{Center, Left, Right};
        let (group, home_side, default_priority, protection, sensitivity, spoken_name_key) =
            match self {
                Self::Alerts => (
                    Group::OtherTabs,
                    Right,
                    990,
                    Prot::Always,
                    Sensitivity::None,
                    "status-spoken-alerts",
                ),
                Self::Health => (
                    Group::Core,
                    Right,
                    950,
                    Prot::Errors,
                    Sensitivity::None,
                    "status-spoken-health",
                ),
                Self::Mem => {
                    (Group::Core, Right, 900, Prot::None, Sensitivity::None, "status-spoken-mem")
                }
                Self::Mode => (
                    Group::Core,
                    Left,
                    800,
                    Prot::BroadcastChip,
                    Sensitivity::None,
                    "status-spoken-mode",
                ),
                Self::Input => {
                    (Group::Input, Left, 780, Prot::None, Sensitivity::None, "status-spoken-input")
                }
                Self::Clip => (
                    Group::ProgramReported,
                    Right,
                    760,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-clip",
                ),
                Self::PathCheck => (
                    Group::Diagnostics,
                    Right,
                    700,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-path-check",
                ),
                Self::Link => (
                    Group::Pointer,
                    Center,
                    600,
                    Prot::DestinationHost,
                    Sensitivity::Whole,
                    "status-spoken-link",
                ),
                Self::Paste => {
                    (Group::Input, Left, 560, Prot::None, Sensitivity::None, "status-spoken-paste")
                }
                Self::Sel => (
                    Group::Pointer,
                    Center,
                    520,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-sel",
                ),
                Self::Ime => {
                    (Group::Input, Left, 500, Prot::None, Sensitivity::None, "status-spoken-ime")
                }
                Self::Scroll => {
                    (Group::Core, Left, 400, Prot::None, Sensitivity::None, "status-spoken-scroll")
                }
                Self::Cpu => (
                    Group::Diagnostics,
                    Right,
                    380,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-cpu",
                ),
                Self::Frame => (
                    Group::Diagnostics,
                    Right,
                    360,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-frame",
                ),
                Self::Out => (
                    Group::Diagnostics,
                    Right,
                    340,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-out",
                ),
                Self::TabMem => (
                    Group::Diagnostics,
                    Right,
                    330,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-tab-mem",
                ),
                Self::Glyphs => (
                    Group::Display,
                    Right,
                    320,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-glyphs",
                ),
                Self::History => (
                    Group::Display,
                    Right,
                    300,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-history",
                ),
                Self::Font => (
                    Group::Display,
                    Right,
                    280,
                    Prot::None,
                    Sensitivity::None,
                    "status-spoken-font",
                ),
                Self::Zoom => {
                    (Group::Display, Left, 260, Prot::None, Sensitivity::None, "status-spoken-zoom")
                }
                Self::Size => {
                    (Group::Display, Right, 80, Prot::None, Sensitivity::None, "status-spoken-size")
                }
            };
        let default_show = match self {
            Self::Mem => ShowMode::Always,
            _ => ShowMode::Active,
        };
        StatusSegmentMeta {
            id: self,
            group,
            home_side,
            default_priority,
            default_show,
            // These have no idle fact worth a permanent slot (a static `14pt` or `120×40`), so
            // they are active-only.
            allows_always: !matches!(
                self,
                Self::Alerts
                    | Self::Clip
                    | Self::Input
                    | Self::Glyphs
                    | Self::PathCheck
                    | Self::TabMem
                    | Self::Link
                    | Self::Paste
                    | Self::Ime
                    | Self::History
                    | Self::Font
                    | Self::Size
                    | Self::Zoom
            ),
            // Paste, ime and size only explain state; a click has nothing to act on.
            clickable: !matches!(self, Self::Paste | Self::Ime | Self::Size | Self::Input),
            sensitivity,
            protection,
            implemented: matches!(
                self,
                Self::Alerts
                    | Self::Clip
                    | Self::Input
                    | Self::Glyphs
                    | Self::Cpu
                    | Self::Frame
                    | Self::Out
                    | Self::PathCheck
                    | Self::TabMem
                    | Self::Health
                    | Self::Mem
                    | Self::Mode
                    | Self::Scroll
                    | Self::Link
                    | Self::Sel
                    | Self::Paste
                    | Self::Ime
                    | Self::History
                    | Self::Font
                    | Self::Size
                    | Self::Zoom
            ),
            spoken_name_key,
        }
    }
}

/// A named starting point for the three lists and segment defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusPreset {
    /// Alerts, clipboard, health and memory only.
    Minimal,
    /// The default: pane modes on the left, pointer context in the center,
    /// app facts on the right.
    #[default]
    Developer,
    /// `developer` plus the diagnostics segments and the memory sparkline.
    Diagnostics,
}

impl StatusPreset {
    /// The preset's catalog lists, before filtering to implemented ids.
    fn catalog_lists(self) -> [&'static [StatusSegmentId]; 3] {
        use StatusSegmentId as Seg;
        const DEV_LEFT: &[Seg] = &[Seg::Mode, Seg::Font, Seg::Scroll, Seg::Input, Seg::Paste];
        const DEV_CENTER: &[Seg] = &[Seg::Link, Seg::Sel];
        const DEV_RIGHT: &[Seg] =
            &[Seg::Alerts, Seg::Clip, Seg::Health, Seg::Glyphs, Seg::History, Seg::Mem];
        const DIAG_RIGHT: &[Seg] = &[
            Seg::Alerts,
            Seg::Clip,
            Seg::Health,
            Seg::Glyphs,
            Seg::History,
            Seg::Cpu,
            Seg::Frame,
            Seg::Out,
            Seg::PathCheck,
            Seg::TabMem,
            Seg::Mem,
        ];
        match self {
            Self::Minimal => [&[], &[], &[Seg::Alerts, Seg::Clip, Seg::Health, Seg::Mem]],
            Self::Developer => [DEV_LEFT, DEV_CENTER, DEV_RIGHT],
            Self::Diagnostics => [DEV_LEFT, DEV_CENTER, DIAG_RIGHT],
        }
    }

    /// The preset's lists, keeping only ids this build implements, so a
    /// preset never lists a segment it cannot draw.
    pub fn lists(self) -> [Vec<StatusSegmentId>; 3] {
        self.catalog_lists()
            .map(|list| list.iter().copied().filter(|id| id.meta().implemented).collect())
    }
}

/// Symbol set for trend, scroll, warning and state icons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusIcons {
    /// Dependable Unicode symbols (`↑`, `⚠`, `⇶`).
    #[default]
    Unicode,
    /// Plain ASCII symbols (`^`, `!`, `>>`).
    Ascii,
    /// Nerd Font symbols, falling back to `unicode` per glyph.
    Nerd,
}

/// Which window edge the strip sits on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusPosition {
    /// The top content edge, above the panes and below any titlebar band.
    #[default]
    Top,
    /// The bottom content edge, between the panes and the tab bar.
    Bottom,
}

impl StatusPosition {
    /// The config spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

/// The strip's text size as a share of the live terminal font size: 0.3 to 1.0, 0.8 by default. A
/// configured value is kept exactly as written; only Larger and Smaller Text land on 0.05 steps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatusFontScale(f64);

// Every constructor refuses NaN, so equality on the held share is total.
impl Eq for StatusFontScale {}

impl StatusFontScale {
    /// The default, 0.8.
    pub const DEFAULT: Self = Self(0.8);
    /// The smallest scale, 0.3.
    pub const MIN: Self = Self(0.3);
    /// The largest scale, 1.0.
    pub const MAX: Self = Self(1.0);
    /// Larger and Smaller Text steps per unit of scale: one step is 0.05.
    const STEPS_PER_UNIT: f64 = 20.0;

    /// `value` exactly, or `None` outside 0.3..=1.0 or not a number.
    #[must_use]
    pub fn from_f64(value: f64) -> Option<Self> {
        (Self::MIN.0..=Self::MAX.0).contains(&value).then_some(Self(value))
    }

    /// The scale as a share, 0.3 to 1.0.
    #[must_use]
    pub fn as_f32(self) -> f32 {
        self.0 as f32
    }

    /// The scale as a share, exactly as configured or stepped, for writing back to the file.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        self.0
    }

    /// The scale `steps` steps of 0.05 larger (negative: smaller), clamped to 0.3..=1.0. The first
    /// step moves to the next 0.05 multiple in its direction, so 0.333 steps to 0.35 or 0.3.
    #[must_use]
    pub fn stepped(self, steps: i16) -> Self {
        // A share within this tolerance of a multiple is on it, so 0.8's float error is not a step.
        const ON_MULTIPLE: f64 = 1e-9;
        let units = self.0 * Self::STEPS_PER_UNIT;
        let start = match steps.signum() {
            1 => (units + ON_MULTIPLE).floor(),
            -1 => (units - ON_MULTIPLE).ceil(),
            _ => {
                // When: steps is zero, there is no direction to step in, so the scale stays.
                return self;
            }
        };
        let lowest = (Self::MIN.0 * Self::STEPS_PER_UNIT).round();
        let highest = (Self::MAX.0 * Self::STEPS_PER_UNIT).round();
        Self((start + f64::from(steps)).clamp(lowest, highest) / Self::STEPS_PER_UNIT)
    }
}

impl Default for StatusFontScale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What a click on a segment does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickBinding {
    /// The segment's registered default handler.
    Default,
    /// Inert to the pointer.
    None,
    /// A keymap action without arguments, run for the clicked window.
    Action(Action),
}

impl ClickBinding {
    /// Parse a `click` value: `default`, `none`, or a bare keymap action
    /// name. An unknown name or an action that takes arguments is an error.
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "default" => Ok(Self::Default),
            "none" => Ok(Self::None),
            _ => {
                // Neither keyword, `text` must be a unit keymap action; serde_plain
                // rejects both unknown names and variants that carry arguments.
                let action: Action = serde_plain::from_str(text).map_err(|_| {
                    anyhow::anyhow!("`{text}` is not a keymap action that takes no arguments")
                })?;
                Ok(Self::Action(action))
            }
        }
    }
}

/// Memory segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemSettings {
    /// Show `↑`/`↓` when the figure moved by at least 1%.
    pub show_trend: bool,
    /// Show the 8-cell, two-minute sparkline.
    pub sparkline: bool,
    /// Amber caution at or above this many MiB.
    pub warn_mib: u64,
    /// Red failure at or above this many MiB.
    pub critical_mib: u64,
}

impl Default for MemSettings {
    fn default() -> Self {
        Self { show_trend: true, sparkline: false, warn_mib: 1024, critical_mib: 2048 }
    }
}

/// Selection segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelSettings {
    /// Seconds a copy result (`copied` / `copy failed`) stays shown.
    pub copied_secs: u64,
}

impl Default for SelSettings {
    fn default() -> Self {
        Self { copied_secs: 2 }
    }
}

/// When the paste segment warns about unbracketed paste.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteWarning {
    /// Bracketed paste is off at a confirmed prompt on the primary screen.
    #[default]
    WhenOff,
    /// Never warn.
    Never,
}

/// Input-method segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImeSettings {
    /// Seconds `discarded (copy mode)` stays shown after a commit was dropped.
    pub hold_secs: u64,
}

impl Default for ImeSettings {
    fn default() -> Self {
        Self { hold_secs: 8 }
    }
}

/// Input segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputSettings {
    /// Seconds a refusal or write failure stays shown.
    pub hold_secs: u64,
    /// An accepted write still outstanding this many milliseconds after admission shows `held`.
    pub held_after_ms: u64,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self { hold_secs: 8, held_after_ms: 1500 }
    }
}

/// What 100% means for the `cpu` segment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CpuScale {
    /// `total` on Windows, `core` elsewhere, as each platform's own tool shows it.
    #[default]
    Auto,
    /// 100% is one core busy; the figure can exceed 100%.
    Core,
    /// 100% is every logical core busy.
    Total,
}

/// CPU segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CpuSettings {
    /// What 100% means.
    pub scale: CpuScale,
    /// The segment appears at this whole percent.
    pub min_percent: u64,
    /// It hides below `min_percent` times this, in thousandths (`hide_ratio` 0.7 is 700).
    pub hide_permille: u64,
    /// Consecutive samples below the hide level before it hides.
    pub hold_samples: u64,
}

impl Default for CpuSettings {
    fn default() -> Self {
        Self { scale: CpuScale::Auto, min_percent: 5, hide_permille: 700, hold_samples: 2 }
    }
}

/// Output-rate segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutSettings {
    /// Count only the focused pane (`scope = "pane"`), else every pane of the window.
    pub pane_scope: bool,
    /// The segment appears at this many bytes per second.
    pub min_bytes_per_sec: u64,
    /// It hides below `min_bytes_per_sec` times this, in thousandths.
    pub hide_permille: u64,
    /// Consecutive samples below the hide level before it hides.
    pub hold_samples: u64,
}

impl Default for OutSettings {
    fn default() -> Self {
        Self { pane_scope: true, min_bytes_per_sec: 1 << 20, hide_permille: 700, hold_samples: 2 }
    }
}

/// Path-check segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathCheckSettings {
    /// A check shows once it has run this many milliseconds.
    pub show_after_ms: u64,
    /// After every check ends, the display stays this many milliseconds.
    pub hold_ms: u64,
    /// A check running this many seconds shows the warning style.
    pub warn_after_secs: u64,
}

impl Default for PathCheckSettings {
    fn default() -> Self {
        Self { show_after_ms: 300, hold_ms: 1000, warn_after_secs: 10 }
    }
}

/// Glyphs segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphsSettings {
    /// The segment hides below this many missing codepoints.
    pub min_count: u64,
    /// Private Use Area codepoints (icon fonts such as Nerd Fonts) are not counted.
    pub ignore_private_use: bool,
}

impl Default for GlyphsSettings {
    fn default() -> Self {
        Self { min_count: 1, ignore_private_use: false }
    }
}

/// Clipboard segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClipSettings {
    /// Seconds the latest OSC 52 request stays shown.
    pub hold_secs: u64,
}

impl Default for ClipSettings {
    fn default() -> Self {
        Self { hold_secs: 10 }
    }
}

/// Alerts segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlertsSettings {
    /// A finished command marks its tab only after running at least this many seconds.
    pub min_secs: u64,
    /// A bell marks its tab.
    pub bell: bool,
    /// A program notification marks its tab.
    pub notify: bool,
    /// A finished command marks its tab.
    pub finished: bool,
}

impl Default for AlertsSettings {
    fn default() -> Self {
        Self { min_secs: 10, bell: true, notify: true, finished: true }
    }
}

/// Size segment settings beyond the common keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizeSettings {
    /// Milliseconds the grid size stays shown after the focused pane's grid was resized.
    pub hold_ms: u64,
}

impl Default for SizeSettings {
    fn default() -> Self {
        Self { hold_ms: 1500 }
    }
}

/// One configured segment after presets and overrides are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSegment {
    /// Which segment.
    pub id: StatusSegmentId,
    /// The list it is drawn in.
    pub side: StatusSide,
    /// When it occupies the strip.
    pub show: ShowMode,
    /// Narrow-strip priority, `0..=1000`.
    pub priority: u16,
    /// What a click does.
    pub click: ClickBinding,
}

/// The `[status_line.accessibility]` table as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusAccessibilityConfig {
    /// Expose the strip to assistive technology; adapters are installed regardless.
    pub expose: bool,
    /// Catalog ids allowed to announce.
    pub announce: Vec<StatusSegmentId>,
    /// At most one announcement per segment within this many milliseconds, `1000..=60000`.
    pub announce_min_interval_ms: u64,
    /// Unknown keys, kept and reported.
    #[serde(flatten, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

impl Default for StatusAccessibilityConfig {
    fn default() -> Self {
        Self {
            expose: true,
            announce: vec![StatusSegmentId::Health, StatusSegmentId::Alerts],
            announce_min_interval_ms: 5000,
            extra: toml::Table::new(),
        }
    }
}

impl StatusAccessibilityConfig {
    /// Whether the table holds only defaults, so a serialized config omits it.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `[status_line.accessibility]` after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccessibility {
    /// Expose the strip to assistive technology.
    pub expose: bool,
    /// Catalog ids allowed to announce.
    pub announce: Vec<StatusSegmentId>,
    /// Minimum milliseconds between two announcements of one segment.
    pub announce_min_interval_ms: u64,
}

/// The `[status_line]` table resolved into what the strip draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStatusLine {
    /// Configured visibility, before any session toggle.
    pub enabled: bool,
    /// Configured `hide_sensitive`, before any session toggle.
    pub hide_sensitive: bool,
    /// Symbol set.
    pub icons: StatusIcons,
    /// The window edge the strip sits on.
    pub position: StatusPosition,
    /// The strip's text size as a share of the live terminal font size.
    pub font_scale: StatusFontScale,
    /// Sampling interval of sampled segments, in milliseconds.
    pub update_interval_ms: u64,
    /// The preset the lists started from; a session enable returns a segment to its list.
    pub preset: StatusPreset,
    /// Every configured segment, left list first, each list in order.
    pub segments: Vec<ResolvedSegment>,
    /// Every implemented segment no list names, with its configured keys, in catalog order, so a
    /// segment the session enables keeps the `show`, `priority` and `click` the file gives it.
    pub unlisted: Vec<ResolvedSegment>,
    /// Memory segment settings.
    pub mem: MemSettings,
    /// Selection segment settings.
    pub sel: SelSettings,
    /// When the paste segment warns.
    pub paste: PasteWarning,
    /// Input-method segment settings.
    pub ime: ImeSettings,
    /// Size segment settings.
    pub size: SizeSettings,
    /// Alerts segment settings.
    pub alerts: AlertsSettings,
    /// Clipboard segment settings.
    pub clip: ClipSettings,
    /// Input segment settings.
    pub input: InputSettings,
    /// Glyphs segment settings.
    pub glyphs: GlyphsSettings,
    /// CPU segment settings.
    pub cpu: CpuSettings,
    /// Output-rate segment settings.
    pub out: OutSettings,
    /// Path-check segment settings.
    pub path_check: PathCheckSettings,
    /// What assistive technology is given.
    pub accessibility: ResolvedAccessibility,
}

impl ResolvedStatusLine {
    /// The configured segment `id`, if it is in any list.
    pub fn segment(&self, id: StatusSegmentId) -> Option<&ResolvedSegment> {
        self.segments.iter().find(|segment| segment.id == id)
    }
}

impl Default for ResolvedStatusLine {
    fn default() -> Self {
        // The default table always resolves; this is the developer preset.
        StatusLineConfig::default().resolve().expect("the default status line config resolves")
    }
}

/// Bounds of `update_interval_ms`.
pub const UPDATE_INTERVAL_RANGE_MS: std::ops::RangeInclusive<u64> = 500..=60_000;
/// Bounds of the memory thresholds, in MiB.
pub const MEM_THRESHOLD_RANGE_MIB: std::ops::RangeInclusive<u64> = 1..=1_048_576;
/// Bounds of the selection segment's `copied_secs`.
pub const COPIED_SECS_RANGE: std::ops::RangeInclusive<u64> = 1..=10;
/// Bounds of the input-method segment's `hold_secs`.
pub const HOLD_SECS_RANGE: std::ops::RangeInclusive<u64> = 1..=60;
/// Bounds of the input segment's `held_after_ms`.
pub const HELD_AFTER_MS_RANGE: std::ops::RangeInclusive<u64> = 250..=60_000;
/// Bounds of the glyphs segment's `min_count`.
pub const GLYPHS_MIN_COUNT_RANGE: std::ops::RangeInclusive<u64> = 1..=1000;
/// Bounds of the clipboard segment's `hold_secs`.
pub const CLIP_HOLD_SECS_RANGE: std::ops::RangeInclusive<u64> = 1..=600;
/// Bounds of the alerts segment's `min_secs`.
pub const MIN_SECS_RANGE: std::ops::RangeInclusive<u64> = 0..=86_400;
/// Bounds of the size segment's `hold_ms`.
pub const HOLD_MS_RANGE: std::ops::RangeInclusive<u64> = 250..=10_000;
/// Highest accepted segment priority.
pub const MAX_PRIORITY: u16 = 1000;
/// Allowed `[status_line.accessibility] announce_min_interval_ms`, in milliseconds.
pub const ANNOUNCE_MIN_INTERVAL_RANGE_MS: std::ops::RangeInclusive<u64> = 1000..=60_000;

/// Keys every segment table accepts.
const COMMON_KEYS: &[&str] = &["show", "priority", "click"];
/// Extra keys of the memory segment table.
const MEM_KEYS: &[&str] = &["show_trend", "sparkline", "warn_mib", "critical_mib"];
/// Extra keys of the selection segment table.
const SEL_KEYS: &[&str] = &["copied_secs"];
/// Extra keys of the paste segment table.
const PASTE_KEYS: &[&str] = &["paste"];
/// Extra keys of the input-method segment table.
const IME_KEYS: &[&str] = &["hold_secs"];
/// Extra keys of the input segment table.
const INPUT_KEYS: &[&str] = &["hold_secs", "held_after_ms"];
/// Extra keys of the CPU segment table.
const CPU_KEYS: &[&str] = &["scale", "min_percent", "hide_ratio", "hold_samples"];
/// Extra keys of the output-rate segment table.
const OUT_KEYS: &[&str] = &["scope", "min_bytes_per_sec", "hide_ratio", "hold_samples"];
/// Extra keys of the path-check segment table.
const PATH_CHECK_KEYS: &[&str] = &["show_after_ms", "hold_ms", "warn_after_secs"];
/// Extra keys of the glyphs segment table.
const GLYPHS_KEYS: &[&str] = &["min_count", "ignore_private_use"];
/// Extra keys of the clipboard segment table.
const CLIP_KEYS: &[&str] = &["hold_secs"];
/// Extra keys of the alerts segment table.
const ALERTS_KEYS: &[&str] = &["min_secs", "sources"];
/// Extra keys of the size segment table.
const SIZE_KEYS: &[&str] = &["hold_ms"];

/// Keys a segment table accepts beyond [`COMMON_KEYS`].
fn segment_keys(id: StatusSegmentId) -> &'static [&'static str] {
    match id {
        StatusSegmentId::Mem => MEM_KEYS,
        StatusSegmentId::Sel => SEL_KEYS,
        StatusSegmentId::Paste => PASTE_KEYS,
        StatusSegmentId::Ime => IME_KEYS,
        StatusSegmentId::Size => SIZE_KEYS,
        StatusSegmentId::Alerts => ALERTS_KEYS,
        StatusSegmentId::Clip => CLIP_KEYS,
        StatusSegmentId::Input => INPUT_KEYS,
        StatusSegmentId::Glyphs => GLYPHS_KEYS,
        StatusSegmentId::Cpu => CPU_KEYS,
        StatusSegmentId::Out => OUT_KEYS,
        StatusSegmentId::PathCheck => PATH_CHECK_KEYS,
        _ => &[],
    }
}

/// The `[status_line]` table as written in `sonicterm.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineConfig {
    /// Show the strip. Off by default.
    pub enabled: bool,
    /// Lists and segment defaults to start from.
    pub preset: StatusPreset,
    /// Replaces the preset's left list when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Vec<StatusSegmentId>>,
    /// Replaces the preset's center list when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<Vec<StatusSegmentId>>,
    /// Replaces the preset's right list when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<Vec<StatusSegmentId>>,
    /// Symbol set.
    pub icons: StatusIcons,
    /// The window edge the strip sits on; an unknown value fails the load.
    pub position: StatusPosition,
    /// The strip's text size as a share of the live terminal font size, `0.3..=1.0`.
    pub font_scale: f64,
    /// Hide every sensitive segment or part (screen sharing).
    pub hide_sensitive: bool,
    /// Sampling interval of sampled segments, `500..=60000` ms.
    pub update_interval_ms: u64,
    /// `[status_line.segment.<id>]` tables, kept raw so unknown keys and
    /// ids survive and can be reported instead of failing the load.
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    pub segment: toml::Table,
    /// `[status_line.accessibility]`: exposure and announcements.
    #[serde(skip_serializing_if = "StatusAccessibilityConfig::is_default")]
    pub accessibility: StatusAccessibilityConfig,
    /// Unknown keys directly under `[status_line]`, kept and reported.
    #[serde(flatten, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: StatusPreset::Developer,
            left: None,
            center: None,
            right: None,
            icons: StatusIcons::Unicode,
            position: StatusPosition::Top,
            font_scale: StatusFontScale::DEFAULT.as_f64(),
            hide_sensitive: false,
            update_interval_ms: 2000,
            segment: toml::Table::new(),
            accessibility: StatusAccessibilityConfig::default(),
            extra: toml::Table::new(),
        }
    }
}

impl StatusLineConfig {
    /// Check the table without keeping the result; the load path calls this
    /// so an invalid `[status_line]` fails the whole config.
    pub fn validate(&self) -> Result<()> {
        self.resolve().map(|_| ())
    }

    /// Apply the preset, explicit lists and segment tables, validating every
    /// value. Unknown keys are ignored here and listed by [`Self::unknown_keys`].
    pub fn resolve(&self) -> Result<ResolvedStatusLine> {
        if !UPDATE_INTERVAL_RANGE_MS.contains(&self.update_interval_ms) {
            // `update_interval_ms` is outside 500..=60000, sampling cadence is unusable.
            bail!(
                "status_line.update_interval_ms must be in 500..=60000, got {}",
                self.update_interval_ms
            );
        }
        let Some(font_scale) = StatusFontScale::from_f64(self.font_scale) else {
            // `font_scale` is outside 0.3..=1.0 or not a number, so no strip size follows from it.
            bail!("status_line.font_scale must be in 0.3..=1.0, got {}", self.font_scale);
        };
        let [preset_left, preset_center, preset_right] = self.preset.lists();
        let lists = [
            (StatusSide::Left, "left", self.left.clone().unwrap_or(preset_left)),
            (StatusSide::Center, "center", self.center.clone().unwrap_or(preset_center)),
            (StatusSide::Right, "right", self.right.clone().unwrap_or(preset_right)),
        ];
        let mut mem = MemSettings {
            sparkline: self.preset == StatusPreset::Diagnostics,
            ..MemSettings::default()
        };
        let mut sel = SelSettings::default();
        let mut paste = PasteWarning::default();
        let mut ime = ImeSettings::default();
        let mut size = SizeSettings::default();
        let mut alerts = AlertsSettings::default();
        let mut clip = ClipSettings::default();
        let mut input = InputSettings::default();
        let mut glyphs = GlyphsSettings::default();
        let mut cpu = CpuSettings::default();
        let mut out = OutSettings::default();
        let mut path_check = PathCheckSettings::default();
        let mut segments = Vec::new();
        for (side, list_name, ids) in lists {
            for id in ids {
                if segments.iter().any(|segment: &ResolvedSegment| segment.id == id) {
                    // `id` already sits in a list, a second slot would draw it twice.
                    bail!(
                        "status_line.{list_name}: `{}` appears in more than one place",
                        id.as_str()
                    );
                }
                if !id.meta().implemented {
                    // The build has no formatter for `id`, the list could not draw it.
                    bail!(
                        "status_line.{list_name}: `{}` is not available in this build",
                        id.as_str()
                    );
                }
                segments.push(ResolvedSegment {
                    id,
                    side,
                    show: id.meta().default_show,
                    priority: id.meta().default_priority,
                    click: ClickBinding::Default,
                });
            }
        }
        let mut unlisted: Vec<ResolvedSegment> = StatusSegmentId::ALL
            .into_iter()
            .filter(|id| id.meta().implemented)
            .filter(|id| !segments.iter().any(|segment| segment.id == *id))
            .map(|id| ResolvedSegment {
                id,
                side: id.meta().home_side,
                show: id.meta().default_show,
                priority: id.meta().default_priority,
                click: ClickBinding::Default,
            })
            .collect();
        for (key, value) in &self.segment {
            let Some(id) = StatusSegmentId::from_name(key) else {
                // When: `key` is not a catalog id, it is an unknown key of `segment`.
                continue;
            };
            let Some(table) = value.as_table() else {
                // `status_line.segment.<id>` is not a table, nothing in it can apply.
                bail!("status_line.segment.{key} must be a table");
            };
            let meta = id.meta();
            let target =
                segments.iter_mut().chain(unlisted.iter_mut()).find(|segment| segment.id == id);
            let mut scratch = ResolvedSegment {
                id,
                side: meta.home_side,
                show: meta.default_show,
                priority: meta.default_priority,
                click: ClickBinding::Default,
            };
            // A table for an unimplemented segment is still validated, so a typo is caught; an
            // unlisted implemented one keeps its keys for a session enable.
            let resolved = target.unwrap_or(&mut scratch);
            apply_common_keys(resolved, &meta, table)
                .with_context(|| format!("status_line.segment.{key}"))?;
            if id == StatusSegmentId::Mem {
                apply_mem_keys(&mut mem, table).context("status_line.segment.mem")?;
            }
            if id == StatusSegmentId::Sel {
                apply_sel_keys(&mut sel, table).context("status_line.segment.sel")?;
            }
            if id == StatusSegmentId::Paste {
                if let Some(value) = table.get("paste") {
                    paste = value.clone().try_into().context(
                        "status_line.segment.paste: paste must be \"when_off\" or \"never\"",
                    )?;
                }
            }
            if id == StatusSegmentId::Ime {
                if let Some(value) = table.get("hold_secs") {
                    ime.hold_secs = value
                        .as_integer()
                        .and_then(|secs| u64::try_from(secs).ok())
                        .filter(|secs| HOLD_SECS_RANGE.contains(secs))
                        .context(
                            "status_line.segment.ime: hold_secs must be an integer in 1..=60",
                        )?;
                }
            }
            if id == StatusSegmentId::Input {
                apply_input_keys(&mut input, table).context("status_line.segment.input")?;
            }
            if id == StatusSegmentId::Glyphs {
                apply_glyphs_keys(&mut glyphs, table).context("status_line.segment.glyphs")?;
            }
            if id == StatusSegmentId::Cpu {
                apply_cpu_keys(&mut cpu, table).context("status_line.segment.cpu")?;
            }
            if id == StatusSegmentId::Out {
                apply_out_keys(&mut out, table).context("status_line.segment.out")?;
            }
            if id == StatusSegmentId::PathCheck {
                apply_path_check_keys(&mut path_check, table)
                    .context("status_line.segment.path_check")?;
            }
            if id == StatusSegmentId::Clip {
                if let Some(value) = table.get("hold_secs") {
                    clip.hold_secs = value
                        .as_integer()
                        .and_then(|secs| u64::try_from(secs).ok())
                        .filter(|secs| CLIP_HOLD_SECS_RANGE.contains(secs))
                        .context(
                            "status_line.segment.clip: hold_secs must be an integer in 1..=600",
                        )?;
                }
            }
            if id == StatusSegmentId::Alerts {
                apply_alerts_keys(&mut alerts, table).context("status_line.segment.alerts")?;
            }
            if id == StatusSegmentId::Size {
                if let Some(value) = table.get("hold_ms") {
                    size.hold_ms = value
                        .as_integer()
                        .and_then(|millis| u64::try_from(millis).ok())
                        .filter(|millis| HOLD_MS_RANGE.contains(millis))
                        .context(
                            "status_line.segment.size: hold_ms must be an integer in 250..=10000",
                        )?;
                }
            }
        }
        let interval = self.accessibility.announce_min_interval_ms;
        if !ANNOUNCE_MIN_INTERVAL_RANGE_MS.contains(&interval) {
            // The interval is outside 1000..=60000, so announcements would flood or never repeat.
            bail!(
                "status_line.accessibility.announce_min_interval_ms must be in 1000..=60000, got {interval}"
            );
        }
        if mem.critical_mib < mem.warn_mib {
            // `critical_mib < warn_mib`, the red level would sit below the amber one.
            bail!(
                "status_line.segment.mem.critical_mib ({}) must be >= warn_mib ({})",
                mem.critical_mib,
                mem.warn_mib
            );
        }
        Ok(ResolvedStatusLine {
            enabled: self.enabled,
            hide_sensitive: self.hide_sensitive,
            icons: self.icons,
            position: self.position,
            font_scale,
            update_interval_ms: self.update_interval_ms,
            preset: self.preset,
            segments,
            unlisted,
            mem,
            sel,
            paste,
            ime,
            size,
            alerts,
            clip,
            input,
            glyphs,
            cpu,
            out,
            path_check,
            accessibility: ResolvedAccessibility {
                expose: self.accessibility.expose,
                announce: self.accessibility.announce.clone(),
                announce_min_interval_ms: interval,
            },
        })
    }

    /// Dotted paths of every key the status line ignores: unknown keys under
    /// `[status_line]`, unknown ids under `segment`, and unknown keys inside a
    /// segment table.
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut unknown: Vec<String> =
            self.extra.keys().map(|key| format!("status_line.{key}")).collect();
        unknown.extend(
            self.accessibility.extra.keys().map(|key| format!("status_line.accessibility.{key}")),
        );
        for (key, value) in &self.segment {
            let Some(id) = StatusSegmentId::from_name(key) else {
                // When: `key` names no catalog id, the whole table is unknown.
                unknown.push(format!("status_line.segment.{key}"));
                continue;
            };
            let Some(table) = value.as_table() else {
                // When: not a table, `resolve` reports it as an error instead.
                continue;
            };
            for field in table.keys() {
                let known = COMMON_KEYS.contains(&field.as_str())
                    || segment_keys(id).contains(&field.as_str());
                if !known {
                    unknown.push(format!("status_line.segment.{key}.{field}"));
                }
            }
        }
        unknown
    }

    /// One warning naming every ignored key, or `None` when there are none.
    pub fn unknown_keys_warning(&self) -> Option<String> {
        let unknown = self.unknown_keys();
        if unknown.is_empty() {
            // When: `unknown` is empty, every key is known and there is nothing to warn about.
            return None;
        }
        Some(format!("status line config ignores unknown keys: {}", unknown.join(", ")))
    }
}

/// Apply `show`, `priority` and `click` from a segment table.
fn apply_common_keys(
    segment: &mut ResolvedSegment,
    meta: &StatusSegmentMeta,
    table: &toml::Table,
) -> Result<()> {
    if let Some(value) = table.get("show") {
        let show: ShowMode =
            value.clone().try_into().context("show must be \"always\" or \"active\"")?;
        if show == ShowMode::Always && !meta.allows_always {
            // `show = "always"` on an active-only segment, there is no idle text to keep.
            bail!("show = \"always\" is not supported; use \"active\"");
        }
        segment.show = show;
    }
    if let Some(value) = table.get("priority") {
        let priority = value
            .as_integer()
            .filter(|priority| (0..=i64::from(MAX_PRIORITY)).contains(priority))
            .context("priority must be an integer in 0..=1000")?;
        segment.priority = u16::try_from(priority).context("priority out of range")?;
    }
    if let Some(value) = table.get("click") {
        let text = value.as_str().context("click must be a string")?;
        let click = ClickBinding::parse(text).context("invalid click")?;
        if !meta.clickable && matches!(click, ClickBinding::Action(_)) {
            // The segment is not clickable, an action could never run.
            bail!("click actions are not supported on this segment");
        }
        segment.click = click;
    }
    Ok(())
}

/// Apply the memory segment's own keys.
fn apply_mem_keys(mem: &mut MemSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("show_trend") {
        mem.show_trend = value.as_bool().context("show_trend must be a boolean")?;
    }
    if let Some(value) = table.get("sparkline") {
        mem.sparkline = value.as_bool().context("sparkline must be a boolean")?;
    }
    for (key, slot) in [("warn_mib", &mut mem.warn_mib), ("critical_mib", &mut mem.critical_mib)] {
        if let Some(value) = table.get(key) {
            let mib = value
                .as_integer()
                .and_then(|mib| u64::try_from(mib).ok())
                .filter(|mib| MEM_THRESHOLD_RANGE_MIB.contains(mib))
                .with_context(|| format!("{key} must be an integer in 1..=1048576"))?;
            *slot = mib;
        }
    }
    Ok(())
}

/// Apply the input segment's own keys.
fn apply_input_keys(input: &mut InputSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("hold_secs") {
        input.hold_secs = value
            .as_integer()
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|secs| CLIP_HOLD_SECS_RANGE.contains(secs))
            .context("hold_secs must be an integer in 1..=600")?;
    }
    if let Some(value) = table.get("held_after_ms") {
        input.held_after_ms = value
            .as_integer()
            .and_then(|millis| u64::try_from(millis).ok())
            .filter(|millis| HELD_AFTER_MS_RANGE.contains(millis))
            .context("held_after_ms must be an integer in 250..=60000")?;
    }
    Ok(())
}

/// Read `key` from `table` as an integer within `range`, if present.
fn integer_key(
    table: &toml::Table,
    key: &str,
    range: std::ops::RangeInclusive<u64>,
) -> Result<Option<u64>> {
    let Some(value) = table.get(key) else {
        // When: table has no `key`, the default stays.
        return Ok(None);
    };
    let integer = value
        .as_integer()
        .and_then(|integer| u64::try_from(integer).ok())
        .filter(|integer| range.contains(integer))
        .with_context(|| {
            format!("{key} must be an integer in {}..={}", range.start(), range.end())
        })?;
    Ok(Some(integer))
}

/// Read `hide_ratio` (0.1..=1.0) from `table` as thousandths, if present.
fn hide_ratio_key(table: &toml::Table) -> Result<Option<u64>> {
    let Some(value) = table.get("hide_ratio") else {
        // When: table has no hide_ratio, the default stays.
        return Ok(None);
    };
    let ratio = value
        .as_float()
        .or_else(|| value.as_integer().map(|integer| integer as f64))
        .filter(|ratio| (0.1..=1.0).contains(ratio))
        .context("hide_ratio must be a number in 0.1..=1.0")?;
    Ok(Some((ratio * 1000.0).round() as u64))
}

/// Apply the CPU segment's own keys.
fn apply_cpu_keys(cpu: &mut CpuSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("scale") {
        cpu.scale = match value.as_str() {
            Some("auto") => CpuScale::Auto,
            Some("core") => CpuScale::Core,
            Some("total") => CpuScale::Total,
            _ => anyhow::bail!("scale must be \"auto\", \"core\" or \"total\""),
        };
    }
    if let Some(percent) = integer_key(table, "min_percent", 0..=1000)? {
        cpu.min_percent = percent;
    }
    if let Some(permille) = hide_ratio_key(table)? {
        cpu.hide_permille = permille;
    }
    if let Some(samples) = integer_key(table, "hold_samples", 0..=10)? {
        cpu.hold_samples = samples;
    }
    Ok(())
}

/// Apply the output-rate segment's own keys.
fn apply_out_keys(out: &mut OutSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("scope") {
        out.pane_scope = match value.as_str() {
            Some("pane") => true,
            Some("window") => false,
            _ => anyhow::bail!("scope must be \"pane\" or \"window\""),
        };
    }
    if let Some(rate) = integer_key(table, "min_bytes_per_sec", 1024..=1 << 30)? {
        out.min_bytes_per_sec = rate;
    }
    if let Some(permille) = hide_ratio_key(table)? {
        out.hide_permille = permille;
    }
    if let Some(samples) = integer_key(table, "hold_samples", 0..=10)? {
        out.hold_samples = samples;
    }
    Ok(())
}

/// Apply the path-check segment's own keys.
fn apply_path_check_keys(path_check: &mut PathCheckSettings, table: &toml::Table) -> Result<()> {
    if let Some(millis) = integer_key(table, "show_after_ms", 100..=5000)? {
        path_check.show_after_ms = millis;
    }
    if let Some(millis) = integer_key(table, "hold_ms", 0..=10_000)? {
        path_check.hold_ms = millis;
    }
    if let Some(secs) = integer_key(table, "warn_after_secs", 1..=600)? {
        path_check.warn_after_secs = secs;
    }
    Ok(())
}

/// Apply the glyphs segment's own keys.
fn apply_glyphs_keys(glyphs: &mut GlyphsSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("min_count") {
        glyphs.min_count = value
            .as_integer()
            .and_then(|count| u64::try_from(count).ok())
            .filter(|count| GLYPHS_MIN_COUNT_RANGE.contains(count))
            .context("min_count must be an integer in 1..=1000")?;
    }
    if let Some(value) = table.get("ignore_private_use") {
        glyphs.ignore_private_use =
            value.as_bool().context("ignore_private_use must be true or false")?;
    }
    Ok(())
}

/// Apply the alerts segment's own keys.
fn apply_alerts_keys(alerts: &mut AlertsSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("min_secs") {
        alerts.min_secs = value
            .as_integer()
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|secs| MIN_SECS_RANGE.contains(secs))
            .context("min_secs must be an integer in 0..=86400")?;
    }
    if let Some(value) = table.get("sources") {
        let sources = value.as_array().context("sources must be a list")?;
        let (mut bell, mut notify, mut finished) = (false, false, false);
        for source in sources {
            match source.as_str() {
                Some("bell") => bell = true,
                Some("notify") => notify = true,
                Some("finished") => finished = true,
                // An unknown source fails the load rather than silently marking nothing.
                _ => bail!("sources entries must be \"bell\", \"notify\" or \"finished\""),
            }
        }
        (alerts.bell, alerts.notify, alerts.finished) = (bell, notify, finished);
    }
    Ok(())
}

/// Apply the selection segment's own keys.
fn apply_sel_keys(sel: &mut SelSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("copied_secs") {
        sel.copied_secs = value
            .as_integer()
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|secs| COPIED_SECS_RANGE.contains(secs))
            .context("copied_secs must be an integer in 1..=10")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod status_line_tests;
