//! Vim-style keyboard copy-mode state.
//!
//! Coordinates are `(col, row)` pairs in visible-grid cell space. The live
//! terminal cursor keeps moving independently; copy mode owns this separate
//! cursor plus an optional selection anchor.
//!
//! The application stores two shapes of this state, and neither produces a
//! selected range. Entering copy mode builds a read-only state via
//! [`CopyModeState::read_only_at`]: the cursor roams for reading,
//! [`CopyModeState::start_select`] is inert, and
//! [`CopyModeState::selected_range`] is `None`. Quick select builds a state
//! that is not read-only but carries a [`QuickSelectState`] and never sets an
//! anchor, so it has no range either; what a hint keypress copies is the
//! owned text captured by [`QuickSelectState::from_grid`], which is a
//! snapshot and does not follow later writes to the grid it was scanned from.

use sonicterm_cfg::url_scan::find_urls;
use sonicterm_grid::grid::{Cell, CellFlags, Grid, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    Cursor,
    Select,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickSelectState {
    pub hints: Vec<QuickSelectHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickSelectHint {
    pub hint: char,
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub text: String,
}

impl QuickSelectState {
    /// Scan the live screen rows for URLs and label each with a keyboard hint.
    ///
    /// Hints are assigned in row-major order and run out after 26 matches. Each
    /// hint owns a copy of its URL text, so later writes to `grid` do not
    /// change what a hint keypress copies.
    pub fn from_grid(grid: &Grid) -> Self {
        let mut hints = Vec::new();
        for row_idx in grid.scrollback_len()..grid.scrollback_len() + grid.rows as usize {
            let Some(row) = visible_row(grid, row_idx) else {
                // When: visible_row cannot resolve row_idx to a stored or live
                // line; keep the hints gathered so far and move on.
                continue;
            };
            let line = row_text_of(row);
            for m in find_urls(&line) {
                let Some(hint) = nth_hint(hints.len()) else {
                    // When: nth_hint has spent all 26 single-letter labels;
                    // return with the hints already assigned and stop scanning.
                    return Self { hints };
                };
                // `m.start`/`m.end` are byte offsets into `line`. The hint's
                // `col_*` fields are consumed downstream as grid columns, so
                // map through cell widths rather than a raw `char` count:
                // wide cells span two columns and combining marks live in a
                // lead cell's `extras()`, so a byte count and a grid column
                // diverge whenever either precedes the URL.
                let col_start = byte_to_grid_col(row, m.start);
                let col_end = byte_to_grid_col(row, m.end.saturating_sub(1));
                hints.push(QuickSelectHint { hint, row: row_idx, col_start, col_end, text: m.url });
            }
        }
        Self { hints }
    }

    /// Text captured for a hint key, matched without regard to case.
    pub fn text_for_hint(&self, hint: char) -> Option<&str> {
        self.hints.iter().find(|h| h.hint.eq_ignore_ascii_case(&hint)).map(|h| h.text.as_str())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyModeState {
    pub cursor: (usize, usize),
    pub anchor: Option<(usize, usize)>,
    pub mode: CopyMode,
    pub quick_select: Option<QuickSelectState>,
    pub read_only: bool,
}

impl CopyModeState {
    /// Copy-mode state starting at `pos` that is allowed to select.
    pub fn new_at(pos: (usize, usize)) -> Self {
        Self {
            cursor: pos,
            anchor: None,
            mode: CopyMode::Cursor,
            quick_select: None,
            read_only: false,
        }
    }

    /// Copy-mode state starting at `pos` that browses without selecting.
    ///
    /// The cursor still roams for reading, but `start_select` is inert and
    /// `selected_range` stays `None`.
    pub fn read_only_at(pos: (usize, usize)) -> Self {
        Self {
            cursor: pos,
            anchor: None,
            mode: CopyMode::Cursor,
            quick_select: None,
            read_only: true,
        }
    }

    /// Whether this state only browses and never yields a selected range.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Step the cursor one column left, stopping at the first column.
    pub fn move_left(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.0 = self.cursor.0.saturating_sub(1);
    }

    /// Step the cursor one column right, stopping at the last column.
    pub fn move_right(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.0 = (self.cursor.0 + 1).min(max_col(grid));
    }

    /// Step the cursor one row up, stopping at the topmost row.
    pub fn move_up(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = self.cursor.1.saturating_sub(1);
    }

    /// Step the cursor one row down, stopping at the bottom row.
    pub fn move_down(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = (self.cursor.1 + 1).min(max_row(grid));
    }

    /// Move the cursor to the start of the next word, or to the final cell
    /// when no further word begins.
    pub fn move_word_fwd(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        let mut pos = self.cursor;
        let current_is_word = char_at(grid, pos).is_some_and(is_word_char);

        loop {
            let Some(next) = next_pos(grid, pos) else {
                // When: next_pos runs off the end of the grid; park the cursor
                // on the final cell rather than leaving it mid-scan.
                self.cursor = (max_col(grid), max_row(grid));
                return;
            };
            pos = next;
            let ch = char_at(grid, pos);
            if current_is_word && ch.is_some_and(|c| !is_word_char(c)) {
                // When: current_is_word held at the start and the scan has now
                // left that word; break to hunt for the next word start.
                break;
            }
            if !current_is_word && ch.is_some_and(is_word_char) {
                // When: current_is_word was false, so the first word cell met
                // is already the destination.
                self.cursor = pos;
                return;
            }
        }

        while let Some(next) = next_pos(grid, pos) {
            pos = next;
            if char_at(grid, pos).is_some_and(is_word_char) {
                // When: char_at reaches the first word cell after the gap; that
                // cell begins the next word.
                self.cursor = pos;
                return;
            }
        }
        self.cursor = (max_col(grid), max_row(grid));
    }

    /// Move the cursor back to the first cell of the word at or before it.
    pub fn move_word_back(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        let mut pos = self.cursor;

        while let Some(prev) = prev_pos(grid, pos) {
            pos = prev;
            if char_at(grid, pos).is_some_and(is_word_char) {
                // When: char_at meets the nearest word cell behind the cursor;
                // stop skipping and start walking to that word's first cell.
                break;
            }
        }

        while let Some(prev) = prev_pos(grid, pos) {
            if !char_at(grid, prev).is_some_and(is_word_char) {
                // When: char_at shows prev lies outside the word, so the cell
                // already reached is its first.
                break;
            }
            pos = prev;
        }
        self.cursor = pos;
    }

    /// Move the cursor to the first column of its row.
    pub fn move_line_start(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.0 = 0;
    }

    /// Move the cursor to the last non-blank column of its row, or to the last
    /// column when the row holds no text.
    pub fn move_line_end(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        let row = visible_row(grid, self.cursor.1);
        let last = row.and_then(last_non_blank_col).unwrap_or_else(|| max_col(grid));
        self.cursor.0 = last.min(max_col(grid));
    }

    /// Move the cursor to the topmost row, keeping its column in range.
    pub fn move_top(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = 0;
        self.cursor.0 = self.cursor.0.min(max_col(grid));
    }

    /// Move the cursor to the bottom row, keeping its column in range.
    pub fn move_bottom(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = max_row(grid);
        self.cursor.0 = self.cursor.0.min(max_col(grid));
    }

    /// Begin a selection anchored at the cursor, or keep an existing anchor.
    pub fn start_select(&mut self) {
        if self.read_only {
            // When: read_only marks a browse-only state; leave anchor and mode
            // untouched so a selection never begins.
            return;
        }
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.mode = CopyMode::Select;
    }

    /// Selected span as an ordered `(start, end)` pair, or `None` when this
    /// state has no anchor or refuses to select.
    pub fn selected_range(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.read_only {
            // When: read_only marks a browse-only state; report no span even if
            // an anchor was set before the state became browse-only.
            return None;
        }
        let anchor = self.anchor?;
        let mut start = anchor;
        let mut end = self.cursor;
        if (start.1, start.0) > (end.1, end.0) {
            std::mem::swap(&mut start, &mut end);
        }
        Some((start, end))
    }

    fn clamp_to_grid(&mut self, grid: &Grid) {
        self.cursor.0 = self.cursor.0.min(max_col(grid));
        self.cursor.1 = self.cursor.1.min(max_row(grid));
    }
}

fn max_col(grid: &Grid) -> usize {
    grid.cols.saturating_sub(1) as usize
}

fn max_row(grid: &Grid) -> usize {
    grid.scrollback_len().saturating_add(grid.rows as usize).saturating_sub(1)
}

fn visible_row(grid: &Grid, row: usize) -> Option<&Row> {
    let sb = grid.scrollback_len();
    if row < sb {
        grid.scrollback_row(row)
    } else {
        // When: row sits at or past sb, so it addresses the live screen;
        // rebase it to a screen-relative index before indexing.
        let live = row - sb;
        (live < grid.rows as usize).then(|| grid.row(live as u16))
    }
}

fn char_at(grid: &Grid, pos: (usize, usize)) -> Option<char> {
    let row = visible_row(grid, pos.1)?;
    let cell = row.get(pos.0)?;
    (!cell.flags.contains(CellFlags::WIDE_CONT)).then_some(cell.ch)
}

fn next_pos(grid: &Grid, pos: (usize, usize)) -> Option<(usize, usize)> {
    let col = pos.0 + 1;
    if col < grid.cols as usize {
        Some((col, pos.1))
    } else if pos.1 < max_row(grid) {
        // When: the step left the row width behind but pos is above max_row;
        // wrap onto the first column of the row below.
        Some((0, pos.1 + 1))
    } else {
        // When: pos already sits in the final column of max_row, so no
        // following cell exists.
        None
    }
}

fn prev_pos(grid: &Grid, pos: (usize, usize)) -> Option<(usize, usize)> {
    if pos.0 > 0 {
        Some((pos.0 - 1, pos.1))
    } else if pos.1 > 0 {
        // When: pos sits in column zero while rows remain above; wrap onto the
        // last column of the preceding row.
        Some((max_col(grid), pos.1 - 1))
    } else {
        // When: pos is the grid origin, so no earlier cell exists to step onto.
        None
    }
}

fn is_word_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn last_non_blank_col(row: &Row) -> Option<usize> {
    row.iter().enumerate().rev().find_map(|(idx, cell)| {
        (!cell.flags.contains(CellFlags::WIDE_CONT) && cell.ch != ' ').then_some(idx)
    })
}
fn row_text_of(row: &Row) -> String {
    let mut text = String::with_capacity(row.len());
    for cell in row.iter() {
        if cell.flags.contains(CellFlags::WIDE_CONT) {
            // When: cell is the trailing half of a wide glyph; its lead cell
            // already emitted the text, so emitting again would duplicate it.
            continue;
        }
        text.push(cell.ch);
        if let Some(extras) = cell.extras() {
            text.push_str(extras);
        }
    }
    text
}

fn nth_hint(idx: usize) -> Option<char> {
    (idx < 26).then(|| (b'a' + idx as u8) as char)
}

/// Number of UTF-8 bytes a cell contributes to [`row_text_of`]: its lead
/// `char` plus any combining marks stored in `extras()`.
fn cell_text_len(cell: &Cell) -> usize {
    cell.ch.len_utf8() + cell.extras().map_or(0, str::len)
}

/// Map a byte offset in [`row_text_of`]'s string back to the grid column of
/// the cell that produced that byte. `WIDE_CONT` cells emit no text but still
/// occupy a column, so this walks the row cell-by-cell keeping a byte cursor
/// and a column cursor in lock-step. Offsets past the emitted text clamp to
/// the last lead column, mirroring `saturating` movement elsewhere.
fn byte_to_grid_col(row: &Row, byte: usize) -> usize {
    let mut acc = 0usize;
    let mut last_lead = 0usize;
    for (col, cell) in row.iter().enumerate() {
        if cell.flags.contains(CellFlags::WIDE_CONT) {
            // When: cell continues a wide glyph and contributed no bytes, so
            // advance the column without moving the byte cursor.
            continue;
        }
        last_lead = col;
        let next = acc + cell_text_len(cell);
        if byte < next {
            // When: byte falls inside the text this cell emitted, so col is the
            // column that produced it.
            return col;
        }
        acc = next;
    }
    last_lead
}

#[cfg(test)]
#[path = "copy_mode_tests.rs"]
mod copy_mode_tests;
