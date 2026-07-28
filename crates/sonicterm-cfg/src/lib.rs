//! sonicterm-cfg — config, theme, keymap, and url_open loaders for SonicTerm Terminal.
//!
//! `sonicterm-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod assets;
pub mod config;
pub mod dimension;
pub mod keymap;
pub mod theme;
pub mod url_open;
pub mod url_scan;

/// Re-export of [`sonicterm_logging::LoggingConfig`] so downstream
/// consumers can construct the field through the `sonicterm_cfg` facade
/// without taking a direct dep on `sonicterm-logging`.
pub use sonicterm_logging::LoggingConfig;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
