use super::*;

impl Grid {
    /// True if row `row` has been mutated since the last `clear_dirty()`.
    /// Out-of-range rows return `false` (the caller's bounds check has
    /// already failed; nothing for the renderer to do).
    #[inline]
    pub fn is_row_dirty(&self, row: u16) -> bool {
        self.dirty_rows.get(row as usize).copied().unwrap_or(false)
    }

    /// Clear the dirty bitset. Called by the renderer after a successful
    /// frame so the next frame only re-shapes rows that have actually
    /// changed. This is NOT a mutator — it does not bump the revision
    /// counter (an unchanged grid post-clear is still semantically
    /// unchanged from the renderer's point of view).
    #[inline]
    pub fn clear_dirty(&mut self) {
        self.dirty_rows.fill(false);
    }

    /// Clear the dirty bits of exactly the rows in `rows`, keeping every other bit. Slots past
    /// the visible row count are ignored. Like [`Self::clear_dirty`] it does not advance
    /// [`Self::dirty_generation`].
    pub fn clear_dirty_rows(&mut self, rows: &RowSet) {
        for row in rows.iter() {
            if let Some(slot) = self.dirty_rows.get_mut(row) {
                *slot = false;
            }
        }
    }

    /// Clear every dirty row last set at or before `generation`, keeping rows set after it. A
    /// renderer passes the generation it assembled at, so dirt written after assembly survives.
    /// Like [`Self::clear_dirty`] it does not advance [`Self::dirty_generation`].
    pub fn clear_dirty_through(&mut self, generation: u64) {
        for (dirty, stamp) in self.dirty_rows.iter_mut().zip(&self.dirty_stamps) {
            if *stamp <= generation {
                // The row's last dirt is what the renderer drew.
                *dirty = false;
            }
        }
    }

    /// [`Self::clear_dirty_through`] limited to the rows in `rows`; slots past the visible row
    /// count are ignored.
    pub fn clear_dirty_rows_through(&mut self, rows: &RowSet, generation: u64) {
        for row in rows.iter() {
            if let (Some(dirty), Some(stamp)) =
                (self.dirty_rows.get_mut(row), self.dirty_stamps.get(row))
            {
                if *stamp <= generation {
                    // The row's last dirt is what the renderer drew.
                    *dirty = false;
                }
            }
        }
    }

    /// The count of dirty-bit writes so far; see the `dirty_generation` field.
    #[inline]
    pub fn dirty_generation(&self) -> u64 {
        self.dirty_generation
    }

    /// Number of rows currently marked dirty. Useful for tests and for
    /// tracing/diagnostic output.
    #[inline]
    pub fn dirty_count(&self) -> usize {
        self.dirty_rows.iter().filter(|dirty| **dirty).count()
    }

    #[inline]
    pub(super) fn mark_row(&mut self, row: u16) {
        if let Some(slot) = self.dirty_rows.get_mut(row as usize) {
            *slot = true;
            self.dirty_generation = self.dirty_generation.wrapping_add(1);
            if let Some(stamp) = self.dirty_stamps.get_mut(row as usize) {
                *stamp = self.dirty_generation;
            }
        }
    }

    #[inline]
    pub(super) fn mark_all(&mut self) {
        self.dirty_rows.fill(true);
        self.dirty_generation = self.dirty_generation.wrapping_add(1);
        self.dirty_stamps.fill(self.dirty_generation);
    }

    /// Mark every row dirty. Public alias of the internal `mark_all`
    /// helper, exposed so callers that mutate state *outside* the grid
    /// (theme swap, focus transition, selection change, etc.) can tell
    /// the renderer "the whole grid needs to be re-shaped on the next
    /// frame even though no cell content changed". This is the
    /// foundation hook the upcoming RowCache will key off of.
    ///
    /// This is intentionally a no-bump operation: it does not advance
    /// the revision counter because cell contents have not changed —
    /// only the *presentation* invariant did.
    #[inline]
    pub fn mark_all_dirty(&mut self) {
        self.mark_all();
    }

