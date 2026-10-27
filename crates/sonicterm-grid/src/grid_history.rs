use super::*;

impl Grid {
    /// Erase primary history without changing live cells, cursor state, or configured limits.
    pub fn clear_scrollback(&mut self) {
        if !self.is_alt() {
            // A primary clear acknowledges every budget trim so far, even with no history left.
            self.budget_trimmed_cleared = self.budget_trimmed_rows;
        }
        if self.is_alt() || self.scrollback.is_empty() {
            // When: `self.is_alt()` or `self.scrollback.is_empty()`, preserve both screens and every change identity.
            return;
        }
        drop(self.drain_scrollback_prefix(self.scrollback.len()));
        self.rows_since_budget_check = 0;
        compact_scrollback_capacity(&mut self.scrollback);
        // Historical viewports resolve to live rows, so repaint without changing their content stamps.
        self.mark_all();
        self.bump();
    }

    pub(super) fn drain_scrollback_prefix(
        &mut self,
        count: usize,
    ) -> std::collections::vec_deque::Drain<'_, Line> {
        let removed = count as u64;
        self.scrollback_evicted = self.scrollback_evicted.saturating_add(removed);
        self.prompts.retain_mut(|prompt| {
            if prompt.start_row < removed {
                // When: the prompt start was evicted, its remaining output cannot identify that prompt.
                return false;
            }
            prompt.start_row -= removed;
            prompt.end_row = prompt.end_row.map(|row| row.saturating_sub(removed));
            true
        });
        // Returning the drain lets the scroll path reuse an evicted row without allocating a replacement.
        self.scrollback.drain(..count)
    }

    /// Number of rows currently stored in the scrollback buffer.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Number of scrollback rows stored trimmed (stored prefix plus one fill cell).
    #[doc(hidden)]
    pub fn scrollback_trimmed_rows(&self) -> usize {
        self.scrollback.iter().filter(|row| row.is_trimmed()).count()
    }

    /// Row-storage buffers (`Vec<Cell>` or `Vec<Cluster>`) the scroll path created or grew since
    /// this grid was built; attribute clones of the fill are not counted.
    #[doc(hidden)]
    pub fn row_storage_allocs(&self) -> u64 {
        self.row_storage_allocs
    }

    /// debug introspection — count (Cluster, other) lines in
    /// scrollback; trimmed and flat rows are both "other".
    #[doc(hidden)]
    pub fn scrollback_storage_breakdown(&self) -> (usize, usize) {
        let mut cluster = 0usize;
        let mut flat = 0usize;
        for row in &self.scrollback {
            if row.is_clustered() {
                cluster += 1;
            } else {
                // When: the row is not compressed as a cluster, count it as flat storage.
                flat += 1;
            }
        }
        (cluster, flat)
    }

    /// sum of per-row `approx_byte_size` across scrollback. This
    /// is a payload-only metric used by internal compression policy tests.
    /// User-visible heap reporting should use [`Self::scrollback_heap_bytes`]
    /// so it counts reserved `Vec::capacity()` bytes and container overhead.
    #[doc(hidden)]
    pub fn scrollback_approx_bytes(&self) -> usize {
        self.scrollback.iter().map(|row| row.approx_byte_size()).sum()
    }

    /// Return the number of row slots reserved by the scrollback container.
    #[doc(hidden)]
    pub fn scrollback_capacity(&self) -> usize {
        self.scrollback.capacity()
    }

    /// Approximate the scrollback heap footprint for diagnostics.
    ///
    /// Counts the scrollback `VecDeque` ring, reserved outer row slots, one
    /// inner `Vec` header per live row, plus each inner storage buffer's
    /// reserved capacity. This intentionally reports user-visible heap shape
    /// rather than the smaller payload-only byte count used by compaction
    /// decisions.
    #[doc(hidden)]
    pub fn scrollback_heap_bytes(&self) -> usize {
        std::mem::size_of::<std::collections::VecDeque<Line>>()
            + self.scrollback_capacity() * std::mem::size_of::<Line>()
            + self.scrollback.len() * std::mem::size_of::<Vec<Cell>>()
            + self.scrollback.iter().map(|row| row.approx_capacity_byte_size()).sum::<usize>()
    }

    /// Set the maximum number of scrollback rows retained. When the new
    /// limit is below the current history depth, the oldest rows are dropped
    /// immediately so memory reflects the new cap — this is what makes
    /// `config.terminal.scrollback` actually bound retained history, both at
    /// pane creation and on config hot-reload.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback_requested_limit = limit;
        self.scrollback_limit = if self.is_alt() {
            0
        } else {
            // When: `self.is_alt()` is false, bound primary history by `limit` and grid size.
            bounded_scrollback_rows(self.cols, self.rows, limit)
        };
        if self.scrollback.len() > self.scrollback_limit {
            let excess = self.scrollback.len() - self.scrollback_limit;
            drop(self.drain_scrollback_prefix(excess));
        }
        compact_scrollback_capacity(&mut self.scrollback);
        if let Some(primary) = self.alt_screen.as_mut() {
            primary.set_scrollback_limit(limit);
        }
        self.enforce_alt_memory_budget();
    }

    pub(super) fn enforce_alt_memory_budget(&mut self) {
        let dropped = {
            let Some(primary) = self.alt_screen.as_mut() else {
                // When: no primary screen is saved, there is no alternate-screen budget to enforce.
                return;
            };
            let active_cells = u64::from(self.cols) * u64::from(self.rows);
            let primary_visible_cells = u64::from(primary.cols) * u64::from(primary.rows);
            let remaining_cells = u64::from(MAX_GRID_CELLS)
                .saturating_sub(active_cells.saturating_add(primary_visible_cells));
            let history_rows = usize::try_from(remaining_cells / u64::from(primary.cols.max(1)))
                .unwrap_or(usize::MAX);
            primary.scrollback_limit =
                primary.scrollback_limit.min(history_rows).min(primary.scrollback_requested_limit);
            if primary.scrollback.len() > primary.scrollback_limit {
                let excess = primary.scrollback.len() - primary.scrollback_limit;
                drop(primary.drain_scrollback_prefix(excess));
            }
            compact_scrollback_capacity(&mut primary.scrollback);
            std::mem::take(&mut primary.scrollback_evicted)
        };
        // The saved primary is still part of this grid. Fold its dropped-row
        // count into the active counter so consumers see one monotonic history
        // identity for the pane, regardless of which screen is visible.
        self.scrollback_evicted = self.scrollback_evicted.saturating_add(dropped);
    }
}

#[cfg(test)]
#[path = "grid_history_tests.rs"]
mod grid_history_tests;
