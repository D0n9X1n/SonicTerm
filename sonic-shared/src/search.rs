//! In-page search (Cmd+F). Pure-data engine: a [`SearchState`] holds the
//! current query plus the precomputed list of [`MatchRange`]s, and exposes
//! cursor-style `next` / `prev` navigation. The renderer reads from this to
//! draw highlight quads and a status line; the app dispatches keystrokes
//! into [`SearchState::input_char`] / [`SearchState::backspace`] while
//! search is active instead of forwarding them to the pty.

use sonic_core::grid::{CellFlags, Grid};

/// A single contiguous match on one row, in **visible** column coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchRange {
    pub row: u16,
    pub col_start: u16,
    /// Exclusive end column (one past the last char of the match).
    pub col_end: u16,
}

/// Live search state for a single tab.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub matches: Vec<MatchRange>,
    /// Index into `matches` of the "current" focused match, or `None`.
    pub current: Option<usize>,
    pub case_sensitive: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn input_char(&mut self, ch: char, grid: &Grid) {
        self.query.push(ch);
        self.refresh(grid);
    }

    pub fn backspace(&mut self, grid: &Grid) {
        self.query.pop();
        self.refresh(grid);
    }

    pub fn refresh(&mut self, grid: &Grid) {
        self.matches = find_in_grid(grid, &self.query, self.case_sensitive);
        self.current = if self.matches.is_empty() { None } else { Some(0) };
    }

    pub fn next(&mut self) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        self.current = Some(match self.current {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        });
    }

    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
    }

    pub fn current_match(&self) -> Option<MatchRange> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }
}

