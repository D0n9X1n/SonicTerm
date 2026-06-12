//! Grid selection model.
//!
//! Coordinates are grid cells, not pixels. The ROW is a scrollback-ABSOLUTE
//! index (0 = oldest scrollback row; `scrollback_len()` = first live row) so
//! a selection tracks the same TEXT as the viewport scrolls. The COLUMN is a
//! plain cell column. The selection is anchored at `start` and extends to
//! `end`; the pair may be in any order. The app layer converts the
//! viewport-relative row returned by `pixel_to_cell` to an absolute row
//! (via `viewport_row_to_abs`) before building/extending a `Selection`, and
//! the renderer maps the absolute row back to a viewport row for drawing.

use sonicterm_grid::grid::{CellFlags, Grid};

/// The granularity a drag extends at, set on press by the click count.
///
/// WezTerm calls this the `SelectionMode`. After a double-click (word) or
/// triple-click (line), dragging extends the selection BY WHOLE WORDS /
/// WHOLE LINES around the original anchor cell, rather than cell-by-cell.
/// A single click is `Cell` and keeps the exact-cell extend behavior.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SelectMode {
    /// Single-click: drag extends to the exact cell under the cursor.
    #[default]
    Cell,
    /// Double-click: drag extends by whole words, keeping the anchor word.
    Word,
    /// Triple-click: drag extends by whole rows, keeping the anchor row.
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: (u64, u16), // (abs_row, col)
    pub end: (u64, u16),
    /// Distinguishes a deliberate region from a bare point anchor.
    ///
    /// `false` = a point/click anchor (single-click): it is "empty" while
    /// `start == end` and clears on mouse release. `true` = a deliberate
    /// word/line/region selection (double/triple-click): it is NEVER treated
    /// as empty, even when it covers a single cell, so a one-character word
    /// or an empty line stays visible, copyable, and survives release.
    pub anchored: bool,
}

impl Selection {
    pub fn new(row: u64, col: u16) -> Self {
        Self { start: (row, col), end: (row, col), anchored: false }
    }

    pub fn extend(&mut self, row: u64, col: u16) {
        self.end = (row, col);
    }

    /// Return the normalized (top-left, bottom-right) pair.
    pub fn normalized(&self) -> ((u64, u16), (u64, u16)) {
        let (mut a, mut b) = (self.start, self.end);
        if (a.0, a.1) > (b.0, b.1) {
            std::mem::swap(&mut a, &mut b);
        }
        (a, b)
    }

    /// True when (abs_row, col) is inside the selection (inclusive).
    pub fn contains(&self, row: u64, col: u16) -> bool {
        let (a, b) = self.normalized();
        let p = (row, col);
        p >= a && p <= b
    }

    /// Empty selection — a bare point anchor (`start == end`) that was not
    /// deliberately anchored. An anchored word/line selection is never empty,
    /// even when it covers a single cell, so callers that treat `is_empty()`
    /// as "no selection" (release-clear, copy, highlight draw) keep a
    /// single-character word or empty-line selection alive.
    pub fn is_empty(&self) -> bool {
        self.start == self.end && !self.anchored
    }