    /// Iterator over row indices currently marked dirty (i.e. the rows
    /// the renderer must re-shape on the next frame). Useful for the
    /// RowCache, for tests, and for tracing/diagnostic output.
    ///
    /// The iterator yields `usize` row indices in ascending order.
    pub fn dirty_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.dirty_rows.iter().enumerate().filter_map(|(index, dirty)| {
            if *dirty {
                Some(index)
            } else {
                // When: `*dirty` is false, omit the clean row from the renderer's work list.
                None
            }
        })
    }

    #[inline]
    pub(super) fn mark_range(&mut self, low: u16, high_inclusive: u16) {
        let low = low as usize;
        let high = (high_inclusive as usize).min(self.dirty_rows.len().saturating_sub(1));
        if low >= self.dirty_rows.len() {
            // When: `low >= self.dirty_rows.len()`, no visible dirty bits exist to set.
            return;
        }
        for slot in &mut self.dirty_rows[low..=high] {
            *slot = true;
        }
        self.dirty_generation = self.dirty_generation.wrapping_add(1);
        for stamp in &mut self.dirty_stamps[low..=high] {
            *stamp = self.dirty_generation;
        }
    }

    /// Current cell-content sequence. Cursor movement and presentation-only
    /// invalidation do not advance it.
    #[inline]
    pub fn content_seq(&self) -> u64 {
        self.content_seq
    }

    /// The content stamp of scrollback-absolute row `abs`: a history line's own stamp, or a
    /// visible row's change stamp. `None` past the visible bottom. A renderer compares it to the
    /// stamp it presented to tell whether the row's content changed since.
    #[must_use]
    pub fn row_content_seq_at_abs(&self, abs: u64) -> Option<u64> {
        let scrollback_rows = self.scrollback.len() as u64;
        if abs < scrollback_rows {
            self.scrollback.get(abs as usize).map(Line::content_seq)
        } else {
            // When: `abs >= scrollback_rows`, the row is visible and its stamp is in `row_content_seq`.
            self.row_content_seq.get((abs - scrollback_rows) as usize).copied()
        }
    }

    /// Current primary/alternate screen incarnation identity.
    #[inline]
    pub fn screen_epoch(&self) -> u64 {
        self.screen_epoch
    }

    /// Count of resizes that changed the bounded rows or columns.
    ///
    /// A resize to the same bounded size and a screen switch leave it unchanged,
    /// so an equal value means no resize has remapped a cell address read under it.
    #[inline]
    pub fn size_generation(&self) -> u64 {
        self.size_generation
    }

    /// Visible row indices whose cell content changed after `seq`.
    ///
    /// The caller maps these live row indices to absolute rows with
    /// `scrollback_len() + row`. Row stamps follow text during primary-screen
    /// scrolling, so an absolute selection continues to follow that text. An
    /// alternate-screen scroll restamps the changed screen positions instead.
    pub fn visible_rows_changed_since(&self, seq: u64) -> impl Iterator<Item = usize> + '_ {
        self.row_content_seq
            .iter()
            .enumerate()
            .filter_map(move |(row, changed_at)| (*changed_at > seq).then_some(row))
    }

    /// Scrollback-absolute rows in history whose content changed after `seq`.
    pub fn scrollback_rows_changed_since(&self, seq: u64) -> impl Iterator<Item = u64> + '_ {
        self.scrollback
            .iter()
            .enumerate()
            .filter_map(move |(row, line)| (line.content_seq() > seq).then_some(row as u64))
    }

    #[inline]
    pub(super) fn next_content_seq(&mut self) -> u64 {
        self.content_seq = self.content_seq.saturating_add(1).min(MAX_LINE_CONTENT_SEQ);
        self.content_seq
    }

    #[inline]
    pub(super) fn content_changed_row(&mut self, row: u16) {
        let seq = self.next_content_seq();
        if let Some(changed_at) = self.row_content_seq.get_mut(row as usize) {
            *changed_at = seq;
        }
        if let Some(line) = self.visible.get_mut(row as usize) {
            line.set_content_seq(seq);
        }
    }

    /// Revoke the wrap `row` passes to the next row after an edit changed `row`'s last column.
    ///
    /// As in xterm and WezTerm, only the wrapping row's final cell carries the
    /// continuation. Edits elsewhere on `row`, and redraws of the continuation row
    /// itself, keep it, so a shell that repaints a wrapped line keeps one logical line.
    pub(super) fn revoke_successor_wrap(&mut self, row: u16) {
        let successor = usize::from(row).saturating_add(1);
        if !self.visible.get(successor).is_some_and(Line::soft_wrapped_from_previous) {
            // When: no `successor` row carries a wrap from `row`, there is no boundary to revoke.
            return;
        }
        let seq = self.next_content_seq();
        self.visible[successor].set_soft_wrapped_from_previous(false);
        self.visible[successor].set_content_seq(seq);
        self.row_content_seq[successor] = seq;
        self.mark_row(u16::try_from(successor).unwrap_or(u16::MAX));
    }

    pub(super) fn set_soft_wrapped_from_previous(&mut self, row: u16, wrapped: bool) {
        let index = usize::from(row);
        if !self
            .visible
            .get_mut(index)
            .is_some_and(|line| line.set_soft_wrapped_from_previous(wrapped))
        {
            // When: `row` is absent or already has `wrapped`, preserve all change identities.
            return;
        }
        let seq = self.next_content_seq();
        self.visible[index].set_content_seq(seq);
        self.row_content_seq[index] = seq;
        self.mark_row(row);
        self.bump();
    }

    pub(super) fn clear_soft_wraps_in_visible_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.visible.len());
        if start >= end || !self.visible.range(start..end).any(Line::soft_wrapped_from_previous) {
            // When: the clamped range is empty or carries no wrap bit, preserve its identities.
            return;
        }
        let seq = self.next_content_seq();
        for index in start..end {
            if self.visible[index].set_soft_wrapped_from_previous(false) {
                self.visible[index].set_content_seq(seq);
                self.row_content_seq[index] = seq;
                self.mark_row(index as u16);
            }
        }
        self.bump();
    }

    pub(super) fn clear_all_soft_wraps(&mut self) {
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

    #[inline]
    pub(super) fn content_changed_range(&mut self, low: u16, high_inclusive: u16) {
        if self.row_content_seq.is_empty() {
            // When: `self.row_content_seq.is_empty()`, there is no content range to advance.
            return;
        }
        let low = low as usize;
        let high = (high_inclusive as usize).min(self.row_content_seq.len() - 1);
        if low > high {
            // When: `low > high`, the requested visible-row range is empty.
            return;
        }
        let seq = self.next_content_seq();
        for row in low..=high {
            self.row_content_seq[row] = seq;
            self.visible[row].set_content_seq(seq);
        }
    }

    #[inline]
    pub(super) fn content_changed_all(&mut self) {
        if self.row_content_seq.is_empty() {
            // When: `self.row_content_seq.is_empty()`, avoid advancing the sequence for no row.
            return;
        }
        let seq = self.next_content_seq();
        for (changed_at, row) in self.row_content_seq.iter_mut().zip(self.visible.iter_mut()) {
            *changed_at = seq;
            row.set_content_seq(seq);
        }
    }

    pub(super) fn content_changed_scrollback_all(&mut self) {
        if self.scrollback.is_empty() {
            // When: `self.scrollback.is_empty()`, avoid advancing the sequence for no history row.
            return;
        }
        let seq = self.next_content_seq();
        for row in &mut self.scrollback {
            row.set_content_seq(seq);
        }
    }

    /// This grid's wrapping revision for coarse rendered-state change detection.
    ///
    /// A fresh grid starts at zero. Mutation paths that affect rendered terminal
    /// state advance it, while dirty-only invalidation such as
    /// [`Self::mark_all_dirty`] may leave it unchanged. Renderers compare the
    /// value with their last observation; ordering is meaningful only between
    /// values from the same [`Grid`] and only until `u64` wraparound.
    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[inline]
    pub(super) fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

#[cfg(test)]
#[path = "grid_dirty_tests.rs"]
mod grid_dirty_tests;
