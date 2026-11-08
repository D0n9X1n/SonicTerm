//! The resize code as it was before history moved to one fused pass, kept only as the reference the
//! differential tests compare the fused resize against. Nothing outside tests calls it.

use super::*;

/// The pre-fusion resize, alternate-screen return, wrap clear and history restamp, verbatim.
impl Grid {
    pub(super) fn reference_leave_alt_screen(&mut self) {
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
                self.reference_content_changed_scrollback_all();
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

    pub(super) fn reference_resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = bounded_grid_size(u64::from(cols), u64::from(rows));
        self.clear_pending_wrap();
        if cols == self.cols && rows == self.rows {
            // When: dimensions are unchanged, preserve storage and revision without reflowing rows.
            return;
        }
        self.reference_clear_all_soft_wraps();
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
            self.reference_content_changed_scrollback_all();
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
            alt.reference_resize(cols, rows);
        }
        self.enforce_alt_memory_budget();
        self.enforce_retained_capacity_budget();
        self.bump();
    }

    pub(super) fn reference_clear_all_soft_wraps(&mut self) {
        let visible_has_wrap = self.visible.iter().any(Line::soft_wrapped_from_previous);
        let history_has_wrap = self.scrollback.iter().any(Line::soft_wrapped_from_previous);
        if !visible_has_wrap && !history_has_wrap {
            // When: `!visible_has_wrap && !history_has_wrap`, leave wrap and content identities unchanged.
            return;
        }
        let seq = self.next_content_seq();
        for index in 0..self.visible.len() {
            if self.visible[index].set_soft_wrapped_from_previous(false) {
                self.visible[index].set_content_seq(seq);
                self.row_content_seq[index] = seq;
                self.mark_row(index as u16);
            }
        }
        for row in &mut self.scrollback {
            if row.set_soft_wrapped_from_previous(false) {
                row.set_content_seq(seq);
            }
        }
        self.bump();
    }

    pub(super) fn reference_content_changed_scrollback_all(&mut self) {
        if self.scrollback.is_empty() {
            // When: `self.scrollback.is_empty()`, avoid advancing the sequence for no history row.
            return;
        }
        let seq = self.next_content_seq();
        for row in &mut self.scrollback {
            row.set_content_seq(seq);
        }
    }
}
