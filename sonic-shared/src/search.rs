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
