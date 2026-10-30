//! Kitty Unicode-placeholder decoding and geometry.
//!
//! A program that cannot address pixels draws an image by printing U+10EEEE cells. Each cell's
//! foreground colour names the image, its underline colour the placement, and up to three
//! combining diacritics give the image row, column and the image id's most significant byte.
//! Because the cells are ordinary text they scroll, clear and survive multiplexer redraws with
//! the rest of the screen; the renderer only has to find them and draw the matching image slice.
//!
//! This module is pure: it decodes cells into runs and computes where a run's slice of the fitted
//! image lands. Pixel storage and drawing belong to the app and the renderer.

use sonicterm_types::cell::{Cell, Color};

#[path = "kitty_placeholder_table.rs"]
mod kitty_placeholder_table;
use kitty_placeholder_table::ROW_COLUMN_DIACRITICS;

/// The character a kitty placeholder cell carries.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// The value a row/column diacritic encodes, or `None` for any other character.
#[must_use]
pub fn diacritic_value(diacritic: char) -> Option<u32> {
    ROW_COLUMN_DIACRITICS
        .iter()
        .position(|candidate| *candidate == diacritic)
        .map(|index| index as u32)
}

/// Whether `cell` is a kitty placeholder cell.
#[inline]
#[must_use]
pub fn is_placeholder(cell: &Cell) -> bool {
    cell.ch == PLACEHOLDER
}

/// A horizontal run of placeholder cells showing consecutive columns of one image row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaceholderRun {
    /// Image id: the foreground colour plus the high byte from the third diacritic.
    pub image_id: u32,
    /// Placement id from the underline colour; 0 when the cell has none.
    pub placement_id: u32,
    /// Grid column of the run's first cell.
    pub grid_col: u16,
    /// Image row (in cells, within the placement box) the run shows.
    pub image_row: u32,
    /// Image column (in cells, within the placement box) of the run's first cell.
    pub image_col: u32,
    /// Number of cells in the run.
    pub cell_count: u16,
}

/// One cell's decoded identity, used to continue the next cell.
#[derive(Clone, Copy)]
struct Decoded {
    grid_col: u16,
    fg: Color,
    underline: Option<Color>,
    image_id: u32,
    row: u32,
    col: u32,
}

/// The 24-bit value a colour carries for id purposes; the default colour carries none.
fn colour_id(color: Color) -> u32 {
    match color {
        Color::Rgb(red, green, blue) => {
            (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
        }
        Color::Indexed(index) => u32::from(index),
        Color::Default => 0,
    }
}

/// Decode the placeholder cells of one row into runs, in column order.
///
/// `cells` yields `(grid column, cell)` pairs left to right; non-placeholder cells may be omitted
/// or included. Inheritance follows kitty's rules: a cell missing its column (and high byte)
/// continues the left cell when that cell is directly to its left and has the same raw foreground
/// and underline colour — and, when only the row is given, the same row. Otherwise the missing
/// values are 0. Raw colours are compared, never theme-resolved ones, because the colour is data.
pub fn decode_row<'cell>(
    cells: impl IntoIterator<Item = (u16, &'cell Cell)>,
) -> Vec<PlaceholderRun> {
    let mut runs: Vec<PlaceholderRun> = Vec::new();
    let mut left: Option<Decoded> = None;
    for (grid_col, cell) in cells {
        if !is_placeholder(cell) {
            // When: the cell is ordinary text, it breaks both inheritance and the current run.
            left = None;
            continue;
        }
        let underline = cell.underline_color();
        let mut marks = cell.extras().unwrap_or("").chars().map(diacritic_value);
        let row_mark = marks.next().flatten();
        let col_mark = marks.next().flatten();
        let high_mark = marks.next().flatten();
        let continues = left.filter(|previous| {
            previous.grid_col.checked_add(1) == Some(grid_col)
                && previous.fg == cell.fg
                && previous.underline == underline
        });
        let (row, col, high) = match (row_mark, col_mark, continues) {
            (None, _, Some(previous)) => (previous.row, previous.col + 1, previous.image_id >> 24),
            (Some(row), None, Some(previous)) if previous.row == row => {
                (row, previous.col + 1, previous.image_id >> 24)
            }
            (Some(row), Some(col), Some(previous))
                if high_mark.is_none() && previous.row == row && previous.col + 1 == col =>
            {
                (row, col, previous.image_id >> 24)
            }
            _ => (row_mark.unwrap_or(0), col_mark.unwrap_or(0), high_mark.unwrap_or(0)),
        };
        let image_id = (high << 24) | colour_id(cell.fg);
        let placement_id = underline.map_or(0, colour_id);
        let extends_run = runs.last().is_some_and(|run| {
            run.image_id == image_id
                && run.placement_id == placement_id
                && run.image_row == row
                && u32::from(run.grid_col) + u32::from(run.cell_count) == u32::from(grid_col)
                && run.image_col + u32::from(run.cell_count) == col
        });
        if extends_run {
            if let Some(run) = runs.last_mut() {
                run.cell_count += 1;
            }
        } else {
            // When: extends_run is false, this cell starts a new image row, image or column jump, so it opens a run.
            runs.push(PlaceholderRun {
                image_id,
                placement_id,
                grid_col,
                image_row: row,
                image_col: col,
                cell_count: 1,
            });
        }
        left = Some(Decoded { grid_col, fg: cell.fg, underline, image_id, row, col });
    }
    runs
}

/// The placement box in cells: the requested columns and rows, or the image's natural size.
///
/// A missing dimension is the source pixel size divided by the cell size, rounded up, as kitty
/// does, so an image without `c`/`r` occupies the cells its pixels cover.
#[must_use]
pub fn placement_box(
    cols: Option<u32>,
    rows: Option<u32>,
    source_width_px: u32,
    source_height_px: u32,
    cell_width_px: f32,
    cell_height_px: f32,
) -> (u32, u32) {
    let natural = |pixels: u32, cell_px: f32| -> u32 {
        if cell_px <= 0.0 {
            // When: cell_px is not a positive size, no division is meaningful, so the box is one cell.
            return 1;
        }
        ((pixels as f32 / cell_px).ceil() as u32).max(1)
    };
    (
        cols.filter(|count| *count > 0).unwrap_or_else(|| natural(source_width_px, cell_width_px)),
        rows.filter(|count| *count > 0)
            .unwrap_or_else(|| natural(source_height_px, cell_height_px)),
    )
}

/// Where the image sits inside its placement box, in pixels relative to the box's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitRect {
    /// Left edge of the fitted image.
    pub x_px: f32,
    /// Top edge of the fitted image.
    pub y_px: f32,
    /// Width of the fitted image.
    pub width_px: f32,
    /// Height of the fitted image.
    pub height_px: f32,
    /// Box width in cells; cells at or past it draw nothing.
    pub box_cols: u32,
    /// Box height in cells; rows at or past it draw nothing.
    pub box_rows: u32,
}

