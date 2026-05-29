use sonic_types::Cell;

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
    /// Preferences window overlay is open.
    pub prefs_open: bool,
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
