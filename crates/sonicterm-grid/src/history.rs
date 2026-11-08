//! A screen's history rows and the running totals of what they retain.
//!
//! Retention used to sum every history row whenever it asked what the grid held: on the scroll path
//! every 512 rows, and again after each 64-row block a trim dropped, all under the parser lock.
//! [`History`] keeps those sums as it changes instead. Its rows are reachable only through methods
//! that keep the totals exact, so no caller can change a row's storage behind the totals' back.

use std::collections::VecDeque;

use crate::line::Line;

/// What one history row retains, in the three figures retention reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RowCost {
    /// Reserved storage bytes: [`Line::approx_capacity_byte_size`].
    pub(crate) capacity: usize,
    /// Rare-attribute box bytes: [`Line::fat_attribute_bytes`].
    pub(crate) fat: usize,
    /// Live storage bytes: [`Line::approx_byte_size`].
    pub(crate) live: usize,
}

impl RowCost {
    /// The cost of `row` as it is stored now.
    pub(crate) fn of(row: &Line) -> Self {
        Self {
            capacity: row.approx_capacity_byte_size(),
            fat: row.fat_attribute_bytes(),
            live: row.approx_byte_size(),
        }
    }

    /// `self` plus `other`.
    fn add(self, other: Self) -> Self {
        Self {
            capacity: self.capacity + other.capacity,
            fat: self.fat + other.fat,
            live: self.live + other.live,
        }
    }

    /// `self` minus `other`, which a row's own cost never exceeds.
    fn sub(self, other: Self) -> Self {
        Self {
            capacity: self.capacity - other.capacity,
            fat: self.fat - other.fat,
            live: self.live - other.live,
        }
    }
}

/// History rows, oldest first, with their summed [`RowCost`].
#[derive(Debug, Default)]
pub(crate) struct History {
    /// The rows, oldest first.
    rows: VecDeque<Line>,
    /// The sum of every row's [`RowCost`], kept exact by every method that changes a row.
    totals: RowCost,
}

impl History {
    /// The summed cost of every row, read in O(1).
    pub(crate) fn totals(&self) -> RowCost {
        self.totals
    }

    /// Number of rows.
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no rows.
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row slots the deque has reserved.
    pub(crate) fn capacity(&self) -> usize {
        self.rows.capacity()
    }

    /// The row at `index`, oldest first.
    pub(crate) fn get(&self, index: usize) -> Option<&Line> {
        self.rows.get(index)
    }

    /// Every row, oldest first.
    pub(crate) fn iter(&self) -> std::collections::vec_deque::Iter<'_, Line> {
        self.rows.iter()
    }

    /// Append `row` as the newest, adding its cost.
    pub(crate) fn push_back(&mut self, row: Line) {
        self.totals = self.totals.add(RowCost::of(&row));
        self.rows.push_back(row);
    }

    /// Remove the oldest `count` rows, subtracting their cost first; the caller may reuse or drop
    /// them, and either way they no longer count.
    pub(crate) fn drain_front(
        &mut self,
        count: usize,
    ) -> std::collections::vec_deque::Drain<'_, Line> {
        let removed =
            self.rows.range(..count).map(RowCost::of).fold(RowCost::default(), RowCost::add);
        self.totals = self.totals.sub(removed);
        self.rows.drain(..count)
    }

    /// Run `change` on every row in order, replacing each row's cost with its cost after the
    /// change, and return whether any call returned `true`.
    pub(crate) fn update_each(&mut self, mut change: impl FnMut(&mut Line) -> bool) -> bool {
        let mut any = false;
        let mut totals = RowCost::default();
        for row in &mut self.rows {
            any |= change(row);
            totals = totals.add(RowCost::of(row));
        }
        self.totals = totals;
        any
    }

    /// Release reserved storage in every row, then recount.
    pub(crate) fn shrink_rows(&mut self) {
        self.update_each(|row| {
            row.shrink_capacity_to_fit();
            false
        });
    }

    /// Shrink the deque's own slot reservation when it is at least twice the rows held, or the
    /// history is empty; row costs are unchanged.
    pub(crate) fn compact_spine(&mut self) {
        let len = self.rows.len();
        if self.rows.capacity() > len && (len == 0 || self.rows.capacity() >= len.saturating_mul(2))
        {
            // The spine reserves at least twice the rows held, or holds none: release it.
            self.rows.shrink_to_fit();
        }
    }

    /// Whether the totals equal a fresh sum over every row, for the debug oracle and tests.
    pub(crate) fn totals_match_rows(&self) -> bool {
        self.rows.iter().map(RowCost::of).fold(RowCost::default(), RowCost::add) == self.totals
    }

    /// A history over `rows`, with their cost summed, for tests that build history directly.
    #[cfg(test)]
    pub(crate) fn from_rows(rows: VecDeque<Line>) -> Self {
        let totals = rows.iter().map(RowCost::of).fold(RowCost::default(), RowCost::add);
        Self { rows, totals }
    }
}

impl<'history> IntoIterator for &'history History {
    type Item = &'history Line;
    type IntoIter = std::collections::vec_deque::Iter<'history, Line>;

    /// Every row, oldest first.
    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;