    /// Select the word under `(abs_row, col)` — the double-click behavior.
    /// `abs_row` is a scrollback-ABSOLUTE row; the matching `Row` is read via
    /// [`Grid::row_at_abs`] so word boundaries come from the correct line
    /// whether the viewport is scrolled or not.
    ///
    /// A "word" is the maximal run of word characters (see
    /// [`is_word_char`]) around the clicked column on the same row. Wide
    /// glyphs are treated as a single unit: the trailing `WIDE_CONT` cell
    /// resolves to its lead cell's character, and a click on either half
    /// expands from the lead column. If the clicked cell is itself a
    /// boundary (whitespace / non-word punctuation), the selection is just
    /// that single cell — it does not expand across whitespace.
    pub fn word_at(grid: &Grid, row: u64, col: u16) -> Selection {
        let Some(line) = grid.row_at_abs(row) else {
            return Selection::new(row, col);
        };
        let len = line.len();
        if len == 0 {
            return Selection::new(row, col);
        }
        // Build a per-column char slice for the row. A WIDE_CONT cell (the
        // trailing half of a wide glyph) carries its lead cell's character
        // so the wide glyph reads as one contiguous word unit during the
        // boundary scan.
        let mut chars: Vec<char> = Vec::with_capacity(len);
        let mut last_lead = ' ';
        for i in 0..len {
            let cell = &line[i];
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                chars.push(last_lead);
            } else {
                last_lead = cell.ch;
                chars.push(cell.ch);
            }
        }
        let c = (col as usize).min(len - 1);
        let (left, right) = word_bounds(&chars, c);
        Selection { start: (row, left as u16), end: (row, right as u16), anchored: true }
    }

    /// Select the whole row under `abs_row` — the triple-click behavior.
    /// `abs_row` is a scrollback-ABSOLUTE row. Spans column 0 through the
    /// last column. `as_text` trims trailing whitespace on copy, so
    /// selecting the full width is fine.
    pub fn line_at(grid: &Grid, row: u64) -> Selection {
        let last_col = match grid.row_at_abs(row) {
            Some(line) => line.len().saturating_sub(1) as u16,
            None => grid.cols.saturating_sub(1),
        };
        Selection { start: (row, 0), end: (row, last_col), anchored: true }
    }

    /// Word-mode drag (WezTerm `SelectionMode::Word`): the selection spans
    /// the union of the word at the `anchor` cell and the word at the
    /// `cursor` cell. Concretely, `word_at(anchor)` and `word_at(cursor)`
    /// are each resolved against the grid, then merged so the result's
    /// `start` is the earlier (row, col) corner and `end` the later one.
    ///
    /// Because the anchor word is always one of the two unioned spans, the
    /// selection NEVER shrinks below the originally double-clicked word —
    /// even when the cursor drags back onto the anchor (then the union is
    /// just the anchor word) or onto a word that is fully contained in it.
    /// Single-cell words and cross-row drags fall out of the (row, col)
    /// min/max naturally. Always `anchored = true`.
    pub fn word_drag(grid: &Grid, anchor: (u64, u16), cursor: (u64, u16)) -> Selection {
        let a = Selection::word_at(grid, anchor.0, anchor.1);
        let c = Selection::word_at(grid, cursor.0, cursor.1);
        // Each of a/c is already a single-row span with start <= end, but
        // the two may be on different rows or ordered either way, so merge
        // by (row, col) corner: the min of the two starts and the max of
        // the two ends.
        let start = a.start.min(c.start);
        let end = a.end.max(c.end);
        Selection { start, end, anchored: true }
    }

    /// Line-mode drag (WezTerm `SelectionMode::Line`): the selection spans
    /// whole rows from `anchor_row` to `cursor_row` inclusive, in either
    /// drag direction. `start` is column 0 of the top row; `end` is the
    /// last column of the bottom row (so `as_text` yields full lines).
    /// The anchor row is always inside `min..=max`, so the selection never
    /// shrinks below the originally triple-clicked line. Always
    /// `anchored = true`.
    pub fn line_drag(grid: &Grid, anchor_row: u64, cursor_row: u64) -> Selection {
        let top = anchor_row.min(cursor_row);
        let bottom = anchor_row.max(cursor_row);
        let last_col = match grid.row_at_abs(bottom) {
            Some(line) => line.len().saturating_sub(1) as u16,
            None => grid.cols.saturating_sub(1),
        };
        Selection { start: (top, 0), end: (bottom, last_col), anchored: true }
    }

    /// Serialize the covered cells from `grid`. Rows are scrollback-ABSOLUTE
    /// and read via [`Grid::row_at_abs`]; a row past the bottom of the
    /// available buffer (`None`) ends the walk.
    pub fn as_text(&self, grid: &Grid) -> String {
        let (a, b) = self.normalized();
        let mut out = String::new();
        // Emit the row separator BEFORE each row after the first, so a walk
        // cut short by an unavailable absolute row (`row_at_abs` → None)
        // never leaves a dangling trailing newline.
        let mut first = true;
        for r in a.0..=b.0 {
            let Some(row) = grid.row_at_abs(r) else {
                break;
            };
            if !first {
                out.push('\n');
            }
            first = false;
            let col_start = if r == a.0 { a.1 as usize } else { 0 };
            let col_end = if r == b.0 { (b.1 as usize + 1).min(row.len()) } else { row.len() };
            let mut line = String::new();
            for cell in row.get_range(col_start, col_end) {
                if cell.flags.contains(CellFlags::WIDE_CONT) {
                    continue;
                }
                line.push(cell.ch);
                if let Some(extras) = cell.extras() {
                    line.push_str(extras);
                }
            }
            out.push_str(line.trim_end());
        }
        out
    }
}

/// Connector characters that count as part of a word in addition to
/// alphanumerics. These are common in filesystem paths and identifiers
/// (`foo-bar`, `a.b.c`, `/usr/local`, `http://`, `~/.config`), so a
/// double-click grabs the whole token rather than stopping at the first
/// punctuation. Tweak this set to adjust double-click word semantics.
/// Mirrors WezTerm's default `selection_word_boundary` spirit.
const WORD_CONNECTORS: &[char] = &['_', '-', '.', '/', ':', '~'];

/// True when `ch` should be treated as part of a word for double-click
/// selection: any Unicode alphanumeric, or one of [`WORD_CONNECTORS`].
/// Whitespace and other punctuation are word boundaries.
pub fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || WORD_CONNECTORS.contains(&ch)
}

/// Find the inclusive `[left, right]` column span of the word containing
/// `col` in `chars`. Pure and grid-free so it is trivially unit-testable.
///
/// - If `chars[col]` is a word char, expands left and right over the
///   maximal run of word chars.
/// - If `chars[col]` is a boundary (space / punctuation), returns
///   `(col, col)` — a single cell, never expanding across whitespace.
/// - An empty slice returns `(0, 0)`.
pub fn word_bounds(chars: &[char], col: usize) -> (usize, usize) {
    if chars.is_empty() {
        return (0, 0);
    }
    let col = col.min(chars.len() - 1);
    if !is_word_char(chars[col]) {
        return (col, col);
    }
    let mut left = col;
    while left > 0 && is_word_char(chars[left - 1]) {
        left -= 1;
    }
    let mut right = col;
    while right + 1 < chars.len() && is_word_char(chars[right + 1]) {
        right += 1;
    }
    (left, right)
}

#[cfg(test)]
#[path = "selection/tests.rs"]
mod tests;
