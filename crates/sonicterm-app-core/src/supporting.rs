//! Pure-data payload types shared by `intent.rs` and `effect.rs`.
//!
//! These identifiers and boundary models intentionally avoid winit, wgpu,
//! clipboard, renderer, and native process handles. Rich live resources stay
//! in `sonicterm-app` and are addressed here through stable ids or snapshots.

use std::path::PathBuf;

// ── Logical-pixel geometry (boundary already converted from
// `winit::dpi::LogicalPosition` / `LogicalSize` by the platform shell).

/// Window-local logical position (CSS pixels, pre-DPI scale applied).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalPos {
    /// X coordinate in logical pixels.
    pub x: f64,
    /// Y coordinate in logical pixels.
    pub y: f64,
}

/// Logical-pixel size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalSize {
    /// Width in logical pixels.
    pub width: f64,
    /// Height in logical pixels.
    pub height: f64,
}

// ── Identifiers ─────────────────────────────────────────────────────

/// Opaque pane identifier. The state machine treats this as a primary
/// key only; concrete construction stays in `sonicterm-app`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u64);

/// Opaque tab identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

// ── Window-role / split-direction / mouse-button ────────────────────

/// What kind of top-level window the platform should create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRole {
    /// Primary user-facing window.
    Primary,
    /// Tear-out child window seeded from an existing tab.
    Child,
}

/// Direction for a pane split or focus move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDir {
    /// Split / focus left.
    Left,
    /// Split / focus right.
    Right,
    /// Split / focus up.
    Up,
    /// Split / focus down.
    Down,
}

/// Mouse button enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// Primary button (typically left).
    Left,
    /// Secondary button (typically right).
    Right,
    /// Middle button / wheel click.
    Middle,
    /// Other / extra button identified by index.
    Other(u16),
}

// ── Keyboard ────────────────────────────────────────────────────────

/// Opaque keyboard-key identifier. Real mapping (from
/// `winit::keyboard::KeyCode`) lives at the platform boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyCode(pub u32);

// ── Broadcast / palette / drag ──────────────────────────────────────

/// Scope of broadcast-input multiplexing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BroadcastScope {
    /// Broadcast disabled.
    #[default]
    Off,
    /// Broadcast to every pane in the current tab.
    CurrentTab,
    /// Broadcast to every pane in every tab.
    AllTabs,
    /// Broadcast to an explicit set of panes.
    Custom(Vec<PaneId>),
}

/// Command-palette selection payload carried across the backend-free boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteChoice {
    /// Unique identifier for the chosen command.
    pub id: String,
}

/// Winit-laundered outcome of an OS drag session (tab tear-out).
///
/// The platform layer translates `winit::WindowId` into `WindowKey`
/// at the boundary so the state machine stays winit-agnostic
/// (`sonicterm_app::window_key_boundary`).
#[derive(Clone, Debug, PartialEq)]
pub struct PendingDragOutcomeCore {
    /// Where the drag originated.
    pub src_window: sonicterm_types::WindowKey,
    /// Whether the drop was accepted by a target window/tab-bar.
    pub committed: bool,
}

// ── PTY-config façade ───────────────────────────────────────────────

/// Opaque config snapshot shipped on `Intent::ConfigChanged`. The real
/// shape is `sonicterm_cfg::Config`; introducing a direct dep on
/// `sonicterm-cfg` from `sonicterm-app-core` is deferred (sonicterm-cfg
/// transitively re-exports winit-adjacent types through the broader
/// façade — landing the dependency cleanly is its own work item).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtyConfig {
    /// Path the config was loaded from, if any.
    pub source: Option<PathBuf>,
    /// Raw TOML body the watcher observed.
    pub raw_toml: String,
}

// ── Menu model ──────────────────────────────────────────────────────

/// Pure-data description of the current application menubar. The
/// platform adapter (`sonicterm-mac` / `sonicterm-windows`) consumes
/// this on `AppEffect::MenubarUpdate` per spec §8.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuModel {
    /// Top-level menu entries in display order.
    pub items: Vec<MenuItem>,
}

/// Single backend-free menubar item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// Label as shown to the user.
    pub label: String,
    /// Stable dispatch tag matching the `__test_register` helper in
    /// `sonicterm-mac::menubar`.
    pub dispatch_tag: u32,
}
