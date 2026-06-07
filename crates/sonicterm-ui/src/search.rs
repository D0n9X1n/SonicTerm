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
    /// First query line shown in the search-bar viewport when the query spans
    /// more lines than the badge can display.
    pub view_row_offset: usize,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn input_char(&mut self, ch: char, grid: &Grid) {
        self.query.push(ch);
        self.scroll_to_bottom();
        self.refresh(grid);
    }

    pub fn backspace(&mut self, grid: &Grid) {
        self.query.pop();
        self.clamp_view_row_offset();
        self.refresh(grid);
    }

    pub fn scroll_view_up(&mut self) {
        self.view_row_offset = self.view_row_offset.saturating_sub(1);
    }

    pub fn scroll_view_down(&mut self) {
        self.view_row_offset = (self.view_row_offset + 1).min(self.max_view_row_offset());
    }

    fn scroll_to_bottom(&mut self) {
        self.view_row_offset = self.max_view_row_offset();
    }

    fn clamp_view_row_offset(&mut self) {
        self.view_row_offset = self.view_row_offset.min(self.max_view_row_offset());
    }

    fn max_view_row_offset(&self) -> usize {
        normalized_newlines(&self.query).split('\n').count().saturating_sub(1)
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
        if grid.revision() == self.last_revision {
            return false;
        }
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
            if let Some(i) =
                self.matches.iter().position(|m| m.row == a.row && m.col_start == a.col_start)
            {
                Some(i)
            } else {
                let preceding = self
                    .matches
                    .iter()
                    .enumerate()
                    .rfind(|(_, m)| (m.row, m.col_start) <= (a.row, a.col_start))
                    .map(|(i, _)| i);
                Some(preceding.unwrap_or(0))
            }
        } else {
            None
        };
        self.update_scroll_request();
        true
    }

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

    pub fn next(&mut self) {
        if self.matches.is_empty() {
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

    pub fn prev(&mut self) {
        if self.matches.is_empty() {
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

    pub fn select_nearest(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
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
                let col_dist =
                    if row_dist == 0 { nearest_col_in_match(*m, col).abs_diff(col) } else { 0 };
                (row_dist, col_dist)
            })
            .map(|(i, _)| i);
        self.update_scroll_request();
    }

    pub fn next_from(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
            self.current = None;
            self.requested_scroll_row = None;
            return;
        }
        self.current =
            self.matches.iter().position(|m| (m.row, m.col_start) > (row, col)).or(Some(0));
        self.update_scroll_request();
    }

    pub fn prev_from(&mut self, row: u32, col: u16) {
        if self.matches.is_empty() {
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
            return None;
        }
        let r = m.row - visible_start;
        if r < self.visible_rows as u32 {
            Some(r as u16)
        } else {
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
        return Vec::new();
    }
    let normalized = normalized_newlines(query);
    if normalized.contains('\n') {
        return find_multiline_in_grid(grid, &normalized, case_sensitive);
    }
    let needle: Vec<char> = query_chars(&normalized, case_sensitive);
    if needle.is_empty() {
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

fn normalized_newlines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            _ => out.push(ch),
        }
    }
    out
}

fn query_chars(input: &str, case_sensitive: bool) -> Vec<char> {
    if case_sensitive {
        input.chars().collect()
    } else {
        input.chars().flat_map(char::to_lowercase).collect()
    }
}

struct SearchRow<'a> {
    abs_row: u32,
    visible: Vec<Visible<'a>>,
    flat: Vec<char>,
    owner: Vec<usize>,
}

fn search_row(row: &Row, abs_row: u32, case_sensitive: bool) -> SearchRow<'_> {
    let visible = visible_cells(row, case_sensitive);
    let mut flat: Vec<char> = Vec::with_capacity(visible.len());
    let mut owner: Vec<usize> = Vec::with_capacity(visible.len());
    for (vi, v) in visible.iter().enumerate() {
        for ch in &v.chars {
            flat.push(*ch);
            owner.push(vi);
        }
    }
    while flat.last() == Some(&' ') {
        flat.pop();
        owner.pop();
    }
    SearchRow { abs_row, visible, flat, owner }
}

fn find_multiline_in_grid(
    grid: &Grid,
    normalized_query: &str,
    case_sensitive: bool,
) -> Vec<MatchRange> {
    let needle_lines: Vec<Vec<char>> =
        normalized_query.split('\n').map(|line| query_chars(line, case_sensitive)).collect();
    if needle_lines.is_empty() {
        return Vec::new();
    }

    let scrollback_len = grid.scrollback_len();
    let mut rows = Vec::new();
    rows.extend(
        grid.scrollback_iter()
            .enumerate()
            .map(|(r, row)| search_row(row, r as u32, case_sensitive)),
    );
    rows.extend(grid.rows_iter().enumerate().map(|(r, row)| {
        let abs = (scrollback_len + r) as u32;
        search_row(row, abs, case_sensitive)
    }));

    let mut out = Vec::new();
    for start_row in 0..rows.len() {
        let first = &needle_lines[0];
        let start_cols: Box<dyn Iterator<Item = usize>> = if first.is_empty() {
            Box::new(std::iter::once(rows[start_row].flat.len()))
        } else {
            Box::new(0..=rows[start_row].flat.len().saturating_sub(first.len()))
        };
        for start_col in start_cols {
            if !multi_line_match_at(&rows, &needle_lines, start_row, start_col, &mut out) {
                continue;
            }
        }
    }
    out
}

