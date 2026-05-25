//! Graphical preferences subsystem (v0.6).
//!
//! Split into three concerns:
//!
//! - [`controls`] — pure widget structs (Toggle, Slider, Dropdown,
//!   ColorSwatch, TextField) with hit-testing and value get/set.
//! - [`layout`] — category sidebar + form-panel rectangles for the
//!   ~720x560 logical preferences window.
//! - [`state`] — an in-memory edit buffer that wraps the user [`Config`],
//!   tracks dirty state, and can apply (write TOML) or cancel.
//!
//! Rendering is intentionally left to the [`crate::render`] layer; this
//! module is pure data so it is trivially testable.

pub mod controls;
pub mod layout;
pub mod state;

pub use controls::{ColorSwatch, Control, Dropdown, Slider, TextField, Toggle, WidgetId};
pub use layout::{Category, PrefsLayout, CATEGORIES};
pub use state::{PrefsHit, PrefsState};

/// Logical (DPI-independent) size of the preferences window.
pub const PREFS_WIN_W: f32 = 720.0;
pub const PREFS_WIN_H: f32 = 560.0;
