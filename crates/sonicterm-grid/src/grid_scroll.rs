use super::*;

impl Grid {
    /// Scroll the visible region up by `line_count` lines, pushing the topmost rows
    /// into scrollback.
    ///
    /// This is O(n) in `line_count` (the number of lines scrolled) rather than
    /// O(rows × cols): `pop_front` rotates the visible deque's ring head. With
    /// scrollback disabled or already at capacity, an existing row is recycled
    /// as the new blank line. While scrollback is still growing, the popped row
    /// enters history and a new blank [`Line`] is allocated.
    pub fn scroll_up(&mut self, line_count: u16) {
        self.scroll_up_with(line_count, Cell::default());
    }

    /// Scroll the visible region up by `line_count` lines, filling newly exposed rows
    /// with `fill`.
    pub fn scroll_up_with(&mut self, line_count: u16, fill: Cell) {
        self.clear_pending_wrap();
        let cols = self.cols as usize;
        let line_count = usize::from(line_count).min(self.visible.len());
        if line_count == 0 {
            // When: `line_count == 0`, the clamped scroll request has no effect.
            return;
        }
        let changed_at = self.next_content_seq();
        for _ in 0..line_count {
            let Some(mut row) = self.visible.pop_front() else {
                // When: the visible deque pop returns `None`, stop rather than inventing rows.
                break;
            };
            self.lines_scrolled = self.lines_scrolled.saturating_add(1);
            // A line assigned through `row_mut` can carry an older stamp than its position;
            // keep the newer one so history still reports the change once the row leaves.
            if let Some(stamp) = self.row_content_seq.pop_front() {
                row.set_content_seq(row.content_seq().max(stamp));
            }
            // With scrollback disabled, recycle the row as the new blank line.
            // Absolute row numbers stay fixed, so all visible positions are
            // restamped after the rotation below.
            if self.scrollback_limit == 0 {
                // When: `self.scrollback_limit == 0`, recycle the row into the visible grid.
                for cell in row.iter_mut() {
                    *cell = fill.clone();
                }
                row.resize(cols, fill.clone());
                row.set_soft_wrapped_from_previous(false);
                row.set_content_seq(changed_at);
                self.visible.push_back(row);
                self.row_content_seq.push_back(changed_at);
                continue;
            }
            // Store the ejected row compactly: a uniform row as one cluster, otherwise its cells up
            // to the last that differs from its fill. Either releases the row's old buffer.
            let released = row.compress_releasing().or_else(|| row.try_trim());
            if released.is_some() {
                // the row was compressed or trimmed, its new small buffer is one allocation.
                self.row_storage_allocs = self.row_storage_allocs.saturating_add(1);
            }
            let mut blank = if let Some(cells) = released {
                // the eject released a `cols`-wide buffer, it becomes the blank row.
                if self.scrollback.len() >= self.scrollback_limit {
                    // history is at capacity, drop its oldest row; that frees, not allocates.
                    drop(self.drain_scrollback_prefix(1));
                }
                // Every released cell is a copy of the ejected row's fill (a trim moves out the stored
                // prefix, a compression keeps the whole uniform row), so when that fill is the scroll's
                // fill the copies are kept and only the missing columns are cloned below.
                let mut cells = cells;
                debug_assert!(cells.windows(2).all(|pair| pair[0] == pair[1]));
                if !cells.first().is_some_and(|cell| crate::line::same_as_fill(cell, &fill)) {
                    // the released copies differ from the scroll's fill, so they are cleared before refilling.
                    cells.clear();
                }
                cells.truncate(cols);
                if cells.capacity() < cols {
                    // the released buffer was narrower than `cols`, growing it allocated.
                    cells.reserve_exact(cols - cells.len());
                    self.row_storage_allocs = self.row_storage_allocs.saturating_add(1);
                }
                Line::from_flat(cells)
            } else if self.scrollback.len() >= self.scrollback_limit {
                // When: `self.scrollback.len() >= self.scrollback_limit` with nothing released, recycle the oldest row unread.
                let oldest = self.drain_scrollback_prefix(1).next();
                // PANIC: safe — `len >= limit >= 1` here (limit == 0 handled above), and a non-empty
                // VecDeque always yields `Some`. `>=` holds an over-limit history steady.
                let mut recycled = oldest.unwrap();
                if recycled.clear_for_reuse(cols) {
                    // the recycled row was compact or too narrow, its new buffer allocated.
                    self.row_storage_allocs = self.row_storage_allocs.saturating_add(1);
                }
                recycled
            } else {
                // When: history is below capacity and nothing was released, allocate a blank row.
                self.row_storage_allocs = self.row_storage_allocs.saturating_add(1);
                Line::from_flat(Vec::with_capacity(cols))
            };
            blank.resize(cols, fill.clone());
            self.scrollback.push_back(row);
            blank.set_content_seq(changed_at);
            self.visible.push_back(blank);
            self.row_content_seq.push_back(changed_at);
        }
        if self.scrollback_limit == 0 {
            // With no history, absolute rows are fixed screen positions. Every
            // one received different content after the rotation, not just the
            // newly blank row at the bottom.
            for (row_seq, row) in self.row_content_seq.iter_mut().zip(self.visible.iter_mut()) {
                *row_seq = changed_at;
                row.set_content_seq(changed_at);
            }
        }
        // Enforce the retained budget on the scroll path, amortized.
        //
        // Enforcement previously ran only on alt-screen transitions and
        // resize, so a grid growing through ordinary output was never checked
        // at all. Measured: 63 MiB retained against a 24 MiB budget at 80
        // columns with linked cells, reached by `cat` alone with no resize.
        //
        // Amortized because the check walks every row: per-scroll it would
        // make a long `cat` quadratic in history depth. One walk per
        // `ROWS_BETWEEN_BUDGET_CHECKS` scrolls bounds the overshoot to that
        // many rows while keeping the scroll path linear.
        self.rows_since_budget_check = self.rows_since_budget_check.saturating_add(line_count);
        if self.rows_since_budget_check >= ROWS_BETWEEN_BUDGET_CHECKS {
            self.rows_since_budget_check = 0;
            let budget_bytes = MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>();
            self.trim_scrollback_to_reported_budget(budget_bytes);
        }

        // Every row's content shifted up — the entire visible region
        // changed identity, so every cached span set is stale. The dirty
        // bitset is keyed on visible-row index (0..rows), not by row
        // identity, so a full mark_all is the simplest correct option
        // and costs nothing at terminal row counts (~40).
        self.mark_all();
        self.bump();
    }

