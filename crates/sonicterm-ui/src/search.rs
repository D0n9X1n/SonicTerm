//! In-page search (Cmd+F). Pure-data engine: a [`SearchState`] holds the
//! current query plus the precomputed list of [`MatchRange`]s, and exposes
//! cursor-style `next` / `prev` navigation. The renderer reads from this to
//! draw highlight quads and a status line; the app dispatches keystrokes
//! into [`SearchState::input_char`] / [`SearchState::backspace`] while
//! search is active instead of forwarding them to the pty.
//!
//! Coordinate system: [`MatchRange::row`] is an **absolute** row index that
//! treats scrollback as rows `0..scrollback_len` and the visible viewport
//! as rows `scrollback_len..scrollback_len+rows`. When there's no
//! scrollback the absolute coordinates collapse onto the visible grid, so
//! callers that don't care about scrollback can ignore the distinction.

use regex::Regex;
use sonicterm_grid::grid::{Cell, CellFlags, Grid, Row};

use crate::text_edit::{apply_edit, normalize_cursor, TextEdit};

/// A single contiguous match on one row, in **absolute** row + visible
/// column coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchRange {
    /// Absolute row: scrollback rows are `0..scrollback_len`, visible
    /// rows are `scrollback_len..scrollback_len+rows`.
    pub row: u32,
    pub col_start: u16,
    /// Exclusive end column (one past the last char of the match).
    pub col_end: u16,
}

/// Search mode — substring (literal) or regex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[default]
    Substring,
    Regex,
}

/// Live search state for a single tab.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    cursor: usize,
    pub matches: Vec<MatchRange>,
    /// Index into `matches` of the "current" focused match, or `None`.
    pub current: Option<usize>,
    pub case_sensitive: bool,
    pub mode: SearchMode,
    /// Number of scrollback rows the grid had when matches were computed.
    /// Used to translate absolute `MatchRange::row` back into a visible
    /// row when rendering, and to detect when a match lives in scrollback.
    pub scrollback_len: u32,
    /// Visible rows captured at refresh time.
    pub visible_rows: u16,
    /// Last grid revision matches were computed against. Lets callers
    /// (the app loop) skip recomputation when the grid hasn't changed.
    pub last_revision: u64,
    /// When [`Self::current`] points to a match in scrollback (or off
    /// screen), this records the absolute row the viewport should center
    /// on. The app/renderer reads this to drive viewport scrolling.
    /// `None` means no scroll request is pending.
    pub requested_scroll_row: Option<u32>,
    /// Last regex compile error, if any (so the UI can show it).
    pub regex_error: Option<String>,
    /// Forces the next revision check to rescan, whatever the revision says.
    ///
    /// Revisions are per-grid counters, so equality means "this grid has not
    /// changed" only while the grid stays the same one. When the search is
    /// pointed at a different grid — the surviving pane after the searched
    /// pane closed — the two counters are unrelated, and an accidental match
    /// skips the rescan and leaves the dead pane's matches on screen.
    needs_rescan: bool,
}

impl SearchState {
    /// Create an idle search state: empty query, no matches, substring mode,
    /// case-insensitive, and no pending scroll request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Point this search at a different grid than the one it last scanned.
    ///
    /// Called when the pane a search was running against goes away and focus
    /// lands on a survivor. The matches, their coordinates, and the recorded
    /// revision all describe a grid that no longer exists, and the revision
    /// check cannot detect that on its own: two unrelated grids can sit at the
    /// same counter, and when they do the rescan is skipped and the dead
    /// pane's highlights are drawn over the survivor's text.
    pub fn invalidate_for_new_grid(&mut self) {
        self.needs_rescan = true;
    }

    /// Index window `[start, end)` into [`Self::matches`] whose rows intersect
    /// the viewport `[view_top_abs, view_top_abs + rows)`.
    ///
    /// `matches` is built scrollback-rows-then-visible-rows, both ascending,
    /// so it is sorted ascending by `row`; this is a binary-search bound. The
    /// renderer iterates only this window instead of all matches every frame,
    /// keeping per-frame highlight cost O(visible matches) rather than
    /// O(total matches) — the latter scales with scrollback depth for a
    /// common query and stutters on deep history. Returned
    /// indices stay valid against the full `matches` slice, so the caller can
    /// still compare each against `self.current`.
    #[must_use]
    pub fn visible_match_range(&self, view_top_abs: u64, rows: u16) -> (usize, usize) {
        let top = view_top_abs;
        let bottom = view_top_abs.saturating_add(u64::from(rows));
        let start = self.matches.partition_point(|m| u64::from(m.row) < top);
        let end = self.matches.partition_point(|m| u64::from(m.row) < bottom);
        (start, end)
    }

