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

use sonicterm_grid::grid::{CellFlags, Grid, Row};

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
    /// Pane whose content sequence `content_seq` belongs to.
    ///
    /// SonicTerm stores selection state per window while row sequences are
    /// per pane. Keeping the identity beside the sequence prevents a pane-focus
    /// change from comparing unrelated counters and invalidating arbitrarily.
    pub pane_id: Option<u64>,
    /// Grid cell-content sequence observed when this selection was last created
    /// or extended. Mirrors WezTerm's selection seqno: rows changed after this
    /// value may invalidate the selected range; older dirty state cannot.
    pub content_seq: u64,
    /// Screen buffer the selected content came from. Switching between primary
    /// and alternate screens replaces every row's identity, so a selection from
    /// the previous buffer cannot remain copyable on the next one.
    pub on_alt_screen: bool,
    /// Number of oldest scrollback rows already removed when the endpoints were
    /// recorded. A later eviction rebases surviving absolute rows by the delta.
    pub scrollback_evicted: u64,
}

impl Selection {
    pub fn new(row: u64, col: u16) -> Self {
        Self {
            start: (row, col),
            end: (row, col),
            anchored: false,
            pane_id: None,
            content_seq: 0,
            on_alt_screen: false,
            scrollback_evicted: 0,
        }
    }

    /// Bind this selection to the pane and grid state it was created from.
    #[must_use]
    pub fn with_content_state(
        mut self,
        pane_id: u64,
        content_seq: u64,
        on_alt_screen: bool,
        scrollback_evicted: u64,
    ) -> Self {
        self.pane_id = Some(pane_id);
        self.content_seq = content_seq;
        self.on_alt_screen = on_alt_screen;
        self.scrollback_evicted = scrollback_evicted;
        self
    }

    /// Extend the range and advance its content baseline, matching WezTerm's
    /// `extend_selection_at_mouse_cursor` behavior.
    pub fn extend_with_content_state(
        &mut self,
        row: u64,
        col: u16,
        pane_id: u64,
        content_seq: u64,
        on_alt_screen: bool,
        scrollback_evicted: u64,
    ) {
        self.end = (row, col);
        self.pane_id = Some(pane_id);
        self.content_seq = content_seq;
        self.on_alt_screen = on_alt_screen;
        self.scrollback_evicted = scrollback_evicted;
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
        Selection {
            start: (row, left as u16),
            end: (row, right as u16),
            anchored: true,
            pane_id: None,
            content_seq: grid.content_seq(),
            on_alt_screen: grid.is_alt(),
            scrollback_evicted: grid.scrollback_evicted(),
        }
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
        Selection {
            start: (row, 0),
            end: (row, last_col),
            anchored: true,
            pane_id: None,
            content_seq: grid.content_seq(),
            on_alt_screen: grid.is_alt(),
            scrollback_evicted: grid.scrollback_evicted(),
        }
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
        Selection {
            start,
            end,
            anchored: true,
            pane_id: None,
            content_seq: grid.content_seq(),
            on_alt_screen: grid.is_alt(),
            scrollback_evicted: grid.scrollback_evicted(),
        }
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
        Selection {
            start: (top, 0),
            end: (bottom, last_col),
            anchored: true,
            pane_id: None,
            content_seq: grid.content_seq(),
            on_alt_screen: grid.is_alt(),
            scrollback_evicted: grid.scrollback_evicted(),
        }
    }

    /// Serialize the covered cells from `grid`. Rows are scrollback-ABSOLUTE
    /// and read via [`Grid::row_at_abs`]; a row past the bottom of the
    /// available buffer (`None`) ends the walk.
    pub fn as_text(&self, grid: &Grid) -> String {
        let (a, b) = self.normalized();
        plain_text_from_grid_range(grid, (usize::from(a.1), a.0), (usize::from(b.1), b.0))
    }
}

