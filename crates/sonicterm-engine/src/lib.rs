//! sonicterm-engine: font, shaping, and raster seams used by the renderer.
//!
//! SonicTerm owns the terminal state in `sonicterm-vt` + `sonicterm-grid`.
//! This crate keeps the remaining font-facing engine seams while WezTerm
//! functionality is converted into Sonic-native modules.

#![warn(clippy::min_ident_chars)]

mod fontstack;
mod resolved_family;
/// The platform's default monospace families, in preference order; the first is
/// [`DEFAULT_FONT_FAMILY`].
pub use config::DEFAULT_MONOSPACE_FAMILIES;
pub use fontstack::{
    CellMetricsPx, FaceIdentity, FontStack, ResolvedGlyphFace, DEFAULT_FONT_FAMILY,
};
/// Installed monospace font families from the platform locator; slow, so it runs on a worker thread.
pub use sonicterm_font::catalog::installed_monospace_families;
/// The fallback notice's unresolved set, as a renderer reads it from [`FontStack::fallback_notice`].
pub use sonicterm_font::UnresolvedSnapshot;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
