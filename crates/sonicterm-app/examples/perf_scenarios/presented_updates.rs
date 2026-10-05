//! The row-run warm phase's handshake: the role writes update `n` and waits; the probe marks `n`
//! presented only once its grid shows `n` and a later frame has presented, then the role writes
//! `n + 1`. So every warm update is drawn, not merely printed.

use crate::workload::RowRunWorkload;

/// The screen row (0-based) whose text identifies an update: the first body row.
pub(crate) const IDENTIFYING_ROW: u16 = 1;

/// The warm-phase handshake state for one row-run role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresentedUpdates {
    kind: RowRunWorkload,
    count: u32,
    next: u32,
    /// The presented-frame count when the awaited update was first seen in the grid.
    seen_at: Option<u64>,
}

impl PresentedUpdates {
    /// A handshake over updates 0 to `count`-1 of `kind`.
    pub(crate) fn new(kind: RowRunWorkload, count: u32) -> Self {
        Self { kind, count, next: 0, seen_at: None }
    }

    /// Whether every warm update has been marked presented.
    pub(crate) fn done(&self) -> bool {
        self.next >= self.count
    }

    /// The digits of update `update`'s identifying row: only the fields that change, since wide
    /// characters' continuation cells make the rest of the row text host-dependent.
    pub(crate) fn fingerprint(kind: RowRunWorkload, update: u32) -> String {
        kind.body_segments(IDENTIFYING_ROW + 1, update)
            .iter()
            .flat_map(|(_, _, text)| text.chars())
            .filter(char::is_ascii_digit)
            .collect()
    }

    /// Feed one scan: the identifying row's text and the presented-frame count. Returns the update
    /// to mark presented, once its row was seen and a later frame has presented.
    pub(crate) fn observe(&mut self, row_text: &str, frames: u64) -> Option<u32> {
        if self.done() {
            // When: `done`, every warm update is marked and nothing more is.
            return None;
        }
        let shown: String = row_text.chars().filter(char::is_ascii_digit).collect();
        if shown != Self::fingerprint(self.kind, self.next) {
            // When: `shown` is not the awaited update's `fingerprint`, nothing is seen.
            self.seen_at = None;
            return None;
        }
        match self.seen_at {
            Some(seen) if frames > seen => {
                // A frame presented after the update was in the grid, so it was drawn.
                let presented = self.next;
                self.next += 1;
                self.seen_at = None;
                Some(presented)
            }
            Some(_) => None,
            None => {
                // Seen for the first time: wait for a later frame.
                self.seen_at = Some(frames);
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "presented_updates_tests.rs"]
mod presented_updates_tests;
