//! sonicterm-engine: font, shaping, and raster seams used by the renderer.
//!
//! SonicTerm owns the terminal state in `sonicterm-vt` + `sonicterm-grid`.
//! This crate keeps the remaining font-facing engine seams while WezTerm
//! functionality is converted into Sonic-native modules.

#![warn(clippy::min_ident_chars)]

mod fontstack;
pub use fontstack::{
    CellMetricsPx, FaceIdentity, FontStack, ResolvedGlyphFace, DEFAULT_FONT_FAMILY,
};
/// The fallback notice's unresolved set, as a renderer reads it from [`FontStack::fallback_notice`].
pub use sonicterm_font::UnresolvedSnapshot;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
