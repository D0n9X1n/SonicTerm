//! Terminal screen grid: cells, attributes, scrollback.

use std::collections::VecDeque;

// Value types live in `sonic-types` so non-engine crates can use them
// without depending on this crate. Re-exported here for source compatibility:
// every existing `use sonic_core::grid::{Cell, CellFlags, Color, Pos}` keeps
// compiling unchanged.
pub use sonic_types::{Cell, CellFlags, Color, Pos};

use crate::hyperlink::HyperlinkId;
use crate::line::Line;

/// A row of cells.
///
/// **PR-B2 (#319):** the public type alias is now `Line`. Public accessors
/// (`row`, `row_mut`, `rows_iter`, `scrollback_row`, `scrollback_iter`,
/// `row_at_abs`) return `&Line` / `&mut Line` directly. Callers index cells
/// via `Line::Index<usize>` / `Index<Range<usize>>`, iterate via
/// `Line::iter`, and use `Line::len`. Helpers that still take `&[Cell]`
/// (e.g. `row_hash`, `row_quad_hash`) get fed `row.as_flat_slice()`.
pub type Row = Line;

/// A single shell prompt region recorded from OSC 133 markers. Rows are
/// expressed in **scrollback-absolute coordinates** — `scrollback_len() +
/// visible_row` at the time the marker was emitted — so the region remains
/// addressable after content scrolls into history. Callers convert back to
/// a visible row by subtracting the current `scrollback_len()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptRegion {
    /// Absolute row of the prompt start (OSC 133 ; A).
    pub start_row: u64,
    /// Absolute row where the command output ended (OSC 133 ; D), if any.
    pub end_row: Option<u64>,
    /// Exit code reported by OSC 133 ; D ; <code>, if any.
    pub exit_code: Option<i32>,
}

/// Maximum number of prompt regions retained per grid. Older ones are
/// discarded FIFO — terminals only need scroll-by-prompt access to the
/// recent past.
pub const PROMPT_REGION_LIMIT: usize = 256;

/// Terminal grid with scrollback.
#[derive(Debug)]
pub struct Grid {
    /// Number of columns in the visible region.
    pub cols: u16,
    /// Number of rows in the visible region.
    pub rows: u16,
    /// Visible region: `rows` rows of `cols` cells.
    ///
    /// `VecDeque` (rather than `Vec`) so that `scroll_up`/`scroll_down`
    /// are O(1) amortized — a single `pop_front` + `push_back` rotates
    /// the ring-buffer head rather than memmove-ing every row. At 200×
    /// rows this turns a 200-row × N-cell memcpy into a pointer bump.
    /// The indexing API is unchanged: `VecDeque` implements `Index<usize>`
    /// and `IntoIterator`, so all existing callers compile untouched.
    visible: VecDeque<Line>,
    /// Scrollback buffer (oldest at front).
    scrollback: VecDeque<Line>,
    scrollback_limit: usize,
    /// Cursor position within the visible region.
    pub cursor: Pos,
    /// Default attributes used for new cells.
    pub default: Cell,
    /// Saved primary screen when the alt screen is active.
    alt_screen: Option<Box<Grid>>,
    /// Monotonically increasing counter bumped by every mutator. Renderers
    /// can compare the current revision with their last-observed value to
    /// skip work when nothing has changed.
    revision: u64,
    /// Per-row dirty bitset. `true` means the row has been mutated since
    /// the last `clear_dirty()` and the renderer must re-shape it; `false`
    /// means the renderer may reuse its cached span data for that row.
    /// `Vec<bool>` is fine at terminal row counts (~40 typical, ~200 max)
    /// — a BitSet has worse cache behavior at this scale.
    dirty_rows: Vec<bool>,
    /// Prompt regions recorded from OSC 133. Oldest first.
    prompts: VecDeque<PromptRegion>,
}

impl Grid {
    /// Create a new grid with the given column/row count and default settings.
    pub fn new(cols: u16, rows: u16) -> Self {
        let visible = (0..rows).map(|_| make_row(cols)).collect();
        Self {
            cols,
            rows,
            visible,
            scrollback: VecDeque::new(),
            scrollback_limit: 10_000,
            cursor: Pos::default(),
            default: Cell::default(),
            alt_screen: None,
            revision: 0,
            // A freshly created grid is fully dirty: the renderer has
            // never seen it. Once it does its first walk and calls
            // clear_dirty(), the flags drop to all-false.
            dirty_rows: vec![true; rows as usize],
            prompts: VecDeque::new(),
        }
    }

