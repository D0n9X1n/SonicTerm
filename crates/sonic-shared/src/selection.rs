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
mod tests {
    use sonic_core::grid::{CellFlags, Color, Grid};

    use super::*;

    fn grid_with(text: &[&str]) -> Grid {
        let cols = text.iter().map(|s| s.chars().count()).max().unwrap_or(1) as u16;
        let rows = text.len() as u16;
        let mut g = Grid::new(cols, rows);
        for (r, line) in text.iter().enumerate() {
            g.cursor.row = r as u16;
            g.cursor.col = 0;
            for ch in line.chars() {
                g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
            }
        }
        g
    }

    #[test]
    fn single_cell_selection_contains_only_itself() {
        let s = Selection::new(2, 5);
        assert!(s.contains(2, 5));
        assert!(!s.contains(2, 6));
        assert!(s.is_empty());
    }

    #[test]
    fn extend_grows_selection() {
        let mut s = Selection::new(1, 1);
        s.extend(3, 7);
        assert!(s.contains(2, 4));
        assert!(s.contains(3, 7));
        assert!(!s.contains(4, 0));
    }

    #[test]
    fn normalized_reorders_reverse_selection() {
        let mut s = Selection::new(3, 7);
        s.extend(1, 2);
        let (a, b) = s.normalized();
        assert_eq!(a, (1, 2));
        assert_eq!(b, (3, 7));
    }

    #[test]
    fn to_string_single_row() {
        let g = grid_with(&["hello world", "second line"]);
        let mut s = Selection::new(0, 0);
        s.extend(0, 4);
        assert_eq!(s.as_text(&g), "hello");
    }

    #[test]
    fn to_string_multi_row() {
        let g = grid_with(&["abc", "def", "ghi"]);
        let mut s = Selection::new(0, 1);
        s.extend(2, 1);
        assert_eq!(s.as_text(&g), "bc\ndef\ngh");
    }

    #[test]
    fn to_string_trims_trailing_spaces() {
        let mut g = Grid::new(10, 1);
        for ch in "hi".chars() {
            g.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
        }
        let mut s = Selection::new(0, 0);
        s.extend(0, 9);
        assert_eq!(s.as_text(&g), "hi");
    }
}
