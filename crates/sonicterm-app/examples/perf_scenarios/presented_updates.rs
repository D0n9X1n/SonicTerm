//! The row-run warm phase's handshake: the role writes update `n` and waits; the probe marks `n`
//! presented only once every body row of its grid shows `n` and a presentation followed that
//! sighting, then the role writes `n + 1`. So every warm update is drawn whole, not merely printed.
//!
//! The scan and the presented-frame count both run on the main thread, and the renderer reads the
//! grid when it builds a frame, so a frame counted after a scan saw update `n` complete draws a grid
//! at least that complete. The role is blocked until the mark, so the grid cannot move past `n`.

use sonicterm_grid::grid::Grid;

use crate::workload::{RowRunWorkload, ROW_RUN_BODY_ROWS};

/// The warm-phase handshake state for one row-run role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresentedUpdates {
    kind: RowRunWorkload,
    count: u32,
    next: u32,
    /// The presented-frame count when the awaited update was first seen complete in the grid.
    complete_at: Option<u64>,
    /// One forced presentation is owed for the sighting and not yet requested. The frame that drew
    /// the update may have presented before the scan saw it, and nothing else need present again.
    redraw_owed: bool,
}

impl PresentedUpdates {
    /// A handshake over updates 0 to `count`-1 of `kind`.
    pub(crate) fn new(kind: RowRunWorkload, count: u32) -> Self {
        Self { kind, count, next: 0, complete_at: None, redraw_owed: false }
    }

    /// Whether every warm update has been marked presented.
    pub(crate) fn done(&self) -> bool {
        self.next >= self.count
    }

    /// The 0-based grid index of the first body row; body row `index` is screen row `index + 1`.
    pub(crate) fn first_body_index() -> u16 {
        ROW_RUN_BODY_ROWS.start() - 1
    }

    /// The number of body rows an update rewrites.
    pub(crate) fn body_row_count() -> usize {
        ROW_RUN_BODY_ROWS.count()
    }

    /// The body rows of `grid` in screen order, each row's cell characters joined. An undersized
    /// grid yields fewer rows, which the handshake never counts as complete.
    pub(crate) fn body_rows(grid: &Grid) -> Vec<String> {
        (Self::first_body_index()..grid.rows)
            .take(Self::body_row_count())
            .map(|index| grid.row(index).iter().map(|cell| cell.ch).collect())
            .collect()
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

    /// Whether `body_rows`, the grid's body rows in screen order, all show update `update`.
    fn complete(kind: RowRunWorkload, body_rows: &[String], update: u32) -> bool {
        body_rows.len() == Self::body_row_count()
            && ROW_RUN_BODY_ROWS.zip(body_rows).all(|(row, text)| {
                text.chars()
                    .filter(char::is_ascii_digit)
                    .eq(Self::fingerprint(kind, row, update).chars())
            })
    }

    /// Feed one scan: the grid's body rows in screen order and the presented-frame count then.
    /// A first complete sighting of the awaited update records `frames` and owes one presentation.
    pub(crate) fn observe_grid(&mut self, body_rows: &[String], frames: u64) {
        if self.done() {
            // When: `done`, every warm update is marked and nothing more is awaited.
            return;
        }
        if !Self::complete(self.kind, body_rows, self.next) {
            // When: the grid does not show every body row of the awaited update (a split delivery
            // or a cleared screen), any earlier sighting no longer holds.
            self.complete_at = None;
            self.redraw_owed = false;
            return;
        }
        if self.complete_at.is_none() {
            self.complete_at = Some(frames);
            self.redraw_owed = true;
        }
    }

    /// Feed the presented-frame count after a presentation. Returns the update to mark presented
    /// once a frame presented after the scan that saw it complete.
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
