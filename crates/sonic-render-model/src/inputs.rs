use sonic_types::Cell;

/// Top-level read-only data for one frame.
#[derive(Default)]
pub struct RenderInputs<'a> {
    pub panes: Vec<PaneViewModel<'a>>,
    pub tab_bar: TabBarSnapshot,
    pub overlays: OverlayData,
    pub selection: Option<SelectionView>,
    pub search: Option<SearchView>,
}

pub struct PaneViewModel<'a> {
    pub rows: &'a [Vec<Cell>],
    pub cursor: CursorView,
    pub scroll_offset: usize,
}

#[derive(Default)]
pub struct TabBarSnapshot {
    pub tabs: Vec<TabEntry>,
    pub active: usize,
}

pub struct TabEntry {
    pub title: String,
    pub width_px: u32,
}

#[derive(Default)]
pub struct OverlayData {
    pub palette_open: bool,
    pub prefs_open: bool,
    pub search_open: bool,
}

#[derive(Default)]
pub struct CursorView {
    pub row: usize,
    pub col: usize,
    pub blink_on: bool,
}

#[derive(Default)]
pub struct SelectionView {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

#[derive(Default)]
pub struct SearchView {
    pub matches: Vec<(usize, usize, usize)>, // row, col_start, col_end
    pub current: usize,
}
