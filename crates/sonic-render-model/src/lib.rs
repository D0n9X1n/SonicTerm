//! Shared render-model: the seam between UI state and GPU drawing.
//! UI builds these structs; GPU consumes them via the Painter trait.

// All pub items in this crate carry per-item doc comments.
#![deny(missing_docs)]

/// Window/pane pixel geometry primitives shared by layout and the painter.
pub mod geometry;
/// Per-frame, renderer-facing snapshot of the UI (panes, tabs, overlays).
pub mod inputs;
/// Abstract drawing surface trait the GPU backend implements.
pub mod painter;
pub mod pane_render;

pub use geometry::*;
pub use inputs::*;
pub use painter::*;
pub use pane_render::{CursorStyle, PaneId, PaneRender};