    /// Caret position within [`Self::query`] as a UTF-8 byte offset, clamped
    /// into range and moved back onto a character boundary before it is used.
    #[must_use]
    pub fn cursor(&self) -> usize {
        normalize_cursor(&self.query, self.cursor)
    }

    /// Replace the whole query, park the caret at its end, and rescan `grid`.
    ///
    /// Focus resets: the rescan goes through [`Self::refresh`], so no match is
    /// current afterwards and no scroll is requested.
    pub fn set_query(&mut self, query: impl Into<String>, grid: &Grid) {
        self.query = query.into();
        self.cursor = self.query.len();
        self.refresh(grid);
    }

    /// Insert one typed character at the caret and rescan `grid`.
    ///
    /// The search field is single-line, so newline input is dropped rather
    /// than stored; the caret and query are left untouched in that case.
    pub fn input_char(&mut self, ch: char, grid: &Grid) {
        if matches!(ch, '\r' | '\n') {
            // When: ch matches a line break, which a single-line search field
            // cannot hold; drop the keystroke instead of inserting it.
            return;
        }
        let cursor = self.cursor();
        self.query.insert(cursor, ch);
        self.cursor = cursor + ch.len_utf8();
        self.refresh(grid);
    }

    /// Insert a committed string at the caret and rescan `grid`; the app feeds
    /// this from IME commit text.
    ///
    /// Line breaks are stripped first because the field is single-line; text
    /// that was only line breaks leaves the query and caret unchanged.
    pub fn input_str(&mut self, text: &str, grid: &Grid) {
        let committed: String = text.chars().filter(|ch| !matches!(ch, '\r' | '\n')).collect();
        if committed.is_empty() {
            // When: committed held nothing but line breaks, so there is no
            // text left to insert after filtering.
            return;
        }
        let cursor = self.cursor();
        self.query.insert_str(cursor, &committed);
        self.cursor = cursor + committed.len();
        self.refresh(grid);
    }

    /// Apply one caret movement or deletion to the query, moving the caret and
    /// rescanning `grid` only when the edit actually changed the text.
    ///
    /// Pure caret moves keep the existing matches, so navigation through the
    /// query does not disturb the highlight set.
    pub fn apply_text_edit(&mut self, edit: TextEdit, grid: &Grid) {
        let outcome = apply_edit(&mut self.query, self.cursor, edit);
        self.cursor = outcome.cursor;
        if outcome.changed {
            self.refresh(grid);
        }
    }

    /// Delete the character before the caret, rescanning `grid` only when that
    /// removed something.
    pub fn backspace(&mut self, grid: &Grid) {
        self.apply_text_edit(TextEdit::DeleteBackward, grid);
    }

    /// Toggle case sensitivity (Cmd+I) and recompute.
    pub fn toggle_case_sensitive(&mut self, grid: &Grid) {
        self.case_sensitive = !self.case_sensitive;
        self.refresh(grid);
    }

    /// Toggle between substring and regex matching (Cmd+R) and recompute.
    pub fn toggle_regex(&mut self, grid: &Grid) {
        self.mode = match self.mode {
            SearchMode::Substring => SearchMode::Regex,
            SearchMode::Regex => SearchMode::Substring,
        };
        self.refresh(grid);
    }

