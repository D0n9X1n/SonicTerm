//! Graphical preferences subsystem (v0.6).
//!
//! Split into three concerns:
//!
//! - [`controls`] — pure widget structs (Toggle, Slider, Dropdown,
//!   ColorSwatch, TextField) with hit-testing and value get/set.
//! - [`layout`] — category sidebar + form-panel rectangles for the
//!   760×600 logical preferences window (min 680×520).
//! - [`state`] — an in-memory edit buffer that wraps the user [`Config`],
//!   tracks dirty state, and can apply (write TOML) or cancel.
//!
//! Rendering is intentionally left to the [`crate::render`] layer; this
//! module is pure data so it is trivially testable.

pub mod controls;
pub mod layout;
pub mod state;

pub use controls::{
    known_theme_preview, Button, ButtonAction, ButtonKind, ColorSwatch, Control, Dropdown,
    InteractionState, Slider, TextField, ThemePreviewSwatch, Toggle, WidgetId,
};
pub use layout::{Category, PrefsLayout, CATEGORIES};
pub use state::{PrefsHit, PrefsState};

/// Logical (DPI-independent) size of the preferences window.
pub const PREFS_WIN_W: f32 = 760.0;
pub const PREFS_WIN_H: f32 = 600.0;
/// Minimum logical size enforced by both the winit window builder
/// (`with_min_inner_size`) and [`layout::PrefsLayout::new`]. Must match
/// the clamp values inside `layout.rs`.
pub const PREFS_MIN_W: f32 = 680.0;
pub const PREFS_MIN_H: f32 = 520.0;
