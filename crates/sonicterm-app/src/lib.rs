//! Cross-platform SonicTerm application orchestration.
//!
//! This crate owns the winit application loop, window/tab/pane lifecycle,
//! PTY and parser wiring, input dispatch, redraw scheduling, config watching,
//! overlays, tab transfer, OS-drag bridges, menubar abstractions, and the
//! platform-shell builders consumed by the macOS and Windows binaries.

// TODO: add per-item docs and switch to #![deny(missing_docs)].
#![allow(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod menu;
pub mod menubar_bridge;
pub mod os_drag;
pub mod os_drag_bridge;
pub mod shell;
pub mod tab_drag;
pub mod tab_thumbnail;
pub mod window_key_boundary;

pub use app::{KeymapLoader, ThemeLoader};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
