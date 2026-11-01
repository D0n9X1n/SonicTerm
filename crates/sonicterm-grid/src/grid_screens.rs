use super::*;

impl Grid {
    /// True if the alt screen is currently active (primary is saved).
    pub fn is_alt(&self) -> bool {
        self.alt_screen.is_some()
    }

    /// Switch to the alt screen, saving the current visible and scrollback.
    /// When already active, clears pending autowrap state but preserves the
    /// existing saved primary and alternate contents.
    pub fn enter_alt_screen(&mut self) {
        self.clear_pending_wrap();
        if self.alt_screen.is_some() {
            // When: `self.alt_screen.is_some()`, keep the already-saved primary unchanged.
            return;
        }
        let cols = self.cols;
        let rows = self.rows;
        let saved_visible = std::mem::replace(
            &mut self.visible,
            (0..rows).map(|_| make_row(cols)).collect::<VecDeque<_>>(),
        );
        let saved_scrollback = std::mem::take(&mut self.scrollback);
        let saved_cursor = self.cursor;
        self.cursor = Pos::default();
        let saved = Grid {
            cols,
            rows,
            visible: saved_visible,
            scrollback: saved_scrollback,
            scrollback_requested_limit: self.scrollback_requested_limit,
            scrollback_limit: self.scrollback_limit,
            cursor: saved_cursor,
            default: self.default.clone(),
            alt_screen: None,
            revision: 0,
            content_seq: self.content_seq,
            screen_epoch: self.screen_epoch,
            size_generation: self.size_generation,
            row_content_seq: std::mem::replace(
                &mut self.row_content_seq,
                vec![self.content_seq; rows as usize].into(),
            ),
            scrollback_evicted: 0,
            lines_scrolled: self.lines_scrolled,
            alt_scroll_floor: 0,
            budget_trimmed_rows: 0,
            budget_trimmed_cleared: 0,
            row_storage_allocs: 0,
            rows_since_budget_check: 0,
            dirty_rows: vec![true; rows as usize],
            dirty_generation: 0,
            dirty_stamps: vec![0; rows as usize],
            prompts: std::mem::take(&mut self.prompts),
            autowrap: self.autowrap,
            pending_wrap: false,
        };
        self.alt_screen = Some(Box::new(saved));
        // The alternate screen counts from past the primary's count and every earlier alternate
        // screen's, so neither screen's images are drawn on the other.
        self.lines_scrolled = self
            .lines_scrolled
            .max(self.alt_scroll_floor)
            .saturating_add(Self::ALT_SCREEN_SCROLL_GAP);
        self.screen_epoch = self.screen_epoch.saturating_add(1);
        self.scrollback_limit = 0;
        self.enforce_alt_memory_budget();
        self.enforce_retained_capacity_budget();
        self.mark_all();
        self.content_changed_all();
        self.bump();
    }

