//! Façade for the app's pure-data state. Sketch type at M6a — the full
//! state machine extraction from `sonicterm-app::app::App` happens in
//! M6b..d. This crate currently owns the *shape* of the boundary, not
//! the implementation.

/// Pure-data application state. Intentionally minimal at M6a — fields
/// migrate over from `sonicterm-app::app::App` in subsequent
/// modularization PRs.
#[derive(Debug, Default)]
pub struct AppState {
    /// Logical grid width in cells. Updated on resize.
    pub cols: u32,
    /// Logical grid height in cells.
    pub rows: u32,
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
        AppState { cols: self.cols, rows: self.rows }
    }
}
