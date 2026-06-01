//! Façade for the app's pure-data state. Sketch type at M6a — the full
//! state machine extraction from `sonicterm-app::app::App` happens in
//! M6b..d. This crate currently owns the *shape* of the boundary, not
//! the implementation.

use sonicterm_types::WindowKey;

use crate::supporting::{BroadcastScope, LogicalPos};

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
    /// Number of panes the reducer believes are alive in the focused
    /// tab of the focused window. Incremented on `SplitPane`,
    /// decremented on `ClosePane`. Saturating in both directions.
    /// Starts at 0 — the first `SplitPane` brings it to 1 (the new
    /// pane that the split spawned); a subsequent `NewTab` does NOT
    /// bump this counter (panes are per-tab; a fresh tab starts with
    /// its own pane via the boundary's `spawn_pane`, not via a
    /// `SplitPane` Intent). The boundary's `WindowState.tab_states`
    /// remains source-of-truth for actual pane content; this counter
    /// is the observability surface used by the reducer arms and the
    /// pane tests in `pane_intents.rs`. (M6a-expand-2c-pane)
    pub pane_count: u32,
    /// Last pane index the reducer observed becoming focused within
    /// the active tab. `None` until the first `SplitPane` /
    /// `FocusPane*` lands. Used by reducer arms to deduplicate
    /// `Render(Focus)` Effects (no-op focus emits nothing).
    pub focused_pane_idx: Option<usize>,
    /// Whether the active pane is currently zoomed (single-pane
    /// fullscreen-within-tab). The boundary's `toggle_active_pane_zoom`
    /// path remains source-of-truth for the rendered layout; this
    /// boolean tracks the reducer's observable view of the toggle so
    /// `ZoomPane` Intents can be added in 2c-misc.
    pub pane_zoomed: bool,
    /// Last logical-pixel cursor position reported via `MouseMove`.
    /// `None` until the platform ships its first `CursorMoved` event.
    /// Used by the `MouseMove` reducer arm to implicitly coalesce
    /// — only emit a `Render(Hover)` when the position actually
    /// changed since the previous report (spec §3 mouse routing).
    /// (M6a-expand-2c-mouse)
    pub last_mouse_pos: Option<LogicalPos>,
    /// Whether the reducer believes the primary mouse button is
    /// currently pressed. Set true on `MouseButton { pressed: true,
    /// button: Left, .. }` and false on the matching release. The
    /// boundary's `WindowState.mouse_down` remains source-of-truth
    /// for the actual hit-test gates (tab drag, selection extend);
    /// this flag is the observability surface used by reducer arms.
    /// (M6a-expand-2c-mouse)
    pub mouse_left_down: bool,
    /// Whether the search overlay is open. Toggled by `OpenSearch` /
    /// `CloseSearch`. Reducer emits `Render(Overlay)` only on the
    /// open/close transition (same shape as `WindowFocused`'s
    /// transition guard). (M6a-expand-2c-misc)
    pub search_open: bool,
    /// Whether the command palette is open. Toggled by
    /// `ToggleCommandPalette`; closed on `PaletteSubmit`.
    /// (M6a-expand-2c-misc)
    pub palette_open: bool,
    /// Whether the reducer currently has an active selection drag.
    /// Set true on `SelectionStart`; cleared on `SelectionEnd` /
    /// `ClearSelection`. The boundary's per-window `selection`
    /// remains source-of-truth for the rendered rectangle; this is
    /// the observability surface. (M6a-expand-2c-misc)
    pub selection_active: bool,
    /// Last foreground-process name observed for the focused pane.
    /// `None` until the first `ForegroundProcChanged`. Reducer emits
    /// `Render(TitleOrTab)` only on actual name change. (2c-misc)
    pub fg_proc_name: Option<String>,
    /// Current broadcast-input multiplexing scope as observed by the
    /// reducer. Updated by `SetBroadcastScope`. The boundary's
    /// `App.broadcast` remains source-of-truth for actual fan-out;
    /// the reducer flag is the title/tab-strip re-paint gate.
    /// (M6a-expand-2c-misc)
    pub broadcast_scope: BroadcastScope,
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
            pane_count: 0,
            focused_pane_idx: None,
            pane_zoomed: false,
            last_mouse_pos: None,
            mouse_left_down: false,
            search_open: false,
            palette_open: false,
            selection_active: false,
            fg_proc_name: None,
            broadcast_scope: BroadcastScope::Off,
        }
    }
}
