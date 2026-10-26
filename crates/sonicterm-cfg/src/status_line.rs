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

#[path = "status_line_catalog.rs"]
mod status_line_catalog;

#[path = "status_line_resolve.rs"]
mod status_line_resolve;