    /// Scroll the visible region DOWN by `line_count` lines: every row
    /// moves down by `line_count`, the topmost `line_count` rows become
    /// blank, and the bottom `line_count` rows fall off the end of the
    /// visible region.
    ///
    /// Scrollback is NOT touched (this is the inverse of `scroll_up`
    /// and is only meaningful for alt-screen / DECSTBM use). Marks
    /// every visible row dirty since rows shifted identity.
    pub fn scroll_down(&mut self, line_count: u16) {
        self.scroll_down_with(line_count, Cell::default());
    }

    /// Scroll the visible region down by `line_count` lines, filling newly exposed rows
    /// with `fill`.
    pub fn scroll_down_with(&mut self, line_count: u16, fill: Cell) {
        self.clear_pending_wrap();
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        if rows == 0 {
            // When: `rows == 0`, no visible screen region exists to shift down.
            return;
        }
        let line_count = (line_count as usize).min(rows);
        if line_count == 0 {
            // When: `line_count == 0`, preserve row identity and revision.
            return;
        }
        for _ in 0..line_count {
            // Drop bottom row, recycle it as the new blank top row.
            let Some(mut row) = self.visible.pop_back() else {
                // When: the visible deque pop returns `None`, stop rather than inventing rows.
                break;
            };
            for cell in row.iter_mut() {
                *cell = fill.clone();
            }
            row.resize(cols, fill.clone());
            row.set_soft_wrapped_from_previous(false);
            self.visible.push_front(row);
        }
        self.clear_soft_wraps_in_visible_range(0, rows);
        // Every row's identity shifted at fixed absolute screen positions.
        self.mark_all();
        self.content_changed_all();
        self.bump();
    }

    /// Scroll a sub-region `[top, bottom]` (inclusive, visible-row
    /// coordinates) UP by `line_count` lines. Rows above `top` and below
    /// `bottom` are left untouched; rows inside the region shift up
    /// by `line_count` and the bottom `line_count` rows of the region become blank.
    /// Proper sub-regions do not touch scrollback. A region covering the whole
    /// visible grid delegates to the full-screen scroll path, so ejected rows
    /// enter history when scrollback is enabled.
    ///
    /// Every row in `[top, bottom]` is marked dirty: even rows whose
    /// content is "the same string they had two scrolls ago" must be
    /// invalidated because the `LineQuadCache` is keyed on
    /// `(pane_id, abs_row, content-hash)` — if the dirty bit is not
    /// set for a moved-into row whose new content happens to collide
    /// with a previously-cached hash for the same `abs_row`, the
    /// renderer would replay stale quads. Marking the entire region
    /// dirty is the simple correct option and costs nothing at
    /// terminal row counts.
    pub fn scroll_region_up(&mut self, top: u16, bottom: u16, line_count: u16) {
        self.scroll_region_up_with(top, bottom, line_count, Cell::default());
    }

