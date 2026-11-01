//! Where the cursor goes after a kitty image is displayed.
//!
//! kitty moves the cursor after a direct placement (`a=T`, or `a=p` without `U=1`) `c` columns
//! right and `r - 1` rows down, so the next line of text starts below the image; `C=1` keeps the
//! cursor still, and a virtual placement never moves it. A move past the right edge wraps to the
//! next row, and one past the bottom margin scrolls the region. A placement without `c` or `r`
//! takes its cells from the image's pixel size and the cell size, as kitty sizes it.
//!
//! Only the host knows whether an image decoded and was stored or placed, so the parser ends its
//! advance prefix after a command that would move the cursor, and the host moves it through
//! [`Parser::move_cursor_past_kitty_image`] once the image is held, before later bytes are parsed.

use super::*;

/// The most cells one side of a placement box covers: a grid never has more rows or columns.
const MAX_BOX_CELLS: u32 = u16::MAX as u32;

/// The cells a placement covers: `c` and `r` when given; otherwise from the image's pixel size,
/// keeping its aspect ratio when one of them is given. `None` when a missing side needs a cell
/// size that is not known. Each side is at most [`MAX_BOX_CELLS`].
fn placement_cells(
    cols: u32,
    rows: u32,
    source_px: (u32, u32),
    cell_px: Option<(f32, f32)>,
) -> Option<(u32, u32)> {
    if cols != 0 && rows != 0 {
        // When: cols and rows are both given, the box is exactly what was asked for.
        return Some((cols.min(MAX_BOX_CELLS), rows.min(MAX_BOX_CELLS)));
    }
    let (source_width, source_height) = source_px;
    if source_width == 0 || source_height == 0 {
        // When: source_width or source_height is 0, the image has no size to take cells from.
        return None;
    }
    let (cell_width, cell_height) = cell_px?;
    let cells = |pixels: f64, cell: f32| {
        (pixels / f64::from(cell)).ceil().clamp(1.0, f64::from(MAX_BOX_CELLS)) as u32
    };
    let aspect = f64::from(source_width) / f64::from(source_height);
    let (cols, rows) = match (cols, rows) {
        (0, 0) => (
            cells(f64::from(source_width), cell_width),
            cells(f64::from(source_height), cell_height),
        ),
        (0, rows) => (cells(f64::from(cell_height) * f64::from(rows) * aspect, cell_width), rows),
        (cols, _) => (cols, cells(f64::from(cell_width) * f64::from(cols) / aspect, cell_height)),
    };
    Some((cols.min(MAX_BOX_CELLS), rows.min(MAX_BOX_CELLS)))
}

impl KittyCommand {
    /// Whether displaying this command's image moves the cursor: a direct placement (`a=T`, or
    /// `a=p` without `U=1`) without `C=1`.
    #[must_use]
    pub fn moves_cursor(&self) -> bool {
        matches!(self.action, KittyAction::TransmitAndDisplay | KittyAction::Place)
            && !self.virtual_placement
            && !self.no_cursor_move
    }
}

impl Parser {
    /// Move the cursor past a kitty image the host has just stored or placed, as kitty does: by
    /// the placement's `cols` x `rows` box, or, for a side not given, by the cells the image's
    /// `source_px` covers. Without a known cell size a side that is not given leaves the cursor
    /// still.
    pub fn move_cursor_past_kitty_image(&mut self, cols: u32, rows: u32, source_px: (u32, u32)) {
        if let Some((cols, rows)) = placement_cells(cols, rows, source_px, self.cell_px) {
            self.move_cursor_by_box(cols, rows);
        }
    }

    /// Move the cursor `cols` right and `rows - 1` down; wrap past the right edge and, when the
    /// cursor started inside the margins, scroll the region as it passes the bottom margin.
    ///
    /// A cursor below the margins moves down within the screen, as a line feed there does.
    fn move_cursor_by_box(&mut self, cols: u32, rows: u32) {
        let performer = &mut self.performer;
        let grid_cols = u32::from(performer.grid.cols);
        let start_row = u32::from(performer.grid.cursor.row);
        let mut col = u32::from(performer.physical_cursor_col()).saturating_add(cols);
        let mut row = start_row.saturating_add(rows.saturating_sub(1));
        if col >= grid_cols {
            // The move passes the right edge, so the cursor wraps to the next row's start.
            col = 0;
            row = row.saturating_add(1);
        }
        let (top, bottom) = performer.effective_scroll_region();
        let in_margins = (u32::from(top)..=u32::from(bottom)).contains(&start_row);
        if in_margins && row > u32::from(bottom) {
            // The region scrolls by every row the image needs, so text after it starts below it.
            let overflow = row - u32::from(bottom);
            if top == 0 && u32::from(bottom) + 1 == u32::from(performer.grid.rows) {
                // The region is the whole screen, so the rows enter history as line feeds would and
                // earlier images stay beside their text there.
                performer.grid.scroll_up_counting(u64::from(overflow), Cell::default());
            } else {
                // When: top or bottom leaves a proper sub-region, one region height is a clear, so
                // no more is shifted.
                let region_rows = u32::from(bottom - top) + 1;
                let lines = u16::try_from(overflow.min(region_rows)).unwrap_or(u16::MAX);
                performer.grid.scroll_region_up_with(top, bottom, lines, Cell::default());
            }
            row = u32::from(bottom);
        }
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        let col = u16::try_from(col).unwrap_or(u16::MAX);
        performer.grid.goto(row, col);
    }
}

#[cfg(test)]
#[path = "vt_kitty_cursor_tests.rs"]
mod vt_kitty_cursor_tests;
