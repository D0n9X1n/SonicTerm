//! Grid selection model.
//!
//! Coordinates are grid cells, not pixels. (0,0) is top-left of the visible
//! region. The selection is anchored at `start` and extends to `end`; the
//! pair may be in any order.

use sonic_core::grid::{CellFlags, Grid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: (u16, u16), // (row, col)
    pub end: (u16, u16),
}

impl Selection {
    pub fn new(row: u16, col: u16) -> Self {
        Self { start: (row, col), end: (row, col) }
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

    /// Empty selection — start == end.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
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
            for cell in &row[col_start..col_end] {
                if cell.flags.contains(CellFlags::WIDE_CONT) {
                    continue;
                }
                line.push(cell.ch);
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

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