    /// True if row `r` has been mutated since the last `clear_dirty()`.
    /// Out-of-range rows return `false` (the caller's bounds check has
    /// already failed; nothing for the renderer to do).
    #[inline]
    pub fn is_row_dirty(&self, r: u16) -> bool {
        self.dirty_rows.get(r as usize).copied().unwrap_or(false)
    }

    /// Clear the dirty bitset. Called by the renderer after a successful
    /// frame so the next frame only re-shapes rows that have actually
    /// changed. This is NOT a mutator — it does not bump the revision
    /// counter (an unchanged grid post-clear is still semantically
    /// unchanged from the renderer's point of view).
    #[inline]
    pub fn clear_dirty(&mut self) {
        for d in &mut self.dirty_rows {
            *d = false;
        }
    }

    /// Number of rows currently marked dirty. Useful for tests and for
    /// tracing/diagnostic output.
    #[inline]
    pub fn dirty_count(&self) -> usize {
        self.dirty_rows.iter().filter(|d| **d).count()
    }

    #[inline]
    fn mark_row(&mut self, r: u16) {
        if let Some(slot) = self.dirty_rows.get_mut(r as usize) {
            *slot = true;
        }
    }

    #[inline]
    fn mark_all(&mut self) {
        for d in &mut self.dirty_rows {
            *d = true;
        }
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
        self.dirty_rows.iter().enumerate().filter_map(|(i, d)| if *d { Some(i) } else { None })
    }

    #[inline]
    fn mark_range(&mut self, lo: u16, hi_inclusive: u16) {
        let lo = lo as usize;
        let hi = (hi_inclusive as usize).min(self.dirty_rows.len().saturating_sub(1));
        if lo >= self.dirty_rows.len() {
            return;
        }
        for slot in &mut self.dirty_rows[lo..=hi] {
            *slot = true;
        }
    }

    /// Monotonic revision counter, bumped by every mutator. A fresh grid
    /// is at revision 0; the first content change yields a value > 0.
    /// Renderers can compare this against their last-observed revision to
    /// skip rebuilding text/quads when nothing has changed.
    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[inline]
    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// True if the alt screen is currently active (primary is saved).
    pub fn is_alt(&self) -> bool {
        self.alt_screen.is_some()
    }

    /// Switch to the alt screen, saving the current visible+scrollback.
    /// No-op if already on the alt screen.
    pub fn enter_alt_screen(&mut self) {
        if self.alt_screen.is_some() {
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
            scrollback_limit: self.scrollback_limit,
            cursor: saved_cursor,
            default: self.default.clone(),
            alt_screen: None,
            revision: 0,
            dirty_rows: vec![true; rows as usize],
            prompts: VecDeque::new(),
        };
        self.alt_screen = Some(Box::new(saved));
        self.mark_all();
        self.bump();
    }

    /// Leave the alt screen, restoring the saved primary screen. No-op if
    /// not on the alt screen.
    pub fn leave_alt_screen(&mut self) {
        let Some(saved) = self.alt_screen.take() else {
            return;
        };
        let saved = *saved;
        self.visible = saved.visible;
        self.scrollback = saved.scrollback;
        self.cursor = saved.cursor;
        if saved.cols != self.cols || saved.rows != self.rows {
            let cols = self.cols;
            let rows = self.rows;
            for row in &mut self.visible {
                row.resize(cols as usize, Cell::default());
            }
            if (rows as usize) > self.visible.len() {
                while self.visible.len() < rows as usize {
                    self.visible.push_back(make_row(cols));
                }
            } else {
                self.visible.truncate(rows as usize);
            }
            self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
            self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        }
        self.mark_all();
        self.bump();
    }

