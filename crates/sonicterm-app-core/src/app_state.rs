//! Façade for the app's pure-data state. Sketch type at M6a — the full
//! state machine extraction from `sonicterm-app::app::App` happens in
//! M6b..d. This crate currently owns the *shape* of the boundary, not
//! the implementation.

use sonicterm_types::WindowKey;

use crate::supporting::LogicalPos;

/// Pure-data application state. Intentionally minimal at M6a — fields
/// migrate over from `sonicterm-app::app::App` in subsequent
/// modularization PRs.
///
/// M6a-expand-2c-window adds tracking for window-lifecycle reducer
/// arms: which window most recently held focus, the last reported
/// resize cell dimensions, and the last reported logical position.
/// These power deterministic reducer outputs (`WindowFocused` emits
/// a `Render(Focus)` only when focus actually changed) without yet
/// owning the full pane/tab topology (that lands in 2c-tab/-pane).
#[derive(Debug, Default)]
pub struct AppState {
    /// Logical grid width in cells. Updated on `WindowResized`.
    pub cols: u32,
    /// Logical grid height in cells. Updated on `WindowResized`.
    pub rows: u32,
    /// Window the reducer believes is currently focused, if any.
    /// Updated on `WindowFocused` / `WindowBlurred`; cleared on
    /// `WindowCloseRequested` of the same window.
    pub focused_window: Option<WindowKey>,
    /// Last logical-pixel position reported via `WindowMoved`.
    /// `None` until the platform ships its first `Moved` event.
    pub last_window_pos: Option<LogicalPos>,
    /// Number of top-level platform windows the reducer believes are
    /// alive. Incremented on `NewWindow` (cascades a `WindowOpen`
    /// Effect), decremented on `WindowCloseRequested`. Used by the
    /// `WindowCloseRequested` reducer arm to decide whether to also
    /// emit `Quit` (last window closed).
    pub live_window_count: u32,
    /// Number of tabs the reducer believes are alive in the focused
    /// window. Incremented on `NewTab`, decremented on `CloseTab`
    /// (and on `TearOutTab` — the tab leaves this window's strip).
    /// The boundary's tab tree in
    /// `sonicterm-app::app::WindowState.tabs` remains source-of-truth
    /// for rendering; this counter is the observability surface used
    /// by the reducer arms and the focused tab tests in
    /// `tab_intents.rs`. Saturating in both directions
    /// (M6a-expand-2c-tab).
    pub tab_count: u32,
    /// Last tab index the reducer observed becoming active. `None`
    /// until the first `NewTab`/`NextTab`/`PrevTab`/`GoToTab` lands.
    /// Used by reducer arms to deduplicate `Render(TabSwitch)`
    /// Effects (no-op switch emits nothing — same shape as
    /// `WindowFocused`'s no-op transition guard).
    pub active_tab_idx: Option<usize>,
}

impl AppState {
    /// Construct a new builder.
    #[must_use]
    pub fn builder() -> AppStateBuilder {
        AppStateBuilder::default()
    }
}

/// Builder for `AppState`. Currently a thin pass-through — gains fields
/// in M6b..d as concrete state migrates.
#[derive(Debug, Default)]
pub struct AppStateBuilder {
    cols: u32,
    rows: u32,
}

impl AppStateBuilder {
    /// Set the initial grid size.
    #[must_use]
    pub fn with_grid(mut self, cols: u32, rows: u32) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Finalize.
    #[must_use]
    pub fn build(self) -> AppState {
        AppState {
            cols: self.cols,
            rows: self.rows,
            focused_window: None,
            last_window_pos: None,
            live_window_count: 0,
            tab_count: 0,
            active_tab_idx: None,
        }
    }
}
