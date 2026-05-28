//! sonic-core — **deprecated façade** for back-compat with pre-PR-3 imports.
//!
//! The original `sonic-core` crate has been decomposed into four leaf crates:
//!
//! - [`sonic_vt`]   — VT/ANSI parser (`vt::Parser`, `vt::Performer`, …)
//! - [`sonic_grid`] — terminal grid + hyperlink registry
//! - [`sonic_cfg`]  — config / theme / keymap / url_open loaders
//! - [`sonic_io`]   — PTY + process probes + optional SSH backend
//!
//! This crate re-exports each leaf as a module so existing imports of the
//! form `use sonic_core::vt::Parser;` and `use sonic_core::grid::Grid;`
//! continue to compile unchanged. New code should depend on the leaf crates
//! directly.

#![forbid(unsafe_op_in_unsafe_fn)]

// Module-shaped re-exports — preserve `sonic_core::vt::...`,
// `sonic_core::grid::...`, etc.
pub use sonic_cfg::config;
pub use sonic_cfg::keymap;
pub use sonic_cfg::theme;
pub use sonic_cfg::url_open;
pub use sonic_cfg::url_scan;
pub use sonic_grid::grid;
pub use sonic_grid::hyperlink;
pub use sonic_io::proc_info;
pub use sonic_io::pty;
pub use sonic_io::ssh;
pub use sonic_vt::vt;

#[cfg(windows)]
pub use sonic_io::foreground_proc;

// `glyph_key` historically lived in sonic-core; it's a thin wrapper that
// just re-exports `sonic_types::GlyphKey`. Keep it as a sub-module here so
// `sonic_core::glyph_key::GlyphKey` resolves.
pub mod glyph_key;

/// Re-exports of the most commonly used items.
pub mod prelude {
    pub use crate::{
        config::Config,
        glyph_key::GlyphKey,
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
