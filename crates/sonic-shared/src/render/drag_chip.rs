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
    /// Phase D — Epic #289 — source tab index in the source bar.
    /// When `Some(idx)` the renderer paints that tab at
    /// [`Self::source_alpha`] (D3 grayed source).
    pub source_tab_idx: Option<usize>,
    /// Phase D source-tab alpha (default 0.3 per spec).
    pub source_alpha: f32,
    /// Phase D — Epic #289 — insertion slot in the destination bar.
    /// When `Some(slot)` the renderer computes the bar layout via
    /// `TabBarLayout::compute_with_insertion_slot` so tabs at
    /// `[slot..]` shift right by 8 logical px (D2 gap).
    pub insertion_slot: Option<usize>,
    /// Phase D ghost alpha (default 0.5 per spec) — multiplier on the
    /// chip body so it renders as a translucent ghost of the dragged
    /// tab (D1).
    pub ghost_alpha: f32,
}

/// Diagnostic snapshot of the most recently rendered drag chip.
/// Production code must not depend on it; tests read it via
/// [`crate::render::GpuRenderer::last_drag_chip_visual`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragChipVisual {
    /// Top-left of the chip in physical pixels.
    pub top_left: (f32, f32),
    /// Chip `(width, height)` in physical pixels.
    pub size: (f32, f32),
}
