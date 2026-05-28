//! sonic-cfg — config, theme, keymap, and url_open loaders for Sonic Terminal.
//!
//! Split out of `sonic-core` in the PR-3 refactor (issue #121).
//! `sonic-core` re-exports this crate's contents for back-compat.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod config;
pub mod keymap;
pub mod theme;
pub mod url_open;
pub mod url_scan;

/// Re-export of [`sonic_logging::LoggingConfig`] so downstream
/// consumers can construct the field through the `sonic_cfg` facade
/// without taking a direct dep on `sonic-logging`.
pub use sonic_logging::LoggingConfig;