    /// Re-scan matches only if `grid.revision()` differs from the last
    /// scan. Preserves the user's "current" match across rescans: tries to
    /// re-find the same (row, col_start) entry; if it's gone, snaps to the
    /// nearest preceding match (or the first one when nothing precedes).
    /// Returns `true` if a rescan happened.
    pub fn maybe_refresh_for_revision(&mut self, grid: &Grid) -> bool {
        if !self.needs_rescan && grid.revision() == self.last_revision {
            // When: no rescan was forced and grid still reports the revision
            // already scanned, so the existing matches still describe it.
            return false;
        }
        self.needs_rescan = false;
        let anchor = self.current_match();
        self.scrollback_len = grid.scrollback_len() as u32;
        self.visible_rows = grid.rows;
        self.last_revision = grid.revision();
        self.regex_error = None;
        self.matches = match self.mode {
            SearchMode::Substring => find_in_grid(grid, &self.query, self.case_sensitive),
            SearchMode::Regex => match find_regex_in_grid(grid, &self.query, self.case_sensitive) {
                Ok(v) => v,
                Err(e) => {
                    self.regex_error = Some(e);
                    Vec::new()
                }
            },
        };
        self.current = if self.matches.is_empty() {
            None
        } else if let Some(a) = anchor {
            // When: anchor recorded the focused match from before the rescan;
            // keep the user on that same entry where it survived.
            if let Some(i) =
                self.matches.iter().position(|m| m.row == a.row && m.col_start == a.col_start)
            {
                Some(i)
            } else {
                // When: the anchored row and col_start no longer appear in
                // matches, so fall back to the nearest preceding entry.
                let preceding = self
                    .matches
                    .iter()
                    .enumerate()
                    .rfind(|(_, m)| (m.row, m.col_start) <= (a.row, a.col_start))
                    .map(|(i, _)| i);
                Some(preceding.unwrap_or(0))
            }
        } else {
            // When: matches exist but anchor was empty, so nothing was focused
            // before the rescan and nothing becomes focused now.
            None
        };
        self.update_scroll_request();
        true
    }

    /// Recompute every match against `grid` unconditionally and drop the focus.
    ///
    /// Records the scrollback depth, visible height, and revision the scan ran
    /// against, and clears any earlier regex error. Unlike
    /// [`Self::maybe_refresh_for_revision`] it does not preserve the focused
    /// match: `current` and the pending scroll request are both reset.
    pub fn refresh(&mut self, grid: &Grid) {
        self.scrollback_len = grid.scrollback_len() as u32;
        self.visible_rows = grid.rows;
        self.last_revision = grid.revision();
        self.regex_error = None;
        self.matches = match self.mode {
            SearchMode::Substring => find_in_grid(grid, &self.query, self.case_sensitive),
            SearchMode::Regex => match find_regex_in_grid(grid, &self.query, self.case_sensitive) {
                Ok(v) => v,
                Err(e) => {
                    self.regex_error = Some(e);
                    Vec::new()
                }
            },
        };
        self.current = None;
        self.requested_scroll_row = None;
    }

    /// Focus the next match, wrapping past the last one back to the first, and
    /// request a scroll to the row it lands on.
    ///
    /// Starts at the first match when nothing is focused yet.
    pub fn next(&mut self) {
        if self.matches.is_empty() {
            // When: matches holds nothing to step onto, so clear the focus and
            // withdraw any pending scroll request.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = Some(match self.current {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        });
        self.update_scroll_request();
    }

    /// Focus the previous match, wrapping past the first one back to the last,
    /// and request a scroll to the row it lands on.
    ///
    /// Starts at the last match when nothing is focused yet.
    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            // When: matches offers no earlier entry to step back onto; drop the
            // focus and the pending scroll request together.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
        self.update_scroll_request();
    }

