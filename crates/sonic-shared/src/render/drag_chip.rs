//! Drag-chip overlay structs extracted from `render.rs` (issue #143).

/// Translucent ~120x24 quad drawn at the cursor while a tab is held.
#[derive(Debug, Clone)]
pub struct DragChipOverlay {
    /// Top-left of the chip rect in physical pixels.
    pub top_left: (f32, f32),
    /// Title text of the dragged tab.
    pub title: String,
    /// When `Some`, draw a 2-3px vertical accent bar (the "drop line")
    /// at this logical-pixel X coordinate, spanning the tab bar's
    /// vertical range. This indicates the insertion slot the dragged
    /// tab would land in if released right now. `None` when the cursor
    /// has left the bar Y range (tear-out armed).
    pub drop_line_x: Option<f32>,
    /// Vertical span `(top, bottom)` of the drop-line accent in
    /// logical pixels — matches the tab bar's Y range so the line is
    /// flush with the bar chrome.
    pub drop_line_y: (f32, f32),
    /// Multiplicative scale applied to the chip when rendered, used to
    /// give a subtle 1.0 → 1.02 ease on tear-out arm. `1.0` is the
    /// in-bar resting state; the renderer interpolates around this.
    pub scale: f32,
}

/// Diagnostic snapshot of the most recently rendered drag chip.
/// Production code must not depend on it; tests read it via
/// [`crate::render::GpuRenderer::last_drag_chip_visual`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragChipVisual {
    pub top_left: (f32, f32),
    pub size: (f32, f32),
}
