//! The row-run warm phase's handshake: the role writes update `n` and waits; the probe marks `n`
//! presented only once the grid shows the whole of `n` and a presentation followed that sighting,
//! then the role writes `n + 1`. So every warm update is drawn whole, not merely printed.
//!
//! An update is whole when the cursor rests just past the footer, the update's final write, and
//! every body row of the frozen geometry carries the update's digits. Each update repositions the
//! cursor to row 1 first, so a cursor still past the footer with every row's digits changed means
//! the footer of this update, not the previous one, was the last thing parsed.
//!
//! The scan and the presented-frame count both run on the main thread, and the renderer reads the
//! grid when it builds a frame, so a frame counted after a scan saw update `n` whole draws a grid
//! at least that complete. The role is blocked until the mark, so the grid cannot move past `n`.

use sonicterm_grid::grid::Grid;

use crate::workload::{RowRunGeometry, RowRunWorkload};

/// The warm-phase handshake state for one row-run role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresentedUpdates {
    kind: RowRunWorkload,
    geometry: RowRunGeometry,
    count: u32,
    next: u32,
    /// The presented-frame count when the awaited update was first seen whole in the grid.
    complete_at: Option<u64>,
    /// One forced presentation is owed for the sighting and not yet requested. The frame that drew
    /// the update may have presented before the scan saw it, and nothing else need present again.
    redraw_owed: bool,
}

impl PresentedUpdates {
    /// A handshake over updates 0 to `count`-1 of `kind`, generated for the frozen `geometry`.
    pub(crate) fn new(kind: RowRunWorkload, geometry: RowRunGeometry, count: u32) -> Self {
        Self { kind, geometry, count, next: 0, complete_at: None, redraw_owed: false }
    }

    /// Whether every warm update has been marked presented.
    pub(crate) fn done(&self) -> bool {
        self.next >= self.count
    }

    /// The digits of body screen row `row` in update `update`: only the fields that change, since
    /// wide characters' continuation cells make the rest of the row text host-dependent.
    pub(crate) fn fingerprint(kind: RowRunWorkload, row: u16, update: u32) -> String {
        kind.body_segments(row, update)
            .iter()
            .flat_map(|(_, _, text)| text.chars())
            .filter(char::is_ascii_digit)
            .collect()
    }

    /// Whether `grid` shows update `update` of `kind` whole on `geometry`: the frozen dimensions, the
    /// cursor just past the footer, and every body row's digits. The cursor is read first, so a scan
    /// in the middle of an update costs one comparison.
    pub(crate) fn complete(
        kind: RowRunWorkload,
        geometry: RowRunGeometry,
        grid: &Grid,
        update: u32,
    ) -> bool {
        let (footer_row, footer_end) = geometry.footer_end();
        (grid.cols, grid.rows) == (geometry.cols, geometry.rows)
            && (grid.cursor.row, grid.cursor.col) == (footer_row, footer_end)
            && geometry.body_screen_rows().all(|row| {
                grid.row(row - 1)
                    .iter()
                    .map(|cell| cell.ch)
                    .filter(char::is_ascii_digit)
                    .eq(Self::fingerprint(kind, row, update).chars())
            })
    }

    /// Feed one scan: whether the grid showed the awaited update whole, and the presented-frame
    /// count then. A first whole sighting records `frames` and owes one presentation.
    pub(crate) fn observe(&mut self, whole: bool, frames: u64) {
        if self.done() {
            // When: `done`, every warm update is marked and nothing more is awaited.
            return;
        }
        if !whole {
            // When: the grid does not show the awaited update whole (a split delivery, a cleared
            // screen or a changed grid), any earlier sighting no longer holds.
            self.complete_at = None;
            self.redraw_owed = false;
            return;
        }
        if self.complete_at.is_none() {
            self.complete_at = Some(frames);
            self.redraw_owed = true;
        }
    }

    /// Feed one scan of `grid` with the presented-frame count then.
    pub(crate) fn observe_grid(&mut self, grid: &Grid, frames: u64) {
        let whole = !self.done() && Self::complete(self.kind, self.geometry, grid, self.next);
        self.observe(whole, frames);
    }

    /// Feed the presented-frame count after a presentation. Returns the update to mark presented
    /// once a frame presented after the scan that saw it whole.
    pub(crate) fn observe_frames(&mut self, frames: u64) -> Option<u32> {
        let seen = self.complete_at?;
        if self.done() || frames <= seen {
            // When: nothing is awaited, or no frame has presented since the sighting.
            return None;
        }
        let presented = self.next;
        self.next += 1;
        self.complete_at = None;
        // A presentation already followed the sighting, so the owed one is no longer needed.
        self.redraw_owed = false;
        Some(presented)
    }

    /// Whether to force the owed presentation now; true at most once per sighting.
    pub(crate) fn take_redraw_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw_owed)
    }
}

#[cfg(test)]
#[path = "presented_updates_tests.rs"]
mod presented_updates_tests;
