//! Pure-logic helpers for the "Cmd+hover auto-detected URL" affordance.
//!
//! Behavior contract:
//!
//! - Modifier required: macOS = Super (Cmd); Windows/Linux = Control.
//! - One hover carries up to eight ordered viewport-row fragments so an
//!   automatically wrapped local path can underline and recolor as one target.
//! - Plain hover uses the yellow hint; modifier-held hover uses the action accent.
//! - OSC 8 hyperlinks keep their separate renderer-owned hover path.
//!
//! Sibling tests in `hovered_url_tests.rs` exercise the renderer projection
//! without a live winit or wgpu context; URL scan/open policy is covered in
//! `sonicterm-cfg` tests.

/// Snapshot of a hover hit used to drive cursor and renderer transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoveredUrl {
    /// Renderer-ready visible fragments of this target.
    pub cells: sonicterm_render_model::inputs::HoveredUrlCells,
    /// Complete target text retained for logging and activation feedback.
    pub url: String,
}

impl HoveredUrl {
    /// Return the allocation-free renderer value unchanged.
    #[must_use]
    pub fn to_cells(&self) -> sonicterm_render_model::inputs::HoveredUrlCells {
        self.cells
    }

    /// Whether the platform open modifier currently authorizes this target.
    #[must_use]
    pub fn active(&self) -> bool {
        self.cells.active
    }
}

#[cfg(test)]
#[path = "hovered_url_tests.rs"]
mod hovered_url_tests;
