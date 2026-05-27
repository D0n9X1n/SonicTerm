//! Terminal screen grid: cells, attributes, scrollback.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::hyperlink::HyperlinkId;

/// (row, col) position. (0, 0) is top-left of the visible region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}

/// 24-bit RGB color or an indexed palette slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u16 {
        const BOLD          = 1 << 0;
        const ITALIC        = 1 << 1;
        const UNDERLINE     = 1 << 2;
        const STRIKETHROUGH = 1 << 3;
        const INVERSE       = 1 << 4;
        const DIM           = 1 << 5;
        const HIDDEN        = 1 << 6;
        const BLINK         = 1 << 7;
        /// Wide cell (occupies 2 columns)
        const WIDE          = 1 << 8;
        /// Continuation of a wide cell (right half)
        const WIDE_CONT     = 1 << 9;
    }
}

/// A single grid cell.
///
/// `extras` stores trailing zero-width codepoints (zero-width joiners
/// U+200D and combining marks) that follow the lead `ch` and must be
/// shaped together with it as part of the same cluster. ZWJ sequences
/// like 👨‍👩‍👧 (MAN + ZWJ + WOMAN + ZWJ + GIRL) reach the grid as
/// five separate `put_char` calls; the four zero-width codepoints
/// (ZWJs + each subsequent emoji's invisible joiners are zero-width
/// per `unicode-width`) get appended to the lead cell's `extras` so
/// the shaper sees the full cluster on a single shape pass. Boxed so
/// the common case (no extras) costs one machine word per cell, not
/// the 24-byte footprint of an inline `Vec<char>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub hyperlink: Option<HyperlinkId>,
    /// Trailing zero-width codepoints (ZWJ, combining marks) that
    /// belong to this cluster, encoded as UTF-8. `None` for the
    /// overwhelming majority of cells (plain ASCII, single emoji,
    /// single CJK glyph). `Box<str>` (rather than `String`) keeps the
    /// footprint at two machine words when present and zero
    /// allocations beyond the boxed slice itself.
    pub extras: Option<Box<str>>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: None,
            extras: None,
        }
    }
}

/// A row of cells.
pub type Row = Vec<Cell>;

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
    pub cols: u16,
    pub rows: u16,
    /// Visible region: `rows` rows of `cols` cells.
    visible: Vec<Row>,
    /// Scrollback buffer (oldest at front).
    scrollback: VecDeque<Row>,
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
        let saved_visible =
            std::mem::replace(&mut self.visible, (0..rows).map(|_| make_row(cols)).collect());
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
                    self.visible.push(make_row(cols));
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
                self.visible.push(make_row(cols));
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
            let mut s = match lead.extras.take() {
                Some(boxed) => String::from(boxed),
                None => String::new(),
            };
            s.push(ch);
            lead.extras = Some(s.into_boxed_str());
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
        self.visible[r][c] = Cell { ch, fg, bg, flags: cell_flags, hyperlink, extras: None };
        if width == 2 && c + 1 < self.cols as usize {
            self.visible[r][c + 1] = Cell {
                ch: ' ',
                fg,
                bg,
                flags: flags | CellFlags::WIDE_CONT,
                hyperlink,
                extras: None,
            };
        }
        self.cursor.col += width;
        self.mark_row(r as u16);
        self.bump();
    }

    pub fn carriage_return(&mut self) {
        self.cursor.col = 0;
        let r = self.cursor.row;
        self.mark_row(r);
        self.bump();
    }

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

    pub fn backspace(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
        let r = self.cursor.row;
        self.mark_row(r);
        self.bump();
    }

    pub fn tab(&mut self) {
        let next = ((self.cursor.col / 8) + 1) * 8;
        self.cursor.col = next.min(self.cols.saturating_sub(1));
        let r = self.cursor.row;
        self.mark_row(r);
        self.bump();
    }

    /// Scroll the visible region up by `n` lines, pushing the topmost rows
    /// into scrollback.
    pub fn scroll_up(&mut self, n: u16) {
        for _ in 0..n {
            let row = self.visible.remove(0);
            if self.scrollback.len() == self.scrollback_limit {
                self.scrollback.pop_front();
            }
            self.scrollback.push_back(row);
            self.visible.push(make_row(self.cols));
        }
        // Every row's content shifted up — the entire visible region
        // changed identity, so every cached span set is stale.
        self.mark_all();
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

fn make_row(cols: u16) -> Row {
    vec![Cell::default(); cols as usize]
}
