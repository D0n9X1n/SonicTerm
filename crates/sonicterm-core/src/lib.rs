//! sonicterm-core — **deprecated façade** for back-compat with pre-PR-3 imports.
//!
//! The original `sonicterm-core` crate has been decomposed into four leaf crates:
//!
//! - [`sonicterm_vt`]   — VT/ANSI parser (`vt::Parser`, `vt::Performer`, …)
//! - [`sonicterm_grid`] — terminal grid + hyperlink registry
//! - [`sonicterm_cfg`]  — config / theme / keymap / url_open loaders
//! - [`sonicterm_io`]   — PTY + process probes + optional SSH backend
//!
//! This crate re-exports each leaf as a module so existing imports of the
//! form `use sonicterm_core::vt::Parser;` and `use sonicterm_core::grid::Grid;`
//! continue to compile unchanged. New code should depend on the leaf crates
//! directly.

#![forbid(unsafe_op_in_unsafe_fn)]

// Module-shaped re-exports — preserve `sonicterm_core::vt::...`,
// `sonicterm_core::grid::...`, etc.
pub use sonicterm_cfg::config;
pub use sonicterm_cfg::keymap;
pub use sonicterm_cfg::theme;
pub use sonicterm_cfg::url_open;
pub use sonicterm_cfg::url_scan;
pub use sonicterm_grid::grid;
pub use sonicterm_grid::hyperlink;
pub use sonicterm_io::proc_info;
pub use sonicterm_io::pty;
pub use sonicterm_io::ssh;
pub use sonicterm_vt::vt;

#[cfg(windows)]
pub use sonicterm_io::foreground_proc;

// `glyph_key` historically lived in sonicterm-core; it's a thin wrapper that
// just re-exports `sonicterm_types::GlyphKey`. Keep it as a sub-module here so
// `sonicterm_core::glyph_key::GlyphKey` resolves.
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
