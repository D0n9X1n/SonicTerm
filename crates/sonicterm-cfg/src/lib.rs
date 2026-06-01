//! sonicterm-cfg — config, theme, keymap, and url_open loaders for SonicTerm Terminal.
//!
//! Split out of `sonicterm-core` in the PR-3 refactor (issue #121).
//! `sonicterm-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod config;
pub mod keymap;
pub mod theme;
pub mod url_open;
pub mod url_scan;

/// Re-export of [`sonicterm_logging::LoggingConfig`] so downstream
/// consumers can construct the field through the `sonicterm_cfg` facade
/// without taking a direct dep on `sonicterm-logging`.
pub use sonicterm_logging::LoggingConfig;
