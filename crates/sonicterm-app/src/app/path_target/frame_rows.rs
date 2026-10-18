//! The one row reader URL and path detection goes through.
//!
//! Detection may read only the rows the pane displays, `[view_top, view_top + rows)`. Every
//! detection helper takes a [`FrameRows`] instead of a grid, so the frame bound, the count of
//! rows read and the range a cached result depends on are all recorded in one place.

use std::cell::Cell;

use sonicterm_grid::grid::{Grid, Row};

/// The displayed frame of one pane's grid, and a record of the rows detection read from it.
pub(in crate::app) struct FrameRows<'grid> {
    grid: &'grid Grid,
    view_top: u64,
    view_end: u64,
    /// Rows served, counting a repeated read of one row each time.
    rows_read: Cell<u64>,
    /// The lowest and highest absolute rows served, or `None` before the first.
    read_span: Cell<Option<(u64, u64)>>,
}

impl<'grid> FrameRows<'grid> {
    /// The frame of `grid` whose top displayed row is absolute row `view_top`.
    pub(in crate::app) fn new(grid: &'grid Grid, view_top: u64) -> Self {
        Self {
            grid,
            view_top,
            view_end: view_top.saturating_add(u64::from(grid.rows)),
            rows_read: Cell::new(0),
            read_span: Cell::new(None),
        }
    }

    /// The grid behind the frame, for its geometry and screen identity; never for its rows.
    pub(in crate::app) fn grid(&self) -> &'grid Grid {
        self.grid
    }

    /// Absolute row of the frame's top displayed row.
    pub(in crate::app) fn view_top(&self) -> u64 {
        self.view_top
    }

    /// Absolute row just past the frame's bottom displayed row.
    pub(in crate::app) fn view_end(&self) -> u64 {
        self.view_end
    }

    /// Whether absolute `row` is displayed.
    pub(in crate::app) fn contains(&self, row: u64) -> bool {
        (self.view_top..self.view_end).contains(&row)
    }

    /// Whether the grid holds a row below the frame, from its row count alone: true exactly when
    /// the view is scrolled back. No row outside the frame is read to answer it.
    pub(in crate::app) fn rows_below(&self) -> bool {
        let stored_rows =
            (self.grid.scrollback_len() as u64).saturating_add(u64::from(self.grid.rows));
        self.view_end < stored_rows
    }

    /// The displayed row at absolute `row`; `None` outside the frame or past the grid's rows.
    ///
    /// `None` outside the frame is not evidence about that row: a caller at a frame edge checks
    /// [`Self::contains`] or [`Self::rows_below`] first and fails closed where it would matter.
    pub(in crate::app) fn row(&self, row: u64) -> Option<&'grid Row> {
        #[cfg(test)]
        read_observer::record(row, self.contains(row));
        if !self.contains(row) {
            // When: `row` is not displayed, detection must not see it.
            return None;
        }
        self.rows_read.set(self.rows_read.get().saturating_add(1));
        let span = match self.read_span.get() {
            Some((low, high)) => (low.min(row), high.max(row)),
            None => (row, row),
        };
        self.read_span.set(Some(span));
        self.grid.row_at_abs(row)
    }

    /// Rows served so far, a repeated read counting each time.
    pub(in crate::app) fn rows_read(&self) -> u64 {
        self.rows_read.get()
    }

    /// The lowest and highest absolute rows served so far; `None` when none was. Every row a
    /// detection depends on lies in this span, because detection reads rows only through here.
    pub(in crate::app) fn read_span(&self) -> Option<(u64, u64)> {
        self.read_span.get()
    }
}

/// A test-only record of every row a detection asked for, inside the frame or not.
#[cfg(test)]
pub(in crate::app) mod read_observer {
    use std::cell::RefCell;

    thread_local! {
        static READS: RefCell<Option<Vec<(u64, bool)>>> = const { RefCell::new(None) };
    }

    /// Record one requested row and whether the frame contained it, while an observation runs.
    pub(super) fn record(row: u64, in_frame: bool) {
        READS.with(|reads| {
            if let Some(reads) = reads.borrow_mut().as_mut() {
                reads.push((row, in_frame));
            }
        });
    }

    /// Run `body` and return its value with every `(row, in_frame)` request it made on this thread.
    pub(in crate::app) fn observe<Value>(
        body: impl FnOnce() -> Value,
    ) -> (Value, Vec<(u64, bool)>) {
        let previous = READS.with(|reads| reads.borrow_mut().replace(Vec::new()));
        let value = body();
        let reads = READS.with(|reads| std::mem::replace(&mut *reads.borrow_mut(), previous));
        (value, reads.unwrap_or_default())
    }
}

#[cfg(test)]
#[path = "frame_rows_tests.rs"]
mod frame_rows_tests;