    /// Leave the alt screen, restoring the saved primary screen. When no
    /// primary is saved, clears pending autowrap state and otherwise preserves
    /// the current screen.
    pub fn leave_alt_screen(&mut self) {
        self.clear_pending_wrap();
        let Some(saved) = self.alt_screen.take() else {
            // When: no primary screen is saved, the grid is already on its primary screen.
            return;
        };
        let saved = *saved;
        self.screen_epoch = self.screen_epoch.saturating_add(1);
        self.scrollback_evicted = self.scrollback_evicted.saturating_add(saved.scrollback_evicted);
        self.budget_trimmed_rows =
            self.budget_trimmed_rows.saturating_add(saved.budget_trimmed_rows);
        let content_seq = self.content_seq.max(saved.content_seq);
        self.visible = saved.visible;
        self.scrollback = saved.scrollback;
        self.prompts = saved.prompts;
        // The primary screen's scroll count returns, so its images stay beside its text; the
        // alternate screen's last count is kept so the next one starts past it.
        self.alt_scroll_floor = self.lines_scrolled;
        self.lines_scrolled = saved.lines_scrolled;
        self.row_content_seq = saved.row_content_seq;
        self.content_seq = content_seq;
        self.scrollback_requested_limit = saved.scrollback_requested_limit;
        self.scrollback_limit =
            bounded_scrollback_rows(self.cols, self.rows, self.scrollback_requested_limit);
        if self.scrollback.len() > self.scrollback_limit {
            let excess = self.scrollback.len() - self.scrollback_limit;
            drop(self.drain_scrollback_prefix(excess));
        }
        compact_scrollback_capacity(&mut self.scrollback);
        self.cursor = saved.cursor;
        if saved.cols != self.cols || saved.rows != self.rows {
            let cols = self.cols;
            let rows = self.rows;
            for row in &mut self.visible {
                row.resize(cols as usize, Cell::default());
            }
            for row in &mut self.scrollback {
                row.resize(cols as usize, Cell::default());
            }
            if saved.cols != self.cols {
                self.content_changed_scrollback_all();
            }
            if (rows as usize) > self.visible.len() {
                while self.visible.len() < rows as usize {
                    self.visible.push_back(make_row(cols));
                    self.row_content_seq.push_back(self.content_seq);
                }
            } else {
                // When: the restored primary is tall enough, discard rows beyond the active height.
                self.visible.truncate(rows as usize);
                self.row_content_seq.truncate(rows as usize);
            }
            self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
            self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        }
        self.enforce_retained_capacity_budget();
        self.mark_all();
        self.content_changed_all();
        self.bump();
    }

    /// Resize the grid to `cols × rows`. Existing rows are clipped or
    /// padded with default cells.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = bounded_grid_size(u64::from(cols), u64::from(rows));
        self.clear_pending_wrap();
        if cols == self.cols && rows == self.rows {
            // When: dimensions are unchanged, preserve storage and revision without reflowing rows.
            return;
        }
        self.clear_all_soft_wraps();
        self.scrollback_limit = if self.is_alt() {
            0
        } else {
            // When: `self.is_alt()` is false, bound history by the resized grid.
            bounded_scrollback_rows(cols, rows, self.scrollback_requested_limit)
        };
        if self.scrollback.len() > self.scrollback_limit {
            let excess = self.scrollback.len() - self.scrollback_limit;
            drop(self.drain_scrollback_prefix(excess));
        }
        compact_scrollback_capacity(&mut self.scrollback);
        // Drop rows that will not survive before widening columns. Otherwise
        // a tall-to-wide resize can transiently expand thousands of rows only
        // to discard them immediately afterward.
        if rows < self.rows {
            self.visible.truncate(rows as usize);
        }
        // Reflow: a very basic implementation — clip or pad.
        for row in &mut self.visible {
            row.resize(cols as usize, Cell::default());
        }
        // keep scrollback consistent with the new column count.
        // `Line::resize` is now Cluster-preserving, so uniform blank
        // scrollback rows (the bulk of typical scrollback) stay compressed.
        if cols != self.cols {
            for row in &mut self.scrollback {
                row.resize(cols as usize, Cell::default());
            }
            self.content_changed_scrollback_all();
        }
        if rows > self.rows {
            for _ in self.rows..rows {
                self.visible.push_back(make_row(cols));
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.size_generation = self.size_generation.saturating_add(1);
        self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        // Re-size the row metadata to the new row count, then mark everything:
        // geometry and cell content both changed for the visible screen.
        self.dirty_rows.resize(rows as usize, true);
        self.dirty_stamps.resize(rows as usize, 0);
        self.row_content_seq.resize(rows as usize, self.content_seq);
        self.mark_all();
        self.content_changed_all();
        if let Some(alt) = self.alt_screen.as_mut() {
            alt.resize(cols, rows);
        }
        self.enforce_alt_memory_budget();
        self.enforce_retained_capacity_budget();
        self.bump();
    }
}

#[cfg(test)]
#[path = "grid_screens_tests.rs"]
mod grid_screens_tests;