    /// Focus the match closest to cell (`row`, `col`) and request a scroll to
    /// it.
    ///
    /// Row distance dominates; column distance breaks ties only among matches
    /// already on `row`, measured to the nearest column inside the match.
    pub fn select_nearest(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            // When: matches has no entry to compare against the given cell, so
            // leave the focus and scroll request cleared.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = self
            .matches
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| {
                let row_dist = m.row.abs_diff(row);
                let col_dist = if row_dist == 0 {
                    nearest_col_in_match(m, col).abs_diff(col)
                } else {
                    // When: row_dist is nonzero, so the row gap alone ranks this
                    // match and the column distance is left at 0 unmeasured.
                    0
                };
                (row_dist, col_dist)
            })
            .map(|(i, _)| i);
        self.update_scroll_request();
    }

    /// Focus the first match that starts strictly after cell (`row`, `col`),
    /// wrapping to the first match when none does, and request a scroll to it.
    pub fn next_from(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            // When: matches contains no entry after the given cell or anywhere
            // else, so clear the focus and the scroll request.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current =
            self.matches.iter().position(|m| (m.row, m.col_start) > (row, col)).or(Some(0));
        self.update_scroll_request();
    }

    /// Focus the last match that starts strictly before cell (`row`, `col`),
    /// wrapping to the final match when none does, and request a scroll to it.
    pub fn prev_from(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            // When: matches contains no entry before the given cell, and none
            // to wrap onto either; clear the focus and scroll request.
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current = self
            .matches
            .iter()
            .rposition(|m| (m.row, m.col_start) < (row, col))
            .or_else(|| self.matches.len().checked_sub(1));
        self.update_scroll_request();
    }

    /// The currently focused match, or `None` when nothing is focused or the
    /// stored index no longer addresses an entry in `matches`.
    pub fn current_match(&self) -> Option<MatchRange> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// "N of M" indicator label. `0 of 0` when there are no matches.
    pub fn count_label(&self) -> String {
        let total = self.matches.len();
        let cur = self.current.map(|i| i + 1).unwrap_or(0);
        format!("{cur} of {total}")
    }

    /// True if the given match lives in scrollback (above the viewport).
    pub fn is_in_scrollback(&self, m: &MatchRange) -> bool {
        m.row < self.scrollback_len
    }

    /// Translate an absolute match row into a visible-row index, or `None`
    /// when the match is in scrollback (off the viewport).
    pub fn match_visible_row(&self, m: &MatchRange) -> Option<u16> {
        let visible_start = self.scrollback_len;
        if m.row < visible_start {
            // When: m sits above visible_start in scrollback history, so it has
            // no on-screen row to report.
            return None;
        }
        let r = m.row - visible_start;
        if r < self.visible_rows as u32 {
            Some(r as u16)
        } else {
            // When: r lands past visible_rows, below the viewport captured at
            // the last refresh, so there is no visible index for it.
            None
        }
    }

    fn update_scroll_request(&mut self) {
        self.requested_scroll_row = self.current_match().map(|m| m.row);
    }
}

fn nearest_col_in_match(m: &MatchRange, col: u16) -> u16 {
    col.clamp(m.col_start, m.col_end.saturating_sub(1))
}

/// Search both scrollback and visible rows of `grid` for literal `query`.
/// Returns matches with absolute row coordinates (see module docs).
pub fn find_in_grid(grid: &Grid, query: &str, case_sensitive: bool) -> Vec<MatchRange> {
    if query.is_empty() {
        // When: query gives nothing to look for, so report no ranges rather
        // than scanning the grid.
        return Vec::new();
    }
    let needle: Vec<char> = query_chars(query, case_sensitive);
    if needle.is_empty() {
        // When: needle came back with no chars to compare, and the row scanners
        // below require at least one.
        return Vec::new();
    }

    let mut out = Vec::new();
    let scrollback_len = grid.scrollback_len();
    for (r, row) in grid.scrollback_iter().enumerate() {
        scan_row_substring(row, r as u32, &needle, case_sensitive, &mut out);
    }
    for (r, row) in grid.rows_iter().enumerate() {
        let abs = (scrollback_len + r) as u32;
        scan_row_substring(row, abs, &needle, case_sensitive, &mut out);
    }
    out
}

fn query_chars(input: &str, case_sensitive: bool) -> Vec<char> {
    if case_sensitive {
        input.chars().collect()
    } else {
        // When: case_sensitive is off, so fold the needle to lowercase to meet
        // the cells that visible_cells folds the same way.
        input.chars().flat_map(char::to_lowercase).collect()
    }
}

/// Regex variant. Returns `Err(msg)` with the compile error if `pattern`
/// isn't a valid regex (the caller stores this and shows it in the UI).
pub fn find_regex_in_grid(
    grid: &Grid,
    pattern: &str,
    case_sensitive: bool,
) -> Result<Vec<MatchRange>, String> {
    if pattern.is_empty() {
        // When: pattern gives nothing to compile, so report no ranges instead
        // of building a regex that would match everywhere.
        return Ok(Vec::new());
    }
    let prefix = if case_sensitive { "" } else { "(?i)" };
    let re = Regex::new(&format!("{prefix}{pattern}")).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let scrollback_len = grid.scrollback_len();
    for (r, row) in grid.scrollback_iter().enumerate() {
        scan_row_regex(row, r as u32, &re, &mut out);
    }
    for (r, row) in grid.rows_iter().enumerate() {
        let abs = (scrollback_len + r) as u32;
        scan_row_regex(row, abs, &re, &mut out);
    }
    Ok(out)
}

