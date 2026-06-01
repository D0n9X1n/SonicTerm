//! sonicterm-app — winit app loop, OS drag-and-drop glue, menubar bridge,
//! config watcher, and top-level event-loop wiring for SonicTerm Terminal.
//!
//! Extracted from `sonicterm-shared` in refactor PR 8a. The previous
//! `sonicterm_shared::app`, `sonicterm_shared::menu`, `sonicterm_shared::menubar_bridge`,
//! `sonicterm_shared::os_drag(_bridge)`, `sonicterm_shared::tab_drag`, and
//! `sonicterm_shared::config_watch` import paths are preserved as re-exports
//! from `sonicterm-shared` for backwards compatibility.

// TODO: add per-item docs and switch to #![deny(missing_docs)] in a follow-up PR.
#![allow(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod config_watch;
pub mod menu;
pub mod menubar_bridge;
pub mod os_drag;
pub mod os_drag_bridge;
pub mod tab_drag;
pub mod tab_thumbnail;
pub mod window_key_boundary;

pub use app::run;
pub use app::{run_with, KeymapLoader, ThemeLoader};