fn multi_line_match_at(
    rows: &[SearchRow<'_>],
    needle_lines: &[Vec<char>],
    start_row: usize,
    start_col: usize,
    out: &mut Vec<MatchRange>,
) -> bool {
    let mut ranges = Vec::new();
    for (line_idx, needle) in needle_lines.iter().enumerate() {
        let Some(row) = rows.get(start_row + line_idx) else {
            return false;
        };
        let offset = if line_idx == 0 { start_col } else { 0 };
        if offset + needle.len() > row.flat.len() {
            return false;
        }
        if row.flat[offset..offset + needle.len()] != needle[..] {
            return false;
        }
        let is_last = line_idx + 1 == needle_lines.len();
        let end = offset + needle.len();
        if !is_last && end != row.flat.len() {
            return false;
        }
        if let Some(range) = flat_range_to_match(row, offset, end) {
            ranges.push(range);
        }
    }
    out.extend(ranges);
    true
}

fn flat_range_to_match(row: &SearchRow<'_>, start: usize, end: usize) -> Option<MatchRange> {
    if end <= start {
        return None;
    }
    let start_cell = row.owner.get(start).copied()?;
    let end_cell = row.owner.get(end - 1).copied()?;
    let col_start = row.visible[start_cell].col;
    let last = &row.visible[end_cell];
    let extra = if last.is_wide { 1 } else { 0 };
    Some(MatchRange { row: row.abs_row, col_start, col_end: last.col + 1 + extra })
}

/// Regex variant. Returns `Err(msg)` with the compile error if `pattern`
/// isn't a valid regex (the caller stores this and shows it in the UI).
pub fn find_regex_in_grid(
    grid: &Grid,
    pattern: &str,
    case_sensitive: bool,
) -> Result<Vec<MatchRange>, String> {
    if pattern.is_empty() {
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
            let chars: Vec<char> =
                if case_sensitive { vec![c.ch] } else { c.ch.to_lowercase().collect() };
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
                flat.len()
            };
        } else {
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
mod tests {
    use super::*;
    use sonicterm_grid::grid::Color;

    fn state_with_matches() -> SearchState {
        SearchState {
            matches: vec![
                MatchRange { row: 10, col_start: 2, col_end: 5 },
                MatchRange { row: 20, col_start: 8, col_end: 9 },
                MatchRange { row: 30, col_start: 1, col_end: 3 },
            ],
            ..SearchState::new()
        }
    }

    #[test]
    fn first_enter_selects_nearest_match_to_cursor() {
        let mut s = state_with_matches();
        s.select_nearest(19, 0);
        assert_eq!(s.current, Some(1));
        assert_eq!(s.requested_scroll_row, Some(20));
    }

    #[test]
    fn arrow_direction_selects_relative_to_cursor_when_unselected() {
        let mut down = state_with_matches();
        down.next_from(20, 8);
        assert_eq!(down.current, Some(2));

        let mut up = state_with_matches();
        up.prev_from(20, 8);
        assert_eq!(up.current, Some(0));
    }

    #[test]
    fn search_query_view_scrolls_between_lines() {
        let mut s = SearchState::new();
        s.query = "one\ntwo\nthree".into();
        s.scroll_to_bottom();
        assert_eq!(s.view_row_offset, 2);
        s.scroll_view_up();
        assert_eq!(s.view_row_offset, 1);
        s.scroll_view_down();
        assert_eq!(s.view_row_offset, 2);
    }

    fn grid_with_lines(lines: &[&str]) -> Grid {
        let rows = lines.len().max(1) as u16;
        let mut grid = Grid::new(16, rows);
        for (idx, line) in lines.iter().enumerate() {
            if idx > 0 {
                grid.linefeed();
                grid.carriage_return();
            }
            for ch in line.chars() {
                grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
            }
        }
        grid
    }

    #[test]
    fn multiline_search_matches_across_rows_with_lf() {
        let grid = grid_with_lines(&["foo", "bar"]);
        assert_eq!(
            find_in_grid(&grid, "foo\nbar", true),
            vec![
                MatchRange { row: 0, col_start: 0, col_end: 3 },
                MatchRange { row: 1, col_start: 0, col_end: 3 },
            ]
        );
    }

    #[test]
    fn multiline_search_matches_windows_crlf_query() {
        let grid = grid_with_lines(&["foo", "bar"]);
        assert_eq!(
            find_in_grid(&grid, "foo\r\nbar", true),
            vec![
                MatchRange { row: 0, col_start: 0, col_end: 3 },
                MatchRange { row: 1, col_start: 0, col_end: 3 },
            ]
        );
    }

    #[test]
    fn multiline_search_matches_classic_mac_cr_query() {
        let grid = grid_with_lines(&["foo", "bar"]);
        assert_eq!(
            find_in_grid(&grid, "foo\rbar", true),
            vec![
                MatchRange { row: 0, col_start: 0, col_end: 3 },
                MatchRange { row: 1, col_start: 0, col_end: 3 },
            ]
        );
    }
}
