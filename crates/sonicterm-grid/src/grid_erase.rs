use super::*;

impl Grid {
    /// Erase from cursor to end of line (CSI 0 K).
    pub fn erase_line_to_end(&mut self) {
        self.erase_line_to_end_with(Cell::default());
    }

    /// Erase from cursor to end of line using `fill`.
    pub fn erase_line_to_end_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        let row = self.cursor.row as usize;
        let start = self.cursor.col as usize;
        let end = self.cols as usize;
        let (clear_start, clear_end) =
            self.wide_expanded_range(row, start, end.saturating_sub(start));
        for column in clear_start..clear_end {
            self.visible[row][column] = fill.clone();
        }
        self.mark_row(row as u16);
        self.content_changed_row(row as u16);
        // EL0 always erases the last column, so it revokes the outgoing wrap but never this row's incoming one.
        self.revoke_successor_wrap(row as u16);
        self.bump();
    }

    /// Erase from beginning of line to cursor inclusive (CSI 1 K).
    pub fn erase_line_to_start(&mut self) {
        self.erase_line_to_start_with(Cell::default());
    }

    /// Erase from beginning of line to cursor inclusive using `fill`.
    pub fn erase_line_to_start_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        let row = self.cursor.row as usize;
        let end = (self.cursor.col as usize).min(self.cols as usize - 1) + 1;
        let (clear_start, clear_end) = self.wide_expanded_range(row, 0, end);
        for column in clear_start..clear_end {
            self.visible[row][column] = fill.clone();
        }
        self.mark_row(row as u16);
        self.content_changed_row(row as u16);
        // EL1 through the last column revokes the outgoing wrap; the row keeps its incoming one.
        if clear_end >= usize::from(self.cols) {
            self.revoke_successor_wrap(row as u16);
        }
        self.bump();
    }

    /// Erase the entire current line (CSI 2 K).
    pub fn erase_line(&mut self) {
        self.erase_line_with(Cell::default());
    }

    /// Erase the entire current line using `fill`.
    pub fn erase_line_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        let row = self.cursor.row as usize;
        for cell in &mut self.visible[row] {
            *cell = fill.clone();
        }
        self.mark_row(row as u16);
        self.content_changed_row(row as u16);
        // EL2 erases the last column, so it revokes the outgoing wrap but never this row's incoming one.
        self.revoke_successor_wrap(row as u16);
        self.bump();
    }

    /// Erase from cursor to end of screen (CSI 0 J). This is what shells
    /// use to redraw a prompt — they jump to a row, erase below, and
    /// reprint. It must NOT touch rows above the cursor.
    pub fn erase_below(&mut self) {
        self.erase_below_with(Cell::default());
    }

    /// Erase from cursor to end of screen using `fill`.
    pub fn erase_below_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        self.erase_line_to_end_with(fill.clone());
        for row in (self.cursor.row as usize + 1)..self.rows as usize {
            for cell in &mut self.visible[row] {
                *cell = fill.clone();
            }
            self.visible[row].set_soft_wrapped_from_previous(false);
        }
        // Mark cursor.row..rows
        let first_row = self.cursor.row;
        let last_row = self.rows.saturating_sub(1);
        self.mark_range(first_row, last_row);
        self.content_changed_range(first_row, last_row);
        self.bump();
    }

    /// Erase from start of screen to cursor (CSI 1 J).
    pub fn erase_above(&mut self) {
        self.erase_above_with(Cell::default());
    }

    /// Erase from start of screen to cursor using `fill`.
    pub fn erase_above_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        for row in 0..self.cursor.row as usize {
            for cell in &mut self.visible[row] {
                *cell = fill.clone();
            }
            self.visible[row].set_soft_wrapped_from_previous(false);
        }
        self.visible[usize::from(self.cursor.row)].set_soft_wrapped_from_previous(false);
        self.erase_line_to_start_with(fill);
        // erase_line_to_start already marked cursor.row; mark 0..cursor.row too.
        let last_row = self.cursor.row;
        self.mark_range(0, last_row);
        self.content_changed_range(0, last_row);
        self.bump();
    }

    /// Erase the entire visible screen (CSI 2 J).
    pub fn erase_screen(&mut self) {
        self.erase_screen_with(Cell::default());
    }

    /// Erase the entire visible screen using `fill`.
    pub fn erase_screen_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        for row in &mut self.visible {
            for cell in row.iter_mut() {
                *cell = fill.clone();
            }
            row.set_soft_wrapped_from_previous(false);
        }
        self.mark_all();
        self.content_changed_all();
        self.bump();
    }

    /// Erase `cell_count` cells starting at (`row`, `col`), overwriting with the
    /// default (blank) Cell. Cursor unchanged. Used by CSI `X` (ECH).
    /// ECMA-48 §8.3.38: erased cells become BLANK with the current SGR
    /// rendition; callers that have the current rendition should use
    /// [`Self::erase_cells_with`].
    pub fn erase_cells(&mut self, row: u16, col: u16, cell_count: usize) {
        self.erase_cells_with(row, col, cell_count, Cell::default());
    }

    /// Erase `cell_count` cells starting at (`row`, `col`) using `fill`.
    pub fn erase_cells_with(&mut self, row: u16, col: u16, cell_count: usize, fill: Cell) {
        self.clear_pending_wrap();
        if row >= self.rows || col >= self.cols || cell_count == 0 {
            // When: `row >= self.rows`, `col >= self.cols`, or `cell_count == 0`, there is nothing to erase.
            return;
        }
        let row_index = row as usize;
        let start = col as usize;
        let raw_end = (start + cell_count).min(self.cols as usize);
        let (start, end) =
            self.wide_expanded_range(row_index, start, raw_end.saturating_sub(start));
        for column in start..end {
            self.visible[row_index][column] = fill.clone();
        }
        self.mark_row(row);
        self.content_changed_row(row);
        // ECH through the last column revokes the outgoing wrap; the row keeps its incoming one.
        if end >= usize::from(self.cols) {
            self.revoke_successor_wrap(row);
        }
        self.bump();
    }

    /// Insert `cell_count` blank cells at (`row`, `col`); shift trailing cells of
    /// the row right and drop the overflow at the right edge. Used by
    /// CSI `@` (ICH).
    pub fn insert_cells(&mut self, row: u16, col: u16, cell_count: usize) {
        self.insert_cells_with(row, col, cell_count, Cell::default());
    }

    /// Insert `cell_count` cells filled with `fill` at (`row`, `col`).
    pub fn insert_cells_with(&mut self, row: u16, col: u16, cell_count: usize, fill: Cell) {
        self.clear_pending_wrap();
        if row >= self.rows || col >= self.cols || cell_count == 0 {
            // When: `row >= self.rows`, `col >= self.cols`, or `cell_count == 0`, there is no insertion.
            return;
        }
        let row_index = row as usize;
        let start = col as usize;
        let cols = self.cols as usize;
        let cell_count = cell_count.min(cols - start);
        // Shift right FIRST, reading from the pristine row: dest =
        // start+cell_count..cols, src = start..cols-cell_count. Do NOT blank
        // [start..start+cell_count] beforehand — those cells are the source
        // for the first `cell_count` shifted destinations, so pre-clearing
        // them destroys the very text being "inserted before" and only the
        // originally-rightmost cell survives.
        // Wide-pair integrity is preserved by the shift (adjacency is kept)
        // and any half-pair split by the fill gap or pushed off the right
        // edge is cleaned by `repair_wide_row` below — so no pre-clear is
        // needed. (Contrast `delete_cells_with`, which also does not
        // pre-clear and reads ahead safely.)
        for dst in (start + cell_count..cols).rev() {
            self.visible[row_index][dst] = self.visible[row_index][dst - cell_count].clone();
        }
        for column in start..start + cell_count {
            self.visible[row_index][column] = fill.clone();
        }
        self.repair_wide_row(row_index, fill);
        self.mark_row(row);
        self.content_changed_row(row);
        // ICH shifts cells off the right edge, so it always rewrites the last column and revokes the outgoing wrap.
        self.revoke_successor_wrap(row);
        self.bump();
    }

    /// Delete `cell_count` cells at (`row`, `col`); shift trailing cells of the row
    /// left and fill the right edge with blanks. Used by CSI `P` (DCH).
    pub fn delete_cells(&mut self, row: u16, col: u16, cell_count: usize) {
        self.delete_cells_with(row, col, cell_count, Cell::default());
    }

    /// Delete `cell_count` cells and fill the right edge with `fill`.
    pub fn delete_cells_with(&mut self, row: u16, col: u16, cell_count: usize, fill: Cell) {
        self.clear_pending_wrap();
        if row >= self.rows || col >= self.cols || cell_count == 0 {
            // When: `row >= self.rows`, `col >= self.cols`, or `cell_count == 0`, there is no deletion.
            return;
        }
        let row_index = row as usize;
        let start = col as usize;
        let cols = self.cols as usize;
        let cell_count = cell_count.min(cols - start);
        let (start, end) = self.wide_expanded_range(row_index, start, cell_count);
        let cell_count = end.saturating_sub(start).min(cols - start);
        for column in start..cols - cell_count {
            self.visible[row_index][column] = self.visible[row_index][column + cell_count].clone();
        }
        for column in cols - cell_count..cols {
            self.visible[row_index][column] = fill.clone();
        }
        self.repair_wide_row(row_index, fill);
        self.mark_row(row);
        self.content_changed_row(row);
        // DCH fills the right edge, so it always rewrites the last column and revokes the outgoing wrap.
        self.revoke_successor_wrap(row);
        self.bump();
    }
}

#[cfg(test)]
#[path = "grid_erase_tests.rs"]
mod grid_erase_tests;