    /// Scroll a sub-region up, filling newly exposed rows with `fill`.
    pub fn scroll_region_up_with(&mut self, top: u16, bottom: u16, line_count: u16, fill: Cell) {
        self.clear_pending_wrap();
        let rows = self.rows as usize;
        if rows == 0 {
            // When: `rows == 0`, the requested scroll region is empty.
            return;
        }
        let top_i = top as usize;
        let bot_i = (bottom as usize).min(rows.saturating_sub(1));
        if top_i > bot_i {
            // When: `top_i > bot_i`, reject the invalid scroll region.
            return;
        }
        // When the scroll region covers the entire visible grid, the
        // semantic operation is identical to a full-screen scroll —
        // and that means ejected lines MUST be pushed into scrollback.
        // Without this routing, a DECSTBM that explicitly sets margins
        // equal to the full screen (e.g. `ESC[1;Nr`, or the historical
        // `ESC[r` lowering bug where omitted params were stored as
        // `Some(0)..Some(rows-1)`) silently drops every line that
        // scrolls off the top — breaking interactive TUIs like Claude
        // Code that briefly set full-screen margins and then redraw,
        // leaving stale text bleeding through behind their UI. Fixes
        // (and the real cause).
        if top_i == 0 && bot_i == rows.saturating_sub(1) {
            // When: `top_i == 0` and `bot_i` is the last row, route ejected rows to history.
            self.scroll_up_with(line_count, fill);
            return;
        }
        let region_len = bot_i - top_i + 1;
        let line_count = (line_count as usize).min(region_len);
        if line_count == 0 {
            // When: `line_count == 0`, preserve every row in the scroll region.
            return;
        }
        // Shift content up by `line_count` within the region.
        for row in top_i..=bot_i {
            let src = row + line_count;
            if src <= bot_i {
                self.visible.swap(row, src);
            } else {
                // When: `src > bot_i`, clear the exposed destination with no source row.
                // Clear rows that have no source.
                for cell in self.visible[row].iter_mut() {
                    *cell = fill.clone();
                }
            }
        }
        // Some rows ended up with stale data after the swaps in the
        // bottom `line_count` slots — explicitly clear them.
        let blank_start = bot_i + 1 - line_count;
        for row in blank_start..=bot_i {
            for cell in self.visible[row].iter_mut() {
                *cell = fill.clone();
            }
        }
        let bottom = bottom.min(self.rows.saturating_sub(1));
        self.clear_soft_wraps_in_visible_range(top_i, bot_i.saturating_add(2));
        self.mark_range(top, bottom);
        self.content_changed_range(top, bottom);
        self.bump();
    }

    /// Scroll a sub-region `[top, bottom]` (inclusive, visible-row
    /// coordinates) DOWN by `line_count` lines. Mirror of [`Self::scroll_region_up`];
    /// see that doc for the dirty-bit / cache-invalidation rationale.
    pub fn scroll_region_down(&mut self, top: u16, bottom: u16, line_count: u16) {
        self.scroll_region_down_with(top, bottom, line_count, Cell::default());
    }

    /// Scroll a sub-region down, filling newly exposed rows with `fill`.
    pub fn scroll_region_down_with(&mut self, top: u16, bottom: u16, line_count: u16, fill: Cell) {
        self.clear_pending_wrap();
        let rows = self.rows as usize;
        if rows == 0 {
            // When: `rows == 0`, the requested scroll region is empty.
            return;
        }
        let top_i = top as usize;
        let bot_i = (bottom as usize).min(rows.saturating_sub(1));
        if top_i > bot_i {
            // When: `top_i > bot_i`, reject the invalid scroll region.
            return;
        }
        let region_len = bot_i - top_i + 1;
        let line_count = (line_count as usize).min(region_len);
        if line_count == 0 {
            // When: `line_count == 0`, preserve every row in the scroll region.
            return;
        }
        // Shift content down by `line_count` within the region (work from the
        // bottom so we don't clobber sources).
        let mut row = bot_i + 1;
        while row > top_i {
            row -= 1;
            if row >= top_i + line_count {
                let src = row - line_count;
                self.visible.swap(row, src);
            } else {
                // When: `row < top_i + line_count`, clear the exposed destination with no source row.
                for cell in self.visible[row].iter_mut() {
                    *cell = fill.clone();
                }
            }
        }
        // Clear the top `line_count` rows of the region.
        for row in top_i..(top_i + line_count).min(bot_i + 1) {
            for cell in self.visible[row].iter_mut() {
                *cell = fill.clone();
            }
        }
        let bottom = bottom.min(self.rows.saturating_sub(1));
        self.clear_soft_wraps_in_visible_range(top_i, bot_i.saturating_add(2));
        self.mark_range(top, bottom);
        self.content_changed_range(top, bottom);
        self.bump();
    }
}

#[cfg(test)]
#[path = "grid_scroll_tests.rs"]
mod grid_scroll_tests;
