//! Shared render-model: the seam between app/UI state and GPU drawing.
//! The app builds these snapshots; the production GPU renderer consumes them
//! directly through `GpuRenderer::render`.

// All pub items in this crate carry per-item doc comments.
#![deny(missing_docs)]

/// Window/pane pixel geometry primitives shared by layout and the painter.
pub mod geometry;
/// Per-frame, renderer-facing snapshot of the UI (panes, tabs, overlays).
pub mod inputs;
/// Small backend-agnostic drawing command trait. The production GPU renderer
/// currently consumes frame snapshots directly rather than implementing it.
pub mod painter;
pub mod pane_render;

/// Boundary re-exports: the single seam through which `sonicterm-gpu` reaches
/// terminal-grid, config/theme, and UI-state types. The renderer imports these
/// from `render_model::boundary::{grid, cfg, ui}` instead of depending on those
/// crates directly, so `render-model` is the one place the vt/grid -> gpu and
/// ui -> gpu boundaries are declared. The types are re-exported unchanged (same
/// identity), so this is a dependency re-layer with no behavior change.
pub mod boundary {
    /// Config + theme types the renderer resolves colors and modes from.
    pub use sonicterm_cfg as cfg;
    /// Terminal grid/cell/line/hyperlink data the renderer reads per cell.
    pub use sonicterm_grid as grid;
    /// UI layout/state (tabs, overlays, selection, scrollbar) the renderer composites.
    pub use sonicterm_ui as ui;
}

pub use geometry::*;
pub use inputs::*;
pub use painter::*;
pub use pane_render::{CursorStyle, InlineImage, PaneId, PaneRender};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
