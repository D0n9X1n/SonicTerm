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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub hyperlink: Option<HyperlinkId>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: None,
        }
    }
}

/// A row of cells.
pub type Row = Vec<Cell>;

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
        };
        self.alt_screen = Some(Box::new(saved));
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
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        if width == 0 {
            return;
        }
        if self.cursor.col + width > self.cols {
            self.linefeed();
            self.cursor.col = 0;
        }
        let (r, c) = (self.cursor.row as usize, self.cursor.col as usize);
        let cell_flags = if width == 2 { flags | CellFlags::WIDE } else { flags };
        self.visible[r][c] = Cell { ch, fg, bg, flags: cell_flags, hyperlink };
        if width == 2 && c + 1 < self.cols as usize {
            self.visible[r][c + 1] =
                Cell { ch: ' ', fg, bg, flags: flags | CellFlags::WIDE_CONT, hyperlink };
        }
        self.cursor.col += width;
        self.bump();
    }

    pub fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.bump();
    }

    pub fn linefeed(&mut self) {
        if self.cursor.row + 1 >= self.rows {
            self.scroll_up(1);
        } else {
            self.cursor.row += 1;
        }
        self.bump();
    }

    pub fn backspace(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
        self.bump();
    }

    pub fn tab(&mut self) {
        let next = ((self.cursor.col / 8) + 1) * 8;
        self.cursor.col = next.min(self.cols.saturating_sub(1));
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
        self.bump();
    }

    /// Erase from cursor to end of line (CSI 0 K).
    pub fn erase_line_to_end(&mut self) {
        let r = self.cursor.row as usize;
        for c in self.cursor.col as usize..self.cols as usize {
            self.visible[r][c] = Cell::default();
        }
        self.bump();
    }

    /// Erase from beginning of line to cursor inclusive (CSI 1 K).
    pub fn erase_line_to_start(&mut self) {
        let r = self.cursor.row as usize;
        for c in 0..=(self.cursor.col as usize).min(self.cols as usize - 1) {
            self.visible[r][c] = Cell::default();
        }
        self.bump();
    }

    /// Erase the entire current line (CSI 2 K).
    pub fn erase_line(&mut self) {
        let r = self.cursor.row as usize;
        for cell in &mut self.visible[r] {
            *cell = Cell::default();
        }
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
        self.bump();
    }

    /// Erase the entire visible screen (CSI 2 J).
    pub fn erase_screen(&mut self) {
        for row in &mut self.visible {
            for cell in row.iter_mut() {
                *cell = Cell::default();
            }
        }
        self.bump();
    }

    /// Move cursor to (row, col), clamping to grid bounds.
    pub fn goto(&mut self, row: u16, col: u16) {
        self.cursor.row = row.min(self.rows.saturating_sub(1));
        self.cursor.col = col.min(self.cols.saturating_sub(1));
        self.bump();
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
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
