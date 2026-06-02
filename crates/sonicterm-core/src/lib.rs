//! sonicterm-core — **deprecated façade** for back-compat with pre-PR-3 imports.
//!
//! The original `sonicterm-core` crate has been decomposed into four leaf crates:
//!
//! - [`sonicterm_vt`]   — VT/ANSI parser (`vt::Parser`, `vt::Performer`, …)
//! - [`sonicterm_grid`] — terminal grid + hyperlink registry
//! - [`sonicterm_cfg`]  — config / theme / keymap / url_open loaders
//! - [`sonicterm_io`]   — PTY + process probes + optional SSH backend
//!
//! Every re-export below carries `#[deprecated(since = "0.9.0")]` so
//! migrating consumers see the rename target. Removal target: v1.1.
//! See `docs/migrations/0.9.0.md` for the mapping table.

#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(deprecated)]

// Module-shaped re-exports — preserve `sonicterm_core::vt::...`,
// `sonicterm_core::grid::...`, etc.
#[deprecated(since = "0.9.0", note = "use `sonicterm_cfg::config` directly")]
pub use sonicterm_cfg::config;
#[deprecated(since = "0.9.0", note = "use `sonicterm_cfg::keymap` directly")]
pub use sonicterm_cfg::keymap;
#[deprecated(since = "0.9.0", note = "use `sonicterm_cfg::theme` directly")]
pub use sonicterm_cfg::theme;
#[deprecated(since = "0.9.0", note = "use `sonicterm_cfg::url_open` directly")]
pub use sonicterm_cfg::url_open;
#[deprecated(since = "0.9.0", note = "use `sonicterm_cfg::url_scan` directly")]
pub use sonicterm_cfg::url_scan;
#[deprecated(since = "0.9.0", note = "use `sonicterm_grid::grid` directly")]
pub use sonicterm_grid::grid;
#[deprecated(since = "0.9.0", note = "use `sonicterm_grid::hyperlink` directly")]
pub use sonicterm_grid::hyperlink;
#[deprecated(since = "0.9.0", note = "use `sonicterm_io::proc_info` directly")]
pub use sonicterm_io::proc_info;
#[deprecated(since = "0.9.0", note = "use `sonicterm_io::pty` directly")]
pub use sonicterm_io::pty;
#[deprecated(since = "0.9.0", note = "use `sonicterm_io::ssh` directly")]
pub use sonicterm_io::ssh;
#[deprecated(since = "0.9.0", note = "use `sonicterm_vt::vt` directly")]
pub use sonicterm_vt::vt;

#[cfg(windows)]
#[deprecated(since = "0.9.0", note = "use `sonicterm_io::foreground_proc` directly")]
pub use sonicterm_io::foreground_proc;

// `glyph_key` historically lived in sonicterm-core; it's a thin wrapper that
// just re-exports `sonicterm_types::GlyphKey`. Keep it as a sub-module here so
// `sonicterm_core::glyph_key::GlyphKey` resolves.
pub mod glyph_key;

/// Test-only helpers (ShellDialect for the e2e gate examples + integration
/// tests). Gated behind `cfg(feature = "test_support")` so production builds
/// don't pull this in. Examples enable the feature in their build config.
#[cfg(feature = "test_support")]
pub mod test_support;

/// Re-exports of the most commonly used items.
#[deprecated(since = "0.9.0", note = "use leaf crates directly; see docs/migrations/0.9.0.md")]
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
