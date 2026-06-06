//! sonicterm-engine: font, shaping, and raster seams used by the renderer.
//!
//! SonicTerm owns the terminal state in `sonicterm-vt` + `sonicterm-grid`.
//! This crate keeps the remaining font-facing engine seams while WezTerm
//! functionality is converted into Sonic-native modules.

mod fontstack;
pub use fontstack::{CellMetricsPx, FontStack};