    /// Resize the grid to `cols × rows`. Existing rows are clipped or
    /// padded with default cells.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        // Reflow: a very basic implementation — clip or pad.
        for row in &mut self.visible {
            row.resize(cols as usize, Cell::default());
        }
        if rows > self.rows {
            for _ in self.rows..rows {
                self.visible.push_back(make_row(cols));
            }
        } else {
            self.visible.truncate(rows as usize);
        }
        self.cols = cols;
        self.rows = rows;
        self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        // Re-size the dirty bitset to the new row count, then mark
        // everything — any geometry change forces a full re-render.
        self.dirty_rows.resize(rows as usize, true);
        self.mark_all();
        if let Some(alt) = self.alt_screen.as_mut() {
            alt.resize(cols, rows);
        }
        self.bump();
    }

    /// Borrow a visible row.
    #[inline]
    pub fn row(&self, r: u16) -> &Row {
        &self.visible[r as usize]
    }

    /// Mutably borrow a visible row.
    #[inline]
    pub fn row_mut(&mut self, r: u16) -> &mut Row {
        &mut self.visible[r as usize]
    }

    /// Iterate visible rows.
    pub fn rows_iter(&self) -> impl Iterator<Item = &Row> {
        self.visible.iter()
    }

    /// Borrow a scrollback row by index (0 = oldest). Returns `None` if out
    /// of range.
    #[inline]
    pub fn scrollback_row(&self, r: usize) -> Option<&Row> {
        self.scrollback.get(r)
    }

    /// Iterate scrollback rows from oldest to newest.
    pub fn scrollback_iter(&self) -> impl Iterator<Item = &Row> {
        self.scrollback.iter()
    }

    /// Borrow the row at scrollback-absolute index `abs`. Returns `None`
    /// if `abs` lies past the bottom of the visible region. Rows inside
    /// scrollback come from the saved backing store; rows ≥ `scrollback_len`
    /// come from the live visible buffer.
    ///
    /// Used by the renderer when the viewport is scrolled away from the
    /// live bottom (e.g. after `ScrollToPrevPrompt`) so the displayed
    /// rows come from history rather than the live shell output.
    #[inline]
    pub fn row_at_abs(&self, abs: u64) -> Option<&Row> {
        let sb = self.scrollback.len() as u64;
        if abs < sb {
            self.scrollback.get(abs as usize)
        } else {
            let r = (abs - sb) as usize;
            self.visible.get(r)
        }
    }

    /// Put a character at cursor, advancing cursor by character width.
    pub fn put_char(&mut self, ch: char, fg: Color, bg: Color, flags: CellFlags) {
        self.put_char_linked(ch, fg, bg, flags, None);
    }

    /// Put a character at cursor, also tagging the cell(s) with an optional
    /// hyperlink id.
    pub fn put_char_linked(
        &mut self,
        ch: char,
        fg: Color,
        bg: Color,
        flags: CellFlags,
        hyperlink: Option<HyperlinkId>,
    ) {
        // ASCII printable fast-path: every codepoint in 0x20..=0x7E has
        // unicode_width == 1 unconditionally. `UnicodeWidthChar::width`
        // performs a binary search through several MiB of tables — for
        // shell output (overwhelmingly ASCII) this dominates the
        // parser hot path. Skipping it here recovers ~30% of
        // parse_ns_per_byte on bursty scroll workloads.
        let width = if (ch as u32) >= 0x20 && (ch as u32) <= 0x7E {
            1u16
        } else {
            unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16
        };
        if width == 0 {
            // Zero-width codepoint (ZWJ U+200D, combining marks, etc.).
            // It must NOT advance the cursor, but it IS part of the
            // current cluster — the shaper needs to see it together
            // with the lead char so ZWJ sequences like 👨‍👩‍👧 actually
            // compose. Attach it to the previous lead cell on this row.
            // If the cursor is at column 0 (no prior cell on this row)
            // there is no cluster to attach to and the codepoint is
            // dropped — matches every other terminal's behavior.
            if self.cursor.col == 0 {
                return;
            }
            let r = self.cursor.row as usize;
            // Walk back past any WIDE_CONT cells to find the lead.
            let mut c = self.cursor.col as usize - 1;
            while c > 0 && self.visible[r][c].flags.contains(CellFlags::WIDE_CONT) {
                c -= 1;
            }
            if self.visible[r][c].flags.contains(CellFlags::WIDE_CONT) {
                // Reached col 0 still on a continuation — nothing to
                // attach to safely.
                return;
            }
            let lead = &mut self.visible[r][c];
            let mut s = match lead.take_extras() {
                Some(boxed) => String::from(boxed),
                None => String::new(),
            };
            s.push(ch);
            lead.set_extras(Some(s.into_boxed_str()));
            self.mark_row(self.cursor.row);
            self.bump();
            return;
        }
        if self.cursor.col + width > self.cols {
            self.linefeed();
            self.cursor.col = 0;
        }
        let (r, c) = (self.cursor.row as usize, self.cursor.col as usize);
        let cell_flags = if width == 2 { flags | CellFlags::WIDE } else { flags };
        let mut lead = Cell::plain(ch, fg, bg, cell_flags);
        lead.set_hyperlink(hyperlink);
        self.visible[r][c] = lead;
        if width == 2 && c + 1 < self.cols as usize {
            let mut cont = Cell::plain(' ', fg, bg, flags | CellFlags::WIDE_CONT);
            cont.set_hyperlink(hyperlink);
            self.visible[r][c + 1] = cont;
        }
        self.cursor.col += width;
        self.mark_row(r as u16);
        self.bump();
    }

    /// Move the cursor to column 0 of the current row.
    pub fn carriage_return(&mut self) {
        self.cursor.col = 0;
        let r = self.cursor.row;
        self.mark_row(r);
        self.bump();
    }

    /// Advance the cursor one row, scrolling the visible region if needed.
    pub fn linefeed(&mut self) {
        let old = self.cursor.row;
        if self.cursor.row + 1 >= self.rows {
            // scroll_up already marks every row dirty.
            self.scroll_up(1);
        } else {
            self.cursor.row += 1;
        }
        // Both leaving and arriving rows count as touched (cursor moved
        // through them, even if the cells themselves are unchanged — the
        // renderer may need to redraw the cursor on either).
        self.mark_row(old);
        self.mark_row(self.cursor.row);
        self.bump();
    }

    /// Move the cursor one column to the left (does not erase).
    pub fn backspace(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
        let r = self.cursor.row;
        self.mark_row(r);
        self.bump();
    }

    /// Advance the cursor to the next tab stop (8-column tabs).
    pub fn tab(&mut self) {
        let next = ((self.cursor.col / 8) + 1) * 8;
        self.cursor.col = next.min(self.cols.saturating_sub(1));
        let r = self.cursor.row;
        self.mark_row(r);
        self.bump();
    }

    /// Scroll the visible region up by `n` lines, pushing the topmost rows
    /// into scrollback.
    ///
    /// This is O(n) in `n` (the number of lines scrolled) rather than
    /// O(rows × cols) — a `pop_front` rotates the `VecDeque`'s ring head
    /// rather than memmove-ing every row down by one. The popped row is
    /// recycled in place: its cells are reset to `Cell::default()` and the
    /// row buffer is pushed onto the back of the deque, avoiding a
    /// per-scroll allocation for the new blank row.
    pub fn scroll_up(&mut self, n: u16) {
        let cols = self.cols as usize;
        for _ in 0..n {
            let Some(mut row) = self.visible.pop_front() else {
                break;
            };
            if self.scrollback_limit == 0 {
                // Scrollback disabled — recycle the row straight back as
                // the new blank line.
                for cell in row.iter_mut() {
                    *cell = Cell::default();
                }
                row.resize(cols, Cell::default());
                self.visible.push_back(row);
                continue;
            }
            // PR-C (#319): try to compress the ejected line into a
            // single Cluster (whole-line uniform attrs). No-op when the
            // line is non-uniform — it stays Flat. Multi-Cluster
            // segmentation of partially-uniform lines is PR-D scope.
            row.try_compress();
            if self.scrollback.len() == self.scrollback_limit {
                // Reuse the oldest scrollback row as the new blank line
                // (avoids both an allocation and a free).
                // PANIC: safe — the surrounding `if` proves the deque is at
                // capacity, so `len >= scrollback_limit >= 1`. We only enter
                // this branch when `scrollback_limit > 0`, and a non-empty
                // VecDeque always yields `Some` from `pop_front`.
                let mut recycled = self.scrollback.pop_front().unwrap();
                // Recycled may itself have been compressed when it was
                // ejected — force back to Flat before we mutate cells.
                recycled.ensure_flat();
                for cell in recycled.iter_mut() {
                    *cell = Cell::default();
                }
                recycled.resize(cols, Cell::default());
                self.scrollback.push_back(row);
                self.visible.push_back(recycled);
            } else {
                self.scrollback.push_back(row);
                self.visible.push_back(make_row(self.cols));
            }
        }
        // Every row's content shifted up — the entire visible region
        // changed identity, so every cached span set is stale. The dirty
        // bitset is keyed on visible-row index (0..rows), not by row
        // identity, so a full mark_all is the simplest correct option
        // and costs nothing at terminal row counts (~40).
        self.mark_all();
        self.bump();
    }

    /// Scroll the visible region DOWN by `n` lines: every row at
    /// `r` moves to `r + n`, the topmost `n` rows become blank, the
    /// bottom `n` rows fall off the end of the visible region.
    ///
    /// Scrollback is NOT touched (this is the inverse of `scroll_up`
    /// and is only meaningful for alt-screen / DECSTBM use). Marks
    /// every visible row dirty since rows shifted identity.
    pub fn scroll_down(&mut self, n: u16) {
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        if rows == 0 {
            return;
        }
        let n = (n as usize).min(rows);
        for _ in 0..n {
            // Drop bottom row, recycle it as the new blank top row.
            let Some(mut row) = self.visible.pop_back() else {
                break;
            };
            for cell in row.iter_mut() {
                *cell = Cell::default();
            }
            row.resize(cols, Cell::default());
            self.visible.push_front(row);
        }
        // Every row's identity shifted — dirty bitset is keyed on
        // visible-row index, so mark all (same justification as
        // `scroll_up`).
        self.mark_all();
        self.bump();
    }

    /// Scroll a sub-region `[top, bottom]` (inclusive, visible-row
    /// coordinates) UP by `n` lines. Rows above `top` and below
    /// `bottom` are left untouched; rows inside the region shift up
    /// by `n` and the bottom `n` rows of the region become blank.
    /// Scrollback is NOT touched — this is the in-region scroll used
    /// by DECSTBM / CSI S / IND-at-bottom-margin and must not push
    /// into history.
    ///
    /// Every row in `[top, bottom]` is marked dirty: even rows whose
    /// content is "the same string they had two scrolls ago" must be
    /// invalidated because the `LineQuadCache` is keyed on
    /// `(pane_id, abs_row, content-hash)` — if the dirty bit is not
    /// set for a moved-into row whose new content happens to collide
    /// with a previously-cached hash for the same `abs_row`, the
    /// renderer would replay stale quads. Marking the entire region
    /// dirty is the simple correct option and costs nothing at
    /// terminal row counts. Closes #348.
    pub fn scroll_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        let rows = self.rows as usize;
        if rows == 0 {
            return;
        }
        let top_i = top as usize;
        let bot_i = (bottom as usize).min(rows.saturating_sub(1));
        if top_i > bot_i {
            return;
        }
        let region_len = bot_i - top_i + 1;
        let n = (n as usize).min(region_len);
        if n == 0 {
            return;
        }
        // Shift content up by `n` within the region.
        for r in top_i..=bot_i {
            let src = r + n;
            if src <= bot_i {
                self.visible.swap(r, src);
            } else {
                // Clear rows that have no source.
                for cell in self.visible[r].iter_mut() {
                    *cell = Cell::default();
                }
            }
        }
        // Some rows ended up with stale data after the swaps in the
        // bottom `n` slots — explicitly clear them.
        let blank_start = bot_i + 1 - n;
        for r in blank_start..=bot_i {
            for cell in self.visible[r].iter_mut() {
                *cell = Cell::default();
            }
        }
        self.mark_range(top, bottom.min(self.rows.saturating_sub(1)));
        self.bump();
    }

    /// Scroll a sub-region `[top, bottom]` (inclusive, visible-row
    /// coordinates) DOWN by `n` lines. Mirror of [`scroll_region_up`];
    /// see that doc for the dirty-bit / cache-invalidation rationale.
    pub fn scroll_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        let rows = self.rows as usize;
        if rows == 0 {
            return;
        }
        let top_i = top as usize;
        let bot_i = (bottom as usize).min(rows.saturating_sub(1));
        if top_i > bot_i {
            return;
        }
        let region_len = bot_i - top_i + 1;
        let n = (n as usize).min(region_len);
        if n == 0 {
            return;
        }
        // Shift content down by `n` within the region (work from the
        // bottom so we don't clobber sources).
        let mut r = bot_i + 1;
        while r > top_i {
            r -= 1;
            if r >= top_i + n {
                let src = r - n;
                self.visible.swap(r, src);
            } else {
                for cell in self.visible[r].iter_mut() {
                    *cell = Cell::default();
                }
            }
        }
        // Clear the top `n` rows of the region.
        for r in top_i..(top_i + n).min(bot_i + 1) {
            for cell in self.visible[r].iter_mut() {
                *cell = Cell::default();
            }
        }
        self.mark_range(top, bottom.min(self.rows.saturating_sub(1)));
        self.bump();
    }

    /// Erase from cursor to end of line (CSI 0 K).
    pub fn erase_line_to_end(&mut self) {
        let r = self.cursor.row as usize;
        for c in self.cursor.col as usize..self.cols as usize {
            self.visible[r][c] = Cell::default();
        }
        self.mark_row(r as u16);
        self.bump();
    }

    /// Erase from beginning of line to cursor inclusive (CSI 1 K).
    pub fn erase_line_to_start(&mut self) {
        let r = self.cursor.row as usize;
        for c in 0..=(self.cursor.col as usize).min(self.cols as usize - 1) {
            self.visible[r][c] = Cell::default();
        }
        self.mark_row(r as u16);
        self.bump();
    }

    /// Erase the entire current line (CSI 2 K).
    pub fn erase_line(&mut self) {
        let r = self.cursor.row as usize;
        for cell in &mut self.visible[r] {
            *cell = Cell::default();
        }
        self.mark_row(r as u16);
        self.bump();
    }

    /// Erase from cursor to end of screen (CSI 0 J). This is what shells
    /// use to redraw a prompt — they jump to a row, erase below, and
    /// reprint. It must NOT touch rows above the cursor.
    pub fn erase_below(&mut self) {
        self.erase_line_to_end();
        for r in (self.cursor.row as usize + 1)..self.rows as usize {
            for cell in &mut self.visible[r] {
                *cell = Cell::default();
            }
        }
        // Mark cursor.row..rows
        let lo = self.cursor.row;
        let hi = self.rows.saturating_sub(1);
        self.mark_range(lo, hi);
        self.bump();
    }

    /// Erase from start of screen to cursor (CSI 1 J).
    pub fn erase_above(&mut self) {
        for r in 0..self.cursor.row as usize {
            for cell in &mut self.visible[r] {
                *cell = Cell::default();
            }
        }
        self.erase_line_to_start();
        // erase_line_to_start already marked cursor.row; mark 0..cursor.row too.
        let hi = self.cursor.row;
        self.mark_range(0, hi);
        self.bump();
    }

    /// Erase the entire visible screen (CSI 2 J).
    pub fn erase_screen(&mut self) {
        for row in &mut self.visible {
            for cell in row.iter_mut() {
                *cell = Cell::default();
            }
        }
        self.mark_all();
        self.bump();
    }

    /// Erase `n` cells starting at (`row`, `col`), overwriting with the
    /// default (blank) Cell. Cursor unchanged. Used by CSI `X` (ECH).
    /// ECMA-48 §8.3.38: erased cells become BLANK with the current SGR
    /// rendition; we approximate by writing `Cell::default()` which keeps
    /// behaviour aligned with the existing erase_line family.
    pub fn erase_cells(&mut self, row: u16, col: u16, n: usize) {
        if row >= self.rows || col >= self.cols || n == 0 {
            return;
        }
        let r = row as usize;
        let start = col as usize;
        let end = (start + n).min(self.cols as usize);
        for c in start..end {
            self.visible[r][c] = Cell::default();
        }
        self.mark_row(row);
        self.bump();
    }

    /// Insert `n` blank cells at (`row`, `col`); shift trailing cells of
    /// the row right and drop the overflow at the right edge. Used by
    /// CSI `@` (ICH).
    pub fn insert_cells(&mut self, row: u16, col: u16, n: usize) {
        if row >= self.rows || col >= self.cols || n == 0 {
            return;
        }
        let r = row as usize;
        let start = col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - start);
        // Shift right: dest = start+n..cols, src = start..cols-n.
        for dst in (start + n..cols).rev() {
            self.visible[r][dst] = self.visible[r][dst - n].clone();
        }
        for c in start..start + n {
            self.visible[r][c] = Cell::default();
        }
        self.mark_row(row);
        self.bump();
    }

    /// Delete `n` cells at (`row`, `col`); shift trailing cells of the row
    /// left and fill the right edge with blanks. Used by CSI `P` (DCH).
    pub fn delete_cells(&mut self, row: u16, col: u16, n: usize) {
        if row >= self.rows || col >= self.cols || n == 0 {
            return;
        }
        let r = row as usize;
        let start = col as usize;
        let cols = self.cols as usize;
        let n = n.min(cols - start);
        for c in start..cols - n {
            self.visible[r][c] = self.visible[r][c + n].clone();
        }
        for c in cols - n..cols {
            self.visible[r][c] = Cell::default();
        }
        self.mark_row(row);
        self.bump();
    }

    /// Move cursor to (row, col), clamping to grid bounds.
    pub fn goto(&mut self, row: u16, col: u16) {
        let old_row = self.cursor.row;
        self.cursor.row = row.min(self.rows.saturating_sub(1));
        self.cursor.col = col.min(self.cols.saturating_sub(1));
        // Both leaving and arriving rows need re-render so the cursor
        // quad doesn't lag behind.
        self.mark_row(old_row);
        self.mark_row(self.cursor.row);
        self.bump();
    }

    /// Number of rows currently stored in the scrollback buffer.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Absolute row of the cursor (= `scrollback_len() + cursor.row`). Used
    /// by OSC 133 marker recording so prompt regions survive scrolling.
    #[inline]
    pub fn cursor_absolute_row(&self) -> u64 {
        self.scrollback.len() as u64 + self.cursor.row as u64
    }

    /// Record an OSC 133 `A` (prompt-start) marker at the cursor row.
    /// Coalesces consecutive markers on the same row so a shell that emits
    /// the marker more than once per prompt doesn't bloat the buffer.
    pub fn record_prompt_start(&mut self) {
        let row = self.cursor_absolute_row();
        if matches!(self.prompts.back(), Some(p) if p.start_row == row && p.end_row.is_none()) {
            return;
        }
        if self.prompts.len() >= PROMPT_REGION_LIMIT {
            self.prompts.pop_front();
        }
        self.prompts.push_back(PromptRegion { start_row: row, end_row: None, exit_code: None });
    }

    /// Record an OSC 133 `D` (command-end) marker. Updates the most recent
    /// prompt region in place; a stray `D` without a prior `A` is ignored.
    pub fn record_prompt_end(&mut self, exit_code: Option<i32>) {
        let row = self.cursor_absolute_row();
        if let Some(last) = self.prompts.back_mut() {
            last.end_row = Some(row);
            last.exit_code = exit_code;
        }
    }

    /// All recorded prompt regions in chronological order.
    pub fn prompts(&self) -> impl Iterator<Item = &PromptRegion> {
        self.prompts.iter()
    }

    /// Number of recorded prompt regions.
    pub fn prompts_len(&self) -> usize {
        self.prompts.len()
    }

    /// Visible-region row of a prompt region, if it currently lies inside
    /// the visible window. Used by the renderer to draw the gutter caret.
    pub fn prompt_visible_row(&self, p: &PromptRegion) -> Option<u16> {
        let scrollback = self.scrollback.len() as u64;
        let rel = p.start_row.checked_sub(scrollback)?;
        if rel < self.rows as u64 {
            Some(rel as u16)
        } else {
            None
        }
    }

    /// Find the prompt whose absolute start row is the largest one strictly
    /// less than `from_absolute_row`. Used by the "scroll to previous
    /// prompt" action.
    pub fn prompt_before(&self, from_absolute_row: u64) -> Option<&PromptRegion> {
        self.prompts.iter().rev().find(|p| p.start_row < from_absolute_row)
    }

    /// Find the prompt whose absolute start row is the smallest one strictly
    /// greater than `from_absolute_row`. Used by the "scroll to next
    /// prompt" action.
    pub fn prompt_after(&self, from_absolute_row: u64) -> Option<&PromptRegion> {
        self.prompts.iter().find(|p| p.start_row > from_absolute_row)
    }

    /// Set the maximum number of scrollback rows retained.
    #[doc(hidden)]
    #[doc(hidden)]
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback_limit = limit;
    }
}

fn make_row(cols: u16) -> Line {
    Line::flat_filled(cols as usize, Cell::default())
}
