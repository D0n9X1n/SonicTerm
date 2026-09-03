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
    /// Bounded viewport fragments of the hovered plain-text URL or local path.
    ///
    /// The renderer uses the same ordered spans for underline geometry and
    /// active glyph recoloring. OSC 8 links retain their separate hover path.
    pub hovered_url_cells: Option<HoveredUrlCells>,
    /// drag visual feedback.
    ///
    /// `Some(ghost)` while a tab drag session is live and the cursor
    /// has moved at least the drag-start threshold from the press
    /// point. Drives the three drag affordances:
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

/// Drag-feedback descriptor — pure data passed from the App
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

/// Maximum number of visible row fragments in one hovered target.
pub const MAX_HOVERED_URL_SPANS: usize = 8;

/// One non-empty half-open hovered-target fragment in viewport coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct HoveredUrlSpan {
    /// Viewport row of this fragment.
    pub row: u16,
    /// Inclusive first column.
    pub start_col: u16,
    /// Exclusive final column.
    pub end_col: u16,
}

/// Bounded visible cell fragments of one hovered target.
///
/// The fixed array keeps this value allocation-free and `Copy` so it can remain
/// part of the renderer's retained-frame and row-cache identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoveredUrlCells {
    /// Pane whose grid owns this target.
    pub pane_id: u64,
    spans: [HoveredUrlSpan; MAX_HOVERED_URL_SPANS],
    span_count: u8,
    /// Whether the platform open modifier currently authorizes activation.
    pub active: bool,
}

impl HoveredUrlCells {
    /// Build one canonical ordered, non-empty, bounded fragment set.
    #[must_use]
    pub fn new(
        pane_id: u64,
        spans: impl IntoIterator<Item = HoveredUrlSpan>,
        active: bool,
    ) -> Option<Self> {
        let mut retained = [HoveredUrlSpan::default(); MAX_HOVERED_URL_SPANS];
        let mut span_count = 0usize;
        for span in spans {
            if span_count == MAX_HOVERED_URL_SPANS
                || span.end_col <= span.start_col
                || span_count > 0 && span.row <= retained[span_count - 1].row
            {
                // When: `span_count`, `end_col`, `start_col`, `row`, or `retained` violates bounds/order, reject the whole hover target.
                return None;
            }
            retained[span_count] = span;
            span_count += 1;
        }
        if span_count == 0 {
            // When: `span_count == 0`, no target geometry exists to render.
            return None;
        }
        Some(Self { pane_id, spans: retained, span_count: span_count as u8, active })
    }

    /// Build one single-row fragment using the same canonical checks.
    #[must_use]
    pub fn single(
        pane_id: u64,
        row: u16,
        start_col: u16,
        end_col: u16,
        active: bool,
    ) -> Option<Self> {
        Self::new(pane_id, [HoveredUrlSpan { row, start_col, end_col }], active)
    }

    /// Ordered visible fragments retained by this value.
    #[must_use]
    pub fn spans(&self) -> &[HoveredUrlSpan] {
        &self.spans[..usize::from(self.span_count)]
    }

    /// Fragment intersecting `row`, if one exists.
    #[must_use]
    pub fn span_for_row(&self, row: u16) -> Option<HoveredUrlSpan> {
        self.spans().iter().copied().find(|span| span.row == row)
    }

    /// Whether viewport cell `(row, col)` belongs to any retained fragment.
    #[must_use]
    pub fn contains(&self, row: u16, col: u16) -> bool {
        self.span_for_row(row).is_some_and(|span| col >= span.start_col && col < span.end_col)
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
#[path = "inputs_tests.rs"]
mod inputs_tests;