/// Walk each row of `grid` (skipping WIDE_CONT cells), and find every
/// non-overlapping occurrence of `query` as a contiguous run of cells on
/// that row. Empty query → empty result. Matches don't overlap: after a hit
/// at columns `[a, b)` the scan resumes at column `b`.
pub fn find_in_grid(grid: &Grid, query: &str, case_sensitive: bool) -> Vec<MatchRange> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let needle: Vec<char> = if case_sensitive {
        query.chars().collect()
    } else {
        query.chars().flat_map(|c| c.to_lowercase()).collect()
    };
    if needle.is_empty() {
        return out;
    }

    for r in 0..grid.rows {
        let row = grid.row(r);
        // Collect (display_col, normalized_chars) per non-WIDE_CONT cell.
        // For case-insensitive search, expand to lowercase (possibly
        // multi-char per cell so ß -> "ss" / İ -> "i\u{0307}" work).
        // For wide chars, the matched range must extend to the WIDE_CONT
        // cell on the right.
        struct Visible {
            col: u16,
            is_wide: bool,
            chars: Vec<char>,
        }
        let visible: Vec<Visible> = row
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.flags.contains(CellFlags::WIDE_CONT))
            .map(|(i, c)| {
                let chars: Vec<char> =
                    if case_sensitive { vec![c.ch] } else { c.ch.to_lowercase().collect() };
                Visible { col: i as u16, is_wide: c.flags.contains(CellFlags::WIDE), chars }
            })
            .collect();

        // Build a flat char stream that maps each char index back to its
        // owning visible-cell index, so we can recover (start, end) cells
        // after matching.
        let mut flat: Vec<char> = Vec::with_capacity(visible.len());
        let mut owner: Vec<usize> = Vec::with_capacity(visible.len());
        for (vi, v) in visible.iter().enumerate() {
            for ch in &v.chars {
                flat.push(*ch);
                owner.push(vi);
            }
        }

        if flat.len() < needle.len() {
            continue;
        }

        let mut i = 0usize;
        while i + needle.len() <= flat.len() {
            let mut matched = true;
            for (k, nc) in needle.iter().enumerate() {
                if flat[i + k] != *nc {
                    matched = false;
                    break;
                }
            }
            if matched {
                let start_cell = owner[i];
                let end_cell = owner[i + needle.len() - 1];
                let col_start = visible[start_cell].col;
                let last_visible_col = visible[end_cell].col;
                let extra = if visible[end_cell].is_wide { 1 } else { 0 };
                let col_end = last_visible_col + 1 + extra;
                out.push(MatchRange { row: r, col_start, col_end });
                // Advance past the entire matched cell range so we don't
                // double-match the same cells when a fold expanded chars.
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonic_core::grid::{Color, Grid};

    fn put(g: &mut Grid, s: &str) {
        for ch in s.chars() {
            g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
    }

    fn grid_with(text: &str, cols: u16) -> Grid {
        let rows = (text.lines().count() as u16).max(1);
        let mut g = Grid::new(cols, rows);
        let mut first = true;
        for line in text.lines() {
            if !first {
                let cur_row = g.cursor.row + 1;
                g.cursor.row = cur_row;
                g.cursor.col = 0;
            }
            first = false;
            put(&mut g, line);
        }
        g
    }

    #[test]
    fn find_empty_query_returns_empty() {
        let g = grid_with("hello world", 32);
        assert!(find_in_grid(&g, "", false).is_empty());
        assert!(find_in_grid(&g, "", true).is_empty());
    }

    #[test]
    fn find_single_line_one_match() {
        let g = grid_with("the quick brown fox", 32);
        let m = find_in_grid(&g, "quick", false);
        assert_eq!(m, vec![MatchRange { row: 0, col_start: 4, col_end: 9 }]);
    }

    #[test]
    fn find_multiple_matches_per_row() {
        let g = grid_with("ababab", 8);
        let m = find_in_grid(&g, "ab", true);
        assert_eq!(
            m,
            vec![
                MatchRange { row: 0, col_start: 0, col_end: 2 },
                MatchRange { row: 0, col_start: 2, col_end: 4 },
                MatchRange { row: 0, col_start: 4, col_end: 6 },
            ]
        );
    }

    #[test]
    fn find_case_sensitive_toggle() {
        let g = grid_with("Foo foo FOO", 16);
        assert_eq!(find_in_grid(&g, "foo", true).len(), 1);
        assert_eq!(find_in_grid(&g, "foo", false).len(), 3);
    }

    #[test]
    fn find_matches_do_not_overlap() {
        let g = grid_with("aaaa", 8);
        let m = find_in_grid(&g, "aa", true);
        assert_eq!(
            m,
            vec![
                MatchRange { row: 0, col_start: 0, col_end: 2 },
                MatchRange { row: 0, col_start: 2, col_end: 4 },
            ]
        );
    }

    #[test]
    fn find_skips_wide_cont_cells() {
        let mut g = Grid::new(8, 1);
        g.put_char('中', Color::Default, Color::Default, CellFlags::empty());
        g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
        assert!(g.row(0)[1].flags.contains(CellFlags::WIDE_CONT));
        let m = find_in_grid(&g, "中A", true);
        assert_eq!(m, vec![MatchRange { row: 0, col_start: 0, col_end: 3 }]);
    }

    #[test]
    fn search_state_next_wraps() {
        let g = grid_with("ab ab ab", 16);
        let mut s = SearchState::new();
        s.input_char('a', &g);
        s.input_char('b', &g);
        assert_eq!(s.matches.len(), 3);
        assert_eq!(s.current, Some(0));
        s.next();
        assert_eq!(s.current, Some(1));
        s.next();
        assert_eq!(s.current, Some(2));
        s.next();
        assert_eq!(s.current, Some(0), "next should wrap from last to first");
    }

    #[test]
    fn search_state_prev_wraps() {
        let g = grid_with("ab ab ab", 16);
        let mut s = SearchState::new();
        s.input_char('a', &g);
        s.input_char('b', &g);
        assert_eq!(s.current, Some(0));
        s.prev();
        assert_eq!(s.current, Some(2), "prev should wrap from first to last");
        s.prev();
        assert_eq!(s.current, Some(1));
    }

    #[test]
    fn search_state_empty_matches_clears_current() {
        let g = grid_with("hello", 16);
        let mut s = SearchState::new();
        s.input_char('z', &g);
        assert!(s.matches.is_empty());
        assert_eq!(s.current, None);
        s.next();
        s.prev();
        assert_eq!(s.current, None);
    }

    #[test]
    fn backspace_recomputes() {
        let g = grid_with("hello", 16);
        let mut s = SearchState::new();
        s.input_char('h', &g);
        s.input_char('z', &g);
        assert!(s.matches.is_empty());
        s.backspace(&g);
        assert_eq!(s.matches.len(), 1);
    }

    // --- Haiku review fixes ---

    #[test]
    fn case_insensitive_handles_multichar_fold() {
        // Turkish dotted-İ lowercases to "i\u{0307}" (two chars). A
        // case-insensitive search for "i\u{0307}" should match a cell
        // containing İ.
        let g = grid_with("aİb", 16);
        let m = find_in_grid(&g, "i\u{0307}", false);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].col_start, 1);
        assert_eq!(m[0].col_end, 2);
    }

    #[test]
    fn wide_cell_match_extends_col_end_over_continuation() {
        // A wide CJK char occupies two cells (WIDE + WIDE_CONT). A match
        // ending on it must report col_end past the continuation cell so
        // the highlight covers the full glyph.
        let g = grid_with("中文", 16);
        let m = find_in_grid(&g, "中", true);
        assert_eq!(m.len(), 1);
        // wide '中' is at col 0; WIDE_CONT at col 1; col_end should be 2.
        assert_eq!(m[0].col_start, 0);
        assert_eq!(m[0].col_end, 2);
    }
}