/// Fit an image of `image_w` x `image_h` pixels into a `box_cols` x `box_rows` cell box,
/// preserving its aspect ratio and centring it, as kitty does for virtual placements.
///
/// `natural` is set when neither `c` nor `r` was given: the box is the image's own size rounded
/// up to whole cells, so the image is drawn at 1:1 from the box's top-left rather than enlarged
/// into the rounding.
#[must_use]
pub fn fit_in_box(
    box_cols: u32,
    box_rows: u32,
    image_w: u32,
    image_h: u32,
    cell_width_px: f32,
    cell_height_px: f32,
    natural: bool,
) -> FitRect {
    let box_w = box_cols as f32 * cell_width_px;
    let box_h = box_rows as f32 * cell_height_px;
    if natural {
        // When: natural is set, the box only rounds the image up to whole cells; it is drawn unscaled at the origin.
        return FitRect {
            x_px: 0.0,
            y_px: 0.0,
            width_px: image_w as f32,
            height_px: image_h as f32,
            box_cols,
            box_rows,
        };
    }
    let scale = if image_w == 0 || image_h == 0 {
        0.0
    } else {
        // When: image_w and image_h are nonzero, the smaller axis ratio fits the whole image inside the box.
        (box_w / image_w as f32).min(box_h / image_h as f32)
    };
    let width_px = image_w as f32 * scale;
    let height_px = image_h as f32 * scale;
    FitRect {
        x_px: (box_w - width_px) / 2.0,
        y_px: (box_h - height_px) / 2.0,
        width_px,
        height_px,
        box_cols,
        box_rows,
    }
}

/// A run's visible slice: its pixel rect relative to the run's first cell, and the image uv.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunSlice {
    /// Left edge relative to the run's first cell.
    pub dest_x: f32,
    /// Top edge relative to the run's first cell.
    pub dest_y: f32,
    /// Width in pixels.
    pub dest_w: f32,
    /// Height in pixels.
    pub dest_h: f32,
    /// Normalised source rect `[u0, v0, u1, v1]` within the image.
    pub source_uv: [f32; 4],
}

/// The part of the fitted image that the run's cells cover, or `None` when they show only
/// letterbox or lie outside the box.
///
/// The cells are intersected with the fitted rect before uv is derived, so letterbox cells never
/// stretch the image's edge pixels.
#[must_use]
pub fn run_slice(
    fit: &FitRect,
    image_row: u32,
    image_col: u32,
    cell_count: u16,
    cell_width_px: f32,
    cell_height_px: f32,
) -> Option<RunSlice> {
    if image_row >= fit.box_rows
        || image_col >= fit.box_cols
        || fit.width_px <= 0.0
        || fit.height_px <= 0.0
    {
        // When: image_row or image_col lies outside the fit box, or fit is empty, the run draws nothing.
        return None;
    }
    let last_col = (image_col + u32::from(cell_count)).min(fit.box_cols);
    let cells_left = image_col as f32 * cell_width_px;
    let cells_top = image_row as f32 * cell_height_px;
    let cells_right = last_col as f32 * cell_width_px;
    let cells_bottom = cells_top + cell_height_px;
    let left = cells_left.max(fit.x_px);
    let top = cells_top.max(fit.y_px);
    let right = cells_right.min(fit.x_px + fit.width_px);
    let bottom = cells_bottom.min(fit.y_px + fit.height_px);
    if right <= left || bottom <= top {
        // When: right and bottom do not pass left and top, these cells cover only letterbox around the image.
        return None;
    }
    Some(RunSlice {
        dest_x: left - cells_left,
        dest_y: top - cells_top,
        dest_w: right - left,
        dest_h: bottom - top,
        source_uv: [
            (left - fit.x_px) / fit.width_px,
            (top - fit.y_px) / fit.height_px,
            (right - fit.x_px) / fit.width_px,
            (bottom - fit.y_px) / fit.height_px,
        ],
    })
}

#[cfg(test)]
#[path = "kitty_placeholder_tests.rs"]
mod kitty_placeholder_tests;