/// Visible chars on a row, with the column they originate from and whether
/// they're the leading half of a wide pair. Skips WIDE_CONT (continuation
/// cells, which carry no glyph of their own).
struct Visible<'a> {
    col: u16,
    is_wide: bool,
    chars: Vec<char>,
    _cell: &'a Cell,
}

fn visible_cells(row: &Row, case_sensitive: bool) -> Vec<Visible<'_>> {
    row.iter()
        .enumerate()
        .filter(|(_, c)| !c.flags.contains(CellFlags::WIDE_CONT))
        .map(|(i, c)| {
            let chars: Vec<char> = if case_sensitive {
                vec![c.ch]
            } else {
                // When: case_sensitive is off, so fold this cell to lowercase
                // to meet a needle query_chars folded the same way.
                c.ch.to_lowercase().collect()
            };
            Visible { col: i as u16, is_wide: c.flags.contains(CellFlags::WIDE), chars, _cell: c }
        })
        .collect()
}

fn scan_row_substring(
    row: &Row,
    abs_row: u32,
    needle: &[char],
    case_sensitive: bool,
    out: &mut Vec<MatchRange>,
) {
    let visible = visible_cells(row, case_sensitive);
    let mut flat: Vec<char> = Vec::with_capacity(visible.len());
    let mut owner: Vec<usize> = Vec::with_capacity(visible.len());
    for (vi, v) in visible.iter().enumerate() {
        for ch in &v.chars {
            flat.push(*ch);
            owner.push(vi);
        }
    }
    if flat.len() < needle.len() {
        // When: this row's flat chars are fewer than needle needs, so no window
        // can fit and out is left untouched.
        return;
    }
    let mut i = 0usize;
    while i + needle.len() <= flat.len() {
        let matched = needle.iter().enumerate().all(|(k, nc)| flat[i + k] == *nc);
        if matched {
            let start_cell = owner[i];
            let end_cell = owner[i + needle.len() - 1];
            let col_start = visible[start_cell].col;
            let last_visible_col = visible[end_cell].col;
            let extra = if visible[end_cell].is_wide { 1 } else { 0 };
            let col_end = last_visible_col + 1 + extra;
            out.push(MatchRange { row: abs_row, col_start, col_end });
            let next_cell = end_cell + 1;
            i = if next_cell < visible.len() {
                owner.iter().position(|o| *o == next_cell).unwrap_or(flat.len())
            } else {
                // When: next_cell is past the last entry of visible, so park i
                // at the end of flat to finish this row.
                flat.len()
            };
        } else {
            // When: matched is false at this offset, so slide the window one
            // char along and compare again.
            i += 1;
        }
    }
}

fn scan_row_regex(row: &Row, abs_row: u32, re: &Regex, out: &mut Vec<MatchRange>) {
    // Regex always runs case-folded via the `(?i)` prefix inserted by the
    // caller, so we build the haystack from raw cell chars without lowercasing.
    let visible = visible_cells(row, true);
    let mut s = String::with_capacity(visible.len());
    // For each byte in `s`, remember which cell it originated from.
    let mut byte_to_cell: Vec<usize> = Vec::with_capacity(visible.len() * 4);
    for (vi, v) in visible.iter().enumerate() {
        for ch in &v.chars {
            let start = s.len();
            s.push(*ch);
            for _ in start..s.len() {
                byte_to_cell.push(vi);
            }
        }
    }
    for m in re.find_iter(&s) {
        if m.start() == m.end() {
            // When: m spans zero bytes, so there is nothing to highlight and no
            // end cell to look up at m.end() - 1.
            continue;
        }
        let start_cell = byte_to_cell[m.start()];
        let end_cell = byte_to_cell[m.end() - 1];
        let col_start = visible[start_cell].col;
        let last_visible_col = visible[end_cell].col;
        let extra = if visible[end_cell].is_wide { 1 } else { 0 };
        let col_end = last_visible_col + 1 + extra;
        out.push(MatchRange { row: abs_row, col_start, col_end });
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod search_tests;
