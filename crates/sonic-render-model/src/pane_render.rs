//! Per-pane render input bundle.
//!
//! Part B of the per-pane render refactor (PR #199). The `GpuRenderer::render`
//! signature is being changed from a single `&mut Grid` to a slice of
//! `PaneRender<'_>` so the renderer iterates panes itself and anchors each
//! pane's cell-emission loop at the pane's own pixel origin rather than the
//! window-level `padding_left` / `top_inset()`.
//!
//! NOTE: This file is the foundation commit. The mechanical re-anchor of the
//! 62 `self.padding_left` / `self.top_inset()` sites inside
//! `crates/sonic-shared/src/render/core.rs::render()` is still pending and
//! tracked in the PR. Until that pass lands, the renderer continues to draw
//! only the active pane's content.

use crate::geometry::PixelRect;

/// Identifier for a pane within a tab. Matches `sonic_mux::proto::PaneId`
/// (kept as `u64` locally to avoid a cross-crate dep).
pub type PaneId = u64;

/// One pane's contribution to a frame. The renderer owns the iteration; the
/// caller (the winit app loop) is responsible for collecting the per-pane
/// `MutexGuard<Parser>` and exposing each `&mut Grid` for the duration of the
/// frame.
///
/// Lifetimes:
/// - `'a` — borrow of the parser's grid; lives as long as the parser guard
///   the caller holds.
pub struct PaneRender<'a> {
    /// Stable id used to look this pane up in the app's pane registry.
    pub id: PaneId,
    /// Pixel rect of this pane within the window content area, already
    /// adjusted for `top_inset()` / tab bar / titlebar.
    pub rect_px: PixelRect,
    /// Mutable borrow of this pane's grid. The renderer mutates dirty flags.
    pub grid: &'a mut sonic_grid::grid::Grid,
    /// True for the pane that owns the focus ring, IME caret, selection
    /// overlay, search highlight ribbon, and hyperlink hover popup. Exactly
    /// one pane per frame should have this set.
    pub is_active: bool,
    /// Cursor presentation style for this pane (block / bar / underline +
    /// blink). The renderer paints the cursor only on the active pane.
    pub cursor_style: CursorStyle,
    /// True when this pane is receiving mirrored broadcast input from the
    /// active/source pane and therefore needs prominent safety chrome.
    pub is_broadcast_receiver: bool,
}

/// Cursor presentation style. Mirrors the legacy enum in `sonic-ui::cursor`
/// but kept here to avoid pulling sonic-ui into sonic-render-model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorStyle {
    /// Solid filled block, no blink (DECSCUSR 2).
    BlockSteady,
    /// Solid filled block with blink (DECSCUSR 1, default).
    #[default]
    BlockBlink,
    /// Vertical bar (I-beam) without blink (DECSCUSR 6).
    BarSteady,
    /// Vertical bar (I-beam) with blink (DECSCUSR 5).
    BarBlink,
    /// Underline under the cell without blink (DECSCUSR 4).
    UnderlineSteady,
    /// Underline under the cell with blink (DECSCUSR 3).
    UnderlineBlink,
}
