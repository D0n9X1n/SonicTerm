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
                    (Group::Input, Left, 500, Prot::None, Sensitivity::Parts, "status-spoken-ime")
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
            // The link segment has no idle fact to show, so it is active-only.
            allows_always: self != Self::Link,
            clickable: true,
            sensitivity,
            protection,
            implemented: matches!(
                self,
                Self::Health | Self::Mem | Self::Mode | Self::Scroll | Self::Link
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
        const DEV_LEFT: &[Seg] = &[Seg::Mode, Seg::Scroll, Seg::Input, Seg::Paste];
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

/// The `[status_line]` table resolved into what the strip draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStatusLine {
    /// Configured visibility, before any session toggle.
    pub enabled: bool,
    /// Configured `hide_sensitive`, before any session toggle.
    pub hide_sensitive: bool,
    /// Symbol set.
    pub icons: StatusIcons,
    /// Sampling interval of sampled segments, in milliseconds.
    pub update_interval_ms: u64,
    /// Every configured segment, left list first, each list in order.
    pub segments: Vec<ResolvedSegment>,
    /// Memory segment settings.
    pub mem: MemSettings,
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
/// Highest accepted segment priority.
pub const MAX_PRIORITY: u16 = 1000;

/// Keys every segment table accepts.
const COMMON_KEYS: &[&str] = &["show", "priority", "click"];
/// Extra keys of the memory segment table.
const MEM_KEYS: &[&str] = &["show_trend", "sparkline", "warn_mib", "critical_mib"];

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
    /// Hide every sensitive segment or part (screen sharing).
    pub hide_sensitive: bool,
    /// Sampling interval of sampled segments, `500..=60000` ms.
    pub update_interval_ms: u64,
    /// `[status_line.segment.<id>]` tables, kept raw so unknown keys and
    /// ids survive and can be reported instead of failing the load.
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    pub segment: toml::Table,
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
            hide_sensitive: false,
            update_interval_ms: 2000,
            segment: toml::Table::new(),
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
            let target = segments.iter_mut().find(|segment| segment.id == id);
            let mut scratch = ResolvedSegment {
                id,
                side: meta.home_side,
                show: meta.default_show,
                priority: meta.default_priority,
                click: ClickBinding::Default,
            };
            // A table for an unlisted segment is still validated, so a typo
            // is caught before the segment is added to a list.
            let resolved = target.unwrap_or(&mut scratch);
            apply_common_keys(resolved, &meta, table)
                .with_context(|| format!("status_line.segment.{key}"))?;
            if id == StatusSegmentId::Mem {
                apply_mem_keys(&mut mem, table).context("status_line.segment.mem")?;
            }
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
            update_interval_ms: self.update_interval_ms,
            segments,
            mem,
        })
    }

    /// Dotted paths of every key the status line ignores: unknown keys under
    /// `[status_line]`, unknown ids under `segment`, and unknown keys inside a
    /// segment table.
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut unknown: Vec<String> =
            self.extra.keys().map(|key| format!("status_line.{key}")).collect();
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
                    || (id == StatusSegmentId::Mem && MEM_KEYS.contains(&field.as_str()));
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

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod status_line_tests;
