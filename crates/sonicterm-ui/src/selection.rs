//! Grid selection model.
//!
//! Coordinates are grid cells, not pixels. (0,0) is top-left of the visible
//! region. The selection is anchored at `start` and extends to `end`; the
//! pair may be in any order.

use sonicterm_grid::grid::{CellFlags, Grid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: (u16, u16), // (row, col)
    pub end: (u16, u16),
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
    pub fn new(row: u16, col: u16) -> Self {
        Self { start: (row, col), end: (row, col), anchored: false }
    }

    pub fn extend(&mut self, row: u16, col: u16) {
        self.end = (row, col);
    }

    /// Return the normalized (top-left, bottom-right) pair.
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        let (mut a, mut b) = (self.start, self.end);
        if (a.0, a.1) > (b.0, b.1) {
            std::mem::swap(&mut a, &mut b);
        }
        (a, b)
    }

    /// True when (row, col) is inside the selection (inclusive).
    pub fn contains(&self, row: u16, col: u16) -> bool {
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

    /// Select the word under `(row, col)` — the double-click behavior.
    ///
    /// A "word" is the maximal run of word characters (see
    /// [`is_word_char`]) around the clicked column on the same row. Wide
    /// glyphs are treated as a single unit: the trailing `WIDE_CONT` cell
    /// resolves to its lead cell's character, and a click on either half
    /// expands from the lead column. If the clicked cell is itself a
    /// boundary (whitespace / non-word punctuation), the selection is just
    /// that single cell — it does not expand across whitespace.
    pub fn word_at(grid: &Grid, row: u16, col: u16) -> Selection {
        if row >= grid.rows {
            return Selection::new(row, col);
        }
        let line = grid.row(row);
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

    /// Select the whole visible row under `row` — the triple-click
    /// behavior. Spans column 0 through the last column. `as_text` trims
    /// trailing whitespace on copy, so selecting the full width is fine.
    pub fn line_at(grid: &Grid, row: u16) -> Selection {
        let last_col = if row < grid.rows {
            grid.row(row).len().saturating_sub(1) as u16
        } else {
            grid.cols.saturating_sub(1)
        };
        Selection { start: (row, 0), end: (row, last_col), anchored: true }
    }

    /// Serialize the covered cells from `grid`.
    pub fn as_text(&self, grid: &Grid) -> String {
        let (a, b) = self.normalized();
        let mut out = String::new();
        for r in a.0..=b.0 {
            if r >= grid.rows {
                break;
            }
            let row = grid.row(r);
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
            let trimmed = line.trim_end();
            out.push_str(trimmed);
            if r < b.0 {
                out.push('\n');
            }
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
mod tests {
    use super::*;
    use sonicterm_grid::grid::{Color, Grid};

    /// Build a 1-row grid wide enough to hold `text`, writing one cell per
    /// char from column 0. Mirrors the put_char usage in grid.rs tests.
    fn grid_with(text: &str) -> Grid {
        let cols = text.chars().count().max(1) as u16;
        let mut grid = Grid::new(cols, 1);
        grid.goto(0, 0);
        for ch in text.chars() {
            grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
        grid
    }

    // ---- word_bounds (pure helper) ----

    #[test]
    fn word_bounds_inside_connector_word() {
        // "foo bar.baz qux"
        //  0123456789012345
        let chars: Vec<char> = "foo bar.baz qux".chars().collect();
        // col 8 is the 'b' of "baz" inside "bar.baz"; the '.' connector
        // keeps "bar.baz" a single word spanning cols 4..=10.
        assert_eq!(word_bounds(&chars, 8), (4, 10));
        // The connector '.' itself (col 7) is a word char → same span.
        assert_eq!(word_bounds(&chars, 7), (4, 10));
    }

    #[test]
    fn word_bounds_on_space_is_single_cell() {
        let chars: Vec<char> = "foo bar.baz qux".chars().collect();
        // col 3 is the space between "foo" and "bar.baz".
        assert_eq!(word_bounds(&chars, 3), (3, 3));
        // col 11 is the space before "qux".
        assert_eq!(word_bounds(&chars, 11), (11, 11));
    }

    #[test]
    fn word_bounds_stops_at_word_edges() {
        let chars: Vec<char> = "foo bar.baz qux".chars().collect();
        // Start of "foo".
        assert_eq!(word_bounds(&chars, 0), (0, 2));
        // End of "foo".
        assert_eq!(word_bounds(&chars, 2), (0, 2));
        // Start of "qux".
        assert_eq!(word_bounds(&chars, 12), (12, 14));
        // End of "qux".
        assert_eq!(word_bounds(&chars, 14), (12, 14));
    }

    #[test]
    fn word_bounds_empty_slice() {
        assert_eq!(word_bounds(&[], 0), (0, 0));
        assert_eq!(word_bounds(&[], 5), (0, 0));
    }

    // ---- word_at / line_at (Grid constructors) ----

    #[test]
    fn word_at_selects_connector_word() {
        let grid = grid_with("foo bar.baz qux");
        // Click inside "baz" (col 8) → whole "bar.baz" (cols 4..=10).
        let sel = Selection::word_at(&grid, 0, 8);
        assert_eq!(sel.start, (0, 4));
        assert_eq!(sel.end, (0, 10));
        assert_eq!(sel.as_text(&grid), "bar.baz");
    }

    #[test]
    fn word_at_on_space_selects_single_cell() {
        let grid = grid_with("foo bar.baz qux");
        let sel = Selection::word_at(&grid, 0, 3);
        assert_eq!(sel.start, (0, 3));
        assert_eq!(sel.end, (0, 3));
    }

    #[test]
    fn word_at_clamps_and_stops_at_boundaries() {
        let grid = grid_with("foo bar.baz qux");
        // Start of "foo".
        let sel = Selection::word_at(&grid, 0, 0);
        assert_eq!((sel.start, sel.end), ((0, 0), (0, 2)));
        // End of last word, with an out-of-range col that clamps to last.
        let sel = Selection::word_at(&grid, 0, 999);
        assert_eq!((sel.start, sel.end), ((0, 12), (0, 14)));
    }

    #[test]
    fn line_at_spans_full_row() {
        let grid = grid_with("foo bar.baz qux");
        let sel = Selection::line_at(&grid, 0);
        assert_eq!(sel.start, (0, 0));
        assert_eq!(sel.end, (0, 14)); // last col = len - 1
        assert_eq!(sel.as_text(&grid), "foo bar.baz qux");
    }

    // ---- anchored vs point-anchor emptiness (single-cell edge case) ----

    #[test]
    fn point_select_new_is_empty() {
        // A bare single-click point anchor (start == end, not anchored) is
        // empty, so release-clear/copy still treat it as "no selection".
        let sel = Selection::new(3, 7);
        assert_eq!(sel.start, sel.end);
        assert!(!sel.anchored);
        assert!(sel.is_empty());
    }

    #[test]
    fn word_at_single_char_word_is_not_empty() {
        // Double-clicking a one-character word ("x") yields start == end but
        // is a deliberate, anchored selection — it must NOT read as empty,
        // or it would be invisible / uncopyable / cleared on release.
        let grid = grid_with("x");
        let sel = Selection::word_at(&grid, 0, 0);
        assert_eq!(sel.start, (0, 0));
        assert_eq!(sel.end, (0, 0));
        assert!(sel.anchored);
        assert!(!sel.is_empty());
        assert_eq!(sel.as_text(&grid), "x");
    }

    #[test]
    fn line_at_empty_row_is_not_empty() {
        // An empty/blank line collapses to a single cell (last_col == 0), so
        // start == end. Triple-click is anchored, so it stays non-empty.
        let grid = grid_with("");
        let sel = Selection::line_at(&grid, 0);
        assert_eq!(sel.start, sel.end);
        assert!(sel.anchored);
        assert!(!sel.is_empty());
    }
}