/// Serialize a cell range as clipboard-safe plain text.
///
/// Terminal UIs commonly draw a detached box frame in the pane's final column.
/// A cross-row selection necessarily spans that column on every intermediate
/// row. Strip only a coherent multi-row right frame: vertical sides on every
/// preceding row followed by a lower-right corner. Isolated or incomplete
/// patterns remain literal text.
pub fn plain_text_from_grid_range(
    grid: &Grid,
    mut start: (usize, u64),
    mut end: (usize, u64),
) -> String {
    if (start.1, start.0) > (end.1, end.0) {
        std::mem::swap(&mut start, &mut end);
    }
    let ((start_col, start_row), (end_col, end_row)) = (start, end);
    let strip_right_frame = has_coherent_right_frame(grid, start_col, start_row, end_row);
    let mut out = String::new();
    let mut first = true;
    for row_idx in start_row..=end_row {
        let Some(row) = grid.row_at_abs(row_idx) else {
            break;
        };
        if !first {
            out.push('\n');
        }
        first = false;
        let col_start = if row_idx == start_row { start_col } else { 0 }.min(row.len());
        let requested_end = if row_idx == end_row { end_col.saturating_add(1) } else { row.len() };
        let requested_end = requested_end.min(row.len());
        let col_end = if strip_right_frame {
            detached_right_frame(row, col_start, requested_end)
                .map_or(requested_end, |(content_end, _)| content_end)
        } else {
            requested_end
        };
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

fn has_coherent_right_frame(grid: &Grid, start_col: usize, start_row: u64, end_row: u64) -> bool {
    if start_row >= end_row {
        return false;
    }
    let mut saw_vertical_side = false;
    for row_idx in start_row..=end_row {
        let Some(row) = grid.row_at_abs(row_idx) else {
            return false;
        };
        let col_start = if row_idx == start_row { start_col } else { 0 }.min(row.len());
        let Some((_, frame)) = detached_right_frame(row, col_start, row.len()) else {
            return false;
        };
        if row_idx == end_row {
            return saw_vertical_side && is_lower_right_frame_corner(frame);
        }
        if !is_vertical_frame_side(frame) {
            return false;
        }
        saw_vertical_side = true;
    }
    false
}

fn detached_right_frame(row: &Row, col_start: usize, col_end: usize) -> Option<(usize, char)> {
    if col_end != row.len() || col_end <= col_start {
        return None;
    }
    let mut last_non_space = col_end;
    while last_non_space > col_start && row[last_non_space - 1].ch.is_whitespace() {
        last_non_space -= 1;
    }
    if last_non_space == col_start {
        return None;
    }
    let frame_col = last_non_space - 1;
    let frame = row[frame_col].ch;
    let at_right_edge = frame_col.saturating_add(2) >= row.len();
    let detached = frame_col > col_start
        && !row[frame_col - 1].flags.contains(CellFlags::WIDE_CONT)
        && row[frame_col - 1].ch.is_whitespace();
    if !at_right_edge || !detached {
        return None;
    }
    let mut content_end = frame_col;
    while content_end > col_start && row[content_end - 1].ch.is_whitespace() {
        content_end -= 1;
    }
    Some((content_end, frame))
}

fn is_vertical_frame_side(ch: char) -> bool {
    matches!(ch, '│' | '┃' | '┆' | '┇' | '┊' | '┋' | '╎' | '╏' | '║')
}

fn is_lower_right_frame_corner(ch: char) -> bool {
    matches!(ch, '┘' | '┙' | '┚' | '┛' | '╛' | '╜' | '╝' | '╯')
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

/// Whether a selection must be dropped because content changed underneath it.
///
/// Mirrors WezTerm's changed-since-selection rule: compare only rows whose
/// cell-content sequence is newer than the selection's baseline, then clear
/// only when those rows intersect the selected absolute-row range. Cursor
/// motion and presentation-only dirtiness do not advance the content sequence.
///
/// Live visible row `r` maps to `scrollback_len + r`, not to the user's current
/// viewport. On the primary screen that lets a selected row move into history
/// without invalidation; on the alternate screen `scrollback_len` is zero, so a
/// TUI repaint of a fixed row invalidates exactly that row.
#[must_use]
pub fn revalidate_selection(selection: &mut Selection, pane_id: u64, grid: &Grid) -> bool {
    if selection.is_empty() {
        return false;
    }
    if selection.pane_id != Some(pane_id) || selection.on_alt_screen != grid.is_alt() {
        return true;
    }

    let evicted = grid.scrollback_evicted().saturating_sub(selection.scrollback_evicted);
    if !selection.on_alt_screen && evicted > 0 {
        let ((first_row, _), _) = selection.normalized();
        if first_row < evicted {
            return true;
        }
        selection.start.0 -= evicted;
        selection.end.0 -= evicted;
        selection.scrollback_evicted = grid.scrollback_evicted();
    }

    let ((first_row, _), (last_row, _)) = selection.normalized();
    let live_top = grid.scrollback_len() as u64;
    grid.scrollback_rows_changed_since(selection.content_seq)
        .chain(
            grid.visible_rows_changed_since(selection.content_seq).map(|row| live_top + row as u64),
        )
        .any(|row| row >= first_row && row <= last_row)
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;
