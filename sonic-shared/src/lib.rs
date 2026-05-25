//! sonic-shared — windowing, tab/pane tree, app loop.
//!
//! This crate contains everything that is platform-agnostic but UI-shaped:
//! the [`tabs::TabBar`] model, [`pane::PaneTree`] for splits, and an
//! [`app::App`] that ties [`sonic_core`] (engine) to `winit` + `wgpu`.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod pane;
pub mod quad;
pub mod render;
pub mod selection;
pub mod tabbar_view;
pub mod tabs;

/// Re-exports for binary crates.
pub use app::run;
