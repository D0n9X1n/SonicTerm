//! Status segment identifiers, metadata, and their stable catalog ordering.

use super::*;

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

#[cfg(test)]
#[path = "status_line_catalog_tests.rs"]
mod status_line_catalog_tests;
