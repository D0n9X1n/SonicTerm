//! Vim-style keyboard copy-mode state.
//!
//! Coordinates are `(col, row)` pairs in visible-grid cell space. The live
//! terminal cursor keeps moving independently; copy mode owns this separate
//! cursor plus an optional selection anchor.

use sonic_cfg::url_scan::find_urls;
use sonic_grid::grid::{CellFlags, Grid, Row};

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
    pub fn from_grid(grid: &Grid) -> Self {
        let mut hints = Vec::new();
        for row_idx in grid.scrollback_len()..grid.scrollback_len() + grid.rows as usize {
            let Some(line) = row_text(grid, row_idx) else { continue };
            for m in find_urls(&line) {
                let Some(hint) = nth_hint(hints.len()) else { return Self { hints } };
                let col_start = byte_to_char_col(&line, m.start);
                let col_end = byte_to_char_col(&line, m.end).saturating_sub(1);
                hints.push(QuickSelectHint { hint, row: row_idx, col_start, col_end, text: m.url });
            }
        }
        Self { hints }
    }

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
}

impl CopyModeState {
    pub fn new_at(pos: (usize, usize)) -> Self {
        Self { cursor: pos, anchor: None, mode: CopyMode::Cursor, quick_select: None }
    }

    pub fn move_left(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.0 = self.cursor.0.saturating_sub(1);
    }

    pub fn move_right(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.0 = (self.cursor.0 + 1).min(max_col(grid));
    }

    pub fn move_up(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = self.cursor.1.saturating_sub(1);
    }

    pub fn move_down(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = (self.cursor.1 + 1).min(max_row(grid));
    }

    pub fn move_word_fwd(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        let mut pos = self.cursor;
        let current_is_word = char_at(grid, pos).is_some_and(is_word_char);

        loop {
            let Some(next) = next_pos(grid, pos) else {
                self.cursor = (max_col(grid), max_row(grid));
                return;
            };
            pos = next;
            let ch = char_at(grid, pos);
            if current_is_word && ch.is_some_and(|c| !is_word_char(c)) {
                break;
            }
            if !current_is_word && ch.is_some_and(is_word_char) {
                self.cursor = pos;
                return;
            }
        }

        while let Some(next) = next_pos(grid, pos) {
            pos = next;
            if char_at(grid, pos).is_some_and(is_word_char) {
                self.cursor = pos;
                return;
            }
        }
        self.cursor = (max_col(grid), max_row(grid));
    }

    pub fn move_word_back(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        let mut pos = self.cursor;

        while let Some(prev) = prev_pos(grid, pos) {
            pos = prev;
            if char_at(grid, pos).is_some_and(is_word_char) {
                break;
            }
        }

        while let Some(prev) = prev_pos(grid, pos) {
            if !char_at(grid, prev).is_some_and(is_word_char) {
                break;
            }
            pos = prev;
        }
        self.cursor = pos;
    }

    pub fn move_line_start(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.0 = 0;
    }

    pub fn move_line_end(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        let row = visible_row(grid, self.cursor.1);
        let last = row.and_then(last_non_blank_col).unwrap_or_else(|| max_col(grid));
        self.cursor.0 = last.min(max_col(grid));
    }

    pub fn move_top(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = 0;
        self.cursor.0 = self.cursor.0.min(max_col(grid));
    }

    pub fn move_bottom(&mut self, grid: &Grid) {
        self.clamp_to_grid(grid);
        self.cursor.1 = max_row(grid);
        self.cursor.0 = self.cursor.0.min(max_col(grid));
    }

    pub fn start_select(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.mode = CopyMode::Select;
    }

    pub fn selected_range(&self) -> Option<((usize, usize), (usize, usize))> {
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
        Some((0, pos.1 + 1))
    } else {
        None
    }
}

fn prev_pos(grid: &Grid, pos: (usize, usize)) -> Option<(usize, usize)> {
    if pos.0 > 0 {
        Some((pos.0 - 1, pos.1))
    } else if pos.1 > 0 {
        Some((max_col(grid), pos.1 - 1))
    } else {
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
fn row_text(grid: &Grid, row: usize) -> Option<String> {
    let row = visible_row(grid, row)?;
    let mut text = String::with_capacity(row.len());
    for cell in row {
        if cell.flags.contains(CellFlags::WIDE_CONT) {
            continue;
        }
        text.push(cell.ch);
    }
    Some(text)
}

fn nth_hint(idx: usize) -> Option<char> {
    (idx < 26).then(|| (b'a' + idx as u8) as char)
}

fn byte_to_char_col(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}
