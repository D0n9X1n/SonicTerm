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

#[cfg(test)]
mod tests {
    use super::*;
    use sonicterm_grid::grid::Color;

    fn put_str(g: &mut Grid, s: &str) {
        for ch in s.chars() {
            g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
    }

    #[test]
    fn selection_preserves_zwj_family_cluster() {
        // 👨‍👩‍👧 — ZWJs must survive copy via cell.extras().
        let mut g = Grid::new(20, 1);
        put_str(&mut g, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
        let sel = Selection { start: (0, 0), end: (0, 19) };
        assert_eq!(sel.as_text(&g), "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
    }

    #[test]
    fn selection_preserves_combining_mark() {
        // 'e' + COMBINING ACUTE ACCENT (U+0301) → "é" (decomposed).
        let mut g = Grid::new(10, 1);
        put_str(&mut g, "e\u{0301}");
        let sel = Selection { start: (0, 0), end: (0, 9) };
        assert_eq!(sel.as_text(&g), "e\u{0301}");
    }
}
