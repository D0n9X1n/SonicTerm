use sonicterm_types::Cell;

/// Top-level read-only data for one frame.
#[derive(Default)]
pub struct RenderInputs<'a> {
    /// One entry per visible pane in z-order; each carries its own grid view.
    pub panes: Vec<PaneViewModel<'a>>,
    /// Tab strip contents — order matches click-target order.
    pub tab_bar: TabBarSnapshot,
    /// Modal / non-modal overlays to draw on top of the grid this frame.
    pub overlays: OverlayData,
    /// Active selection rectangle, if the user is mid-drag or has a sticky one.
    pub selection: Option<SelectionView>,
    /// Active in-pane search state when the search overlay is open.
    pub search: Option<SearchView>,
    /// Pixel rect of a visible "Cmd+hover URL underline" on the focused
    /// pane this frame, or `None` when no auto-detected URL is being
    /// hovered while the platform open-URL modifier (Cmd on macOS,
    /// Ctrl on Windows) is held. Added for the v1.0 Cmd-held URL
    /// affordance — OSC 8 hyperlinks have their own pre-existing
    /// hover-underline path and are not represented here.
    pub hovered_url_underline: Option<UnderlineRect>,
    /// Viewport cell range of the Cmd-hovered URL to recolor with the
    /// theme accent. `row` is the viewport row (0 = top visible),
    /// `start_col` is inclusive, `end_col` is exclusive. `None` when no
    /// auto-detected URL is being hovered while the open-URL modifier
    /// (Cmd on macOS, Ctrl on Windows / Linux) is held. Shares the same
    /// lifetime/gating as [`Self::hovered_url_underline`]; this is the
    /// glyph-recolor companion to that underline overlay.
    pub hovered_url_cells: Option<HoveredUrlCells>,
    /// Phase D — drag visual feedback (Epic #289).
    ///
    /// `Some(ghost)` while a tab drag session is live and the cursor
    /// has moved at least the drag-start threshold from the press
    /// point. Drives the three Phase D affordances:
    ///   * D1 ghost copy of the dragged tab at the cursor position,
    ///     painted at `alpha = 0.5`
    ///   * D2 insertion gap — when `insertion_slot` is `Some`, the
    ///     destination bar's `TabBarLayout::compute_with_insertion_slot`
    ///     shifts tabs at `[slot..]` right by 8 logical px
    ///   * D3 source tab grayed — when `source_tab_idx` is `Some`,
    ///     the corresponding tab in the source bar is painted at
    ///     `alpha = 0.3`
    pub drag_ghost: Option<DragGhost>,
}

/// Phase D drag-feedback descriptor — pure data passed from the App
/// layer to the renderer. The renderer reads this to paint a 50 %
/// alpha ghost copy of the dragged tab at the cursor, draw the 8 px
/// insertion gap in the destination bar, and gray out the source tab.
#[derive(Debug, Clone, PartialEq)]
pub struct DragGhost {
    /// Top-left of the ghost rect in physical pixels (typically the
    /// cursor position offset by half the chip size).
    pub top_left: (f32, f32),
    /// Title of the dragged tab — painted into the ghost.
    pub title: String,
    /// Alpha multiplier for the ghost. Spec: `0.5`.
    pub alpha: f32,
    /// Index of the tab in the source bar being dragged. The renderer
    /// paints that tab at [`Self::source_alpha`] in the source bar.
    pub source_tab_idx: Option<usize>,
    /// Alpha multiplier for the source tab while the drag is live.
    /// Spec: `0.3`.
    pub source_alpha: f32,
    /// Insertion slot in the destination bar — `Some(slot)` when the
    /// cursor is over a tab bar (OnBar / OnOtherBar). Tabs at
    /// `[slot..]` shift right by [`Self::insertion_gap_px`] logical
    /// pixels to preview the drop position.
    pub insertion_slot: Option<usize>,
    /// Width of the insertion gap in logical pixels. Spec: `8.0`.
    pub insertion_gap_px: f32,
}

impl DragGhost {
    /// Spec-default alpha for the ghost chip following the cursor.
    pub const GHOST_ALPHA: f32 = 0.5;
    /// Spec-default alpha for the source tab while drag is live.
    pub const SOURCE_ALPHA: f32 = 0.3;
    /// Spec-default width of the insertion gap in logical pixels.
    pub const INSERTION_GAP_PX: f32 = 8.0;
}

impl Default for DragGhost {
    fn default() -> Self {
        Self {
            top_left: (0.0, 0.0),
            title: String::new(),
            alpha: Self::GHOST_ALPHA,
            source_tab_idx: None,
            source_alpha: Self::SOURCE_ALPHA,
            insertion_slot: None,
            insertion_gap_px: Self::INSERTION_GAP_PX,
        }
    }
}

/// Axis-aligned pixel rectangle for an overlay underline quad.
/// Coordinates are in logical pixels relative to the focused pane's
/// origin; the renderer clips against the pane rect before submitting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnderlineRect {
    /// Left edge in pixels (pane-relative).
    pub x: f32,
    /// Top edge of the underline strip in pixels (pane-relative).
    pub y: f32,
    /// Width of the underline in pixels (covers all URL chars on the row).
    pub w: f32,
    /// Thickness in pixels (2.0 per the v1.0 spec).
    pub h: f32,
}

