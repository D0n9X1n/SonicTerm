//! Per-pane render input bundle.
//!
//! `GpuRenderer::render` receives a slice of `PaneRender<'_>` and iterates every
//! visible pane. Each pane carries its own pixel origin and mutable grid view so
//! split panes, scrollback viewports, cursors, broadcast chrome, and inline
//! images are assembled in one frame.

use crate::geometry::PixelRect;
use std::sync::Arc;

/// Identifier for a pane within a tab. Kept as a bare `u64` so the render
/// model stays free of a cross-crate dependency for an id it only carries.
pub type PaneId = u64;

/// Decoded inline image ready for the GPU atlas.
#[derive(Clone, Debug)]
pub struct InlineImage {
    /// Stable image id used as the renderer cache key.
    pub id: u64,
    /// Grid row where the image's top-left corner anchors.
    pub row: u16,
    /// Grid column where the image's top-left corner anchors.
    pub col: u16,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Premultiplied sRGB-encoded BGRA8 pixels, row-major.
    pub bgra: Arc<[u8]>,
}

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
    /// Per-frame view of the pane's Sonic grid. Terminal state remains owned
    /// by `sonicterm-vt` + `sonicterm-grid`; WezTerm behavior is converted
    /// into those crates instead of inserting an upstream terminal facade here.
    pub grid: &'a mut sonicterm_grid::grid::Grid,
    /// Optional scrollback-absolute row at the top of this pane's viewport.
    /// `None` means follow the live tail.
    pub viewport_top_abs: Option<u64>,
    /// True for the pane that owns the focus ring, IME caret, selection
    /// overlay, search highlight ribbon, and hyperlink hover popup. Exactly
    /// one pane per frame should have this set.
    pub is_active: bool,
    /// Cursor presentation style for this pane (block / bar / underline +
    /// blink). The renderer paints the cursor only on the active pane.
    pub cursor_style: CursorStyle,
    /// True for the fixed broadcast source and each eligible receiver that needs safety chrome.
    pub is_broadcast_participant: bool,
    /// Per-pane scrollbar alpha. `1.0` = fully visible,
    /// `0.0` = hidden. The renderer multiplies the scrollbar tint
    /// alphas by this and skips the emit entirely below the floor.
    pub scrollbar_alpha: f32,
    /// Decoded inline media images anchored to this pane's grid.
    pub inline_images: Vec<InlineImage>,
}

/// A frame's panes, lent to the renderer once. The renderer assembles the frame inside `lend`'s
/// closure; when `lend` returns, the source is gone, so an owning source releases every parser
/// guard before the frame is presented.
pub trait FrameSource {
    /// Lend the frame's panes to `assemble` exactly once, then drop every guard.
    ///
    /// The closure is higher-ranked over both lifetimes, so its result cannot hold a pane or a grid.
    fn lend<R>(
        self,
        assemble: impl for<'slice, 'grid> FnOnce(&'slice mut [PaneRender<'grid>]) -> R,
    ) -> R;
}

/// A source over panes the caller keeps borrowing; lending releases nothing.
pub struct BorrowedSource<'slice, 'grid>(pub &'slice mut [PaneRender<'grid>]);

impl FrameSource for BorrowedSource<'_, '_> {
    fn lend<R>(
        self,
        assemble: impl for<'slice, 'grid> FnOnce(&'slice mut [PaneRender<'grid>]) -> R,
    ) -> R {
        assemble(self.0)
    }
}

/// Which of a pane's dirty rows a presented frame drew and may acknowledge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AckRows {
    /// Every row: the frame drew the whole pane.
    All,
    /// Only these visible row slots.
    Rows(sonicterm_grid::grid::RowSet),
}

/// What a presented frame drew of one pane, as metadata only: the pane's position in the frame, its
/// id, the grid identities it was assembled from, and the rows it may acknowledge. Applying it clears
/// those rows only when every identity still matches the grid, so dirt written after assembly stays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AckReceipt {
    /// The pane's index in the frame's pane slice.
    pub index: usize,
    /// The pane's id.
    pub pane_id: PaneId,
    /// The grid's cell-content revision when the frame was assembled.
    pub revision: u64,
    /// The grid's dirty generation when the frame was assembled.
    pub dirty_generation: u64,
    /// The grid's size generation when the frame was assembled.
    pub size_generation: u64,
    /// The grid's screen epoch when the frame was assembled.
    pub screen_epoch: u64,
    /// The rows the frame drew.
    pub rows: AckRows,
}

impl AckReceipt {
    /// The receipt for what a frame drew of `grid`, read from the grid as it is now.
    pub fn of(
        index: usize,
        pane_id: PaneId,
        grid: &sonicterm_grid::grid::Grid,
        rows: AckRows,
    ) -> Self {
        AckReceipt {
            index,
            pane_id,
            revision: grid.revision(),
            dirty_generation: grid.dirty_generation(),
            size_generation: grid.size_generation(),
            screen_epoch: grid.screen_epoch(),
            rows,
        }
    }

    /// Whether `grid` still has the size and screen this receipt was assembled from, so its row
    /// slots still name the same rows. Content and dirt may have moved on since.
    pub fn same_structure(&self, grid: &sonicterm_grid::grid::Grid) -> bool {
        self.size_generation == grid.size_generation() && self.screen_epoch == grid.screen_epoch()
    }

    /// Whether `grid` is still exactly the grid this receipt was assembled from.
    pub fn matches(&self, grid: &sonicterm_grid::grid::Grid) -> bool {
        self.revision == grid.revision()
            && self.dirty_generation == grid.dirty_generation()
            && self.size_generation == grid.size_generation()
            && self.screen_epoch == grid.screen_epoch()
    }

    /// Clear the receipt's rows from `grid`, keeping every row dirtied after the frame was assembled;
    /// returns whether the receipt applied. A grid whose size or screen changed clears nothing.
    pub fn try_apply(&self, grid: &mut sonicterm_grid::grid::Grid) -> bool {
        if !self.same_structure(grid) {
            // When: `same_structure` is false, a resize or screen switch renumbered the rows; keep all dirt.
            return false;
        }
        match &self.rows {
            AckRows::All => grid.clear_dirty_through(self.dirty_generation),
            AckRows::Rows(rows) => grid.clear_dirty_rows_through(rows, self.dirty_generation),
        }
        true
    }
}

/// Cursor presentation style carried directly in the render boundary so the
/// GPU does not depend on a concrete UI cursor-state representation.
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

#[cfg(test)]
#[path = "pane_render_tests.rs"]
mod pane_render_tests;
