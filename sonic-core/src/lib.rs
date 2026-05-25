//! sonic-core — terminal engine for Sonic Terminal.
//!
//! Modules:
//! - [`vt`]      — VT/ANSI parser built on top of the `vte` crate, with a
//!   semantic [`vt::Performer`] that mutates a [`grid::Grid`].
//! - [`grid`]    — terminal screen model: cells, attributes, scrollback.
//! - [`pty`]     — cross-platform pty spawning and IO.
//! - [`config`]  — TOML configuration with hot-reload.
//! - [`keymap`]  — keymap binding loader.
//! - [`theme`]   — color theme loader.
//!
//! The crate is platform-agnostic. Windowing and GPU rendering live in
//! `sonic-shared` and the platform bin crates.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod config;
pub mod grid;
pub mod hyperlink;
pub mod keymap;
pub mod pty;
pub mod theme;
pub mod url_open;
pub mod vt;

/// Re-exports of the most commonly used items.
pub mod prelude {
    pub use crate::{
        config::Config,
        grid::{Cell, Grid, Pos},
        hyperlink::{Hyperlink, HyperlinkId, HyperlinkRegistry},
        keymap::{Action, Keymap},
        pty::PtyHandle,
        theme::Theme,
        url_open,
        vt::Parser,
    };
}

/// Crate version, baked at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Internal helpers exposed only for the crate's own integration tests.
///
/// **Not part of the public API.** Items here may change or disappear in
/// any release without notice; downstream users must not depend on them.
/// Internal helpers exposed only for the crate's own integration tests.
///
/// **Not part of the public API.** Items here may change or disappear in
/// any release without notice; downstream users must not depend on them.
#[doc(hidden)]
pub mod __test_support {
    pub use crate::url_open::build_command;
    /// Set the scrollback limit on a [`crate::grid::Grid`]. Test-only.
    pub fn set_scrollback_limit(grid: &mut crate::grid::Grid, limit: usize) {
        grid.set_scrollback_limit(limit);
    }
}