/// Viewport cell range of a Cmd-hovered auto-detected URL, carried to
/// the renderer so it can recolor the URL's glyphs with the theme
/// accent (in addition to the [`UnderlineRect`] hover underline).
///
/// Coordinates mirror `sonicterm_app`'s `HoveredUrl`: `row` is the
/// viewport row (0 = top visible row, same index the glyph-emit loop
/// uses), `start_col` is inclusive, and `end_col` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoveredUrlCells {
    /// Viewport row of the URL (0 = top visible row).
    pub row: u16,
    /// Inclusive start column of the URL on this row.
    pub start_col: u16,
    /// Exclusive end column of the URL on this row.
    pub end_col: u16,
}

impl HoveredUrlCells {
    /// True when cell `(row, col)` (viewport coordinates) falls inside
    /// the hovered URL span: same row, and `start_col <= col < end_col`.
    /// Used by the renderer's per-cell foreground decision to swap in
    /// the theme accent for the URL's glyphs only.
    #[must_use]
    pub fn contains(&self, row: u16, col: u16) -> bool {
        row == self.row && col >= self.start_col && col < self.end_col
    }
}

/// Per-pane data the renderer needs to paint one terminal grid this frame.
pub struct PaneViewModel<'a> {
    /// Borrowed rows of the grid slice currently visible (scrollback applied).
    pub rows: &'a [Vec<Cell>],
    /// Where the cursor is and whether it's lit on this blink phase.
    pub cursor: CursorView,
    /// Lines scrolled back from the live tail; 0 means "looking at bottom".
    pub scroll_offset: usize,
}

/// Snapshot of the tab strip for this frame — owned, so the renderer doesn't
/// need to lock the app's tab list.
#[derive(Default)]
pub struct TabBarSnapshot {
    /// Tab entries in left-to-right paint order.
    pub tabs: Vec<TabEntry>,
    /// Index into `tabs` of the active (highlighted) tab.
    pub active: usize,
}

/// One drawable tab in the tab strip.
pub struct TabEntry {
    /// Display title (already truncated to fit width_px by the layout pass).
    pub title: String,
    /// Computed pixel width of the tab's cell on the strip.
    pub width_px: u32,
}

/// Toggle flags for the modal/non-modal overlays drawn on top of the panes.
#[derive(Default)]
pub struct OverlayData {
    /// Command palette overlay is open.
    pub palette_open: bool,
    /// In-pane search bar overlay is open.
    pub search_open: bool,
}

/// Cursor position + blink phase used to draw the caret box.
#[derive(Default)]
pub struct CursorView {
    /// Row index in the visible viewport (0 = top row).
    pub row: usize,
    /// Column index in cells (0 = leftmost).
    pub col: usize,
    /// True on the visible half of the blink cycle.
    pub blink_on: bool,
}

/// Inclusive selection range in grid cell coordinates.
#[derive(Default)]
pub struct SelectionView {
    /// Anchor cell `(row, col)` — where the drag started.
    pub start: (usize, usize),
    /// Caret-side cell `(row, col)` — current pointer location.
    pub end: (usize, usize),
}

/// Search overlay state — list of hits and which one is currently focused.
#[derive(Default)]
pub struct SearchView {
    /// Each tuple is `(row, col_start, col_end)` of a match in the viewport.
    pub matches: Vec<(usize, usize, usize)>,
    /// Index into `matches` of the currently focused / highlighted hit.
    pub current: usize,
}

#[cfg(test)]
mod tests {
    use super::HoveredUrlCells;

    #[test]
    fn hovered_url_cells_contains_matches_inside_range_only() {
        // URL on viewport row 3, columns 5..10 (5,6,7,8,9 inclusive).
        let h = HoveredUrlCells { row: 3, start_col: 5, end_col: 10 };

        // Inclusive start, interior, and last-included column hit.
        assert!(h.contains(3, 5), "start_col is inclusive");
        assert!(h.contains(3, 7), "interior column");
        assert!(h.contains(3, 9), "end_col - 1 is the last included column");

        // Exclusive end and out-of-span columns miss.
        assert!(!h.contains(3, 10), "end_col is exclusive");
        assert!(!h.contains(3, 4), "column before the span");
        assert!(!h.contains(3, 11), "column past the span");

        // Wrong row never matches, even for in-span columns.
        assert!(!h.contains(2, 7), "row above");
        assert!(!h.contains(4, 7), "row below");
    }

    #[test]
    fn hovered_url_cells_empty_span_contains_nothing() {
        // Degenerate start == end span: end_col is exclusive so no
        // column can satisfy `start_col <= col < end_col`.
        let h = HoveredUrlCells { row: 0, start_col: 8, end_col: 8 };
        assert!(!h.contains(0, 8));
        assert!(!h.contains(0, 7));
    }
}
