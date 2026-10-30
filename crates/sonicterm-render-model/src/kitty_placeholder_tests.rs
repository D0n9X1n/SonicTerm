//! Pins the kitty Unicode-placeholder decoding: the diacritic table, how a cell's colours and marks
//! name an image, placement, row and column, the left-neighbour inheritance rules, and the geometry
//! that maps a run of placeholder cells onto its slice of the fitted image.

use sonicterm_types::cell::{Cell, Color};

use super::*;

/// A placeholder cell whose foreground carries `image_rgb` and whose marks are `marks`.
fn placeholder(image_rgb: (u8, u8, u8), marks: &[char]) -> Cell {
    let mut cell = Cell::default();
    cell.ch = PLACEHOLDER;
    cell.fg = Color::Rgb(image_rgb.0, image_rgb.1, image_rgb.2);
    if !marks.is_empty() {
        // When: marks are given they become the cell's zero-width cluster, as the VT stores them.
        cell.set_extras(Some(marks.iter().collect::<String>().into_boxed_str()));
    }
    cell
}

/// The diacritic encoding `value`.
fn mark(value: usize) -> char {
    ROW_COLUMN_DIACRITICS[value]
}

#[test]
fn the_diacritic_table_matches_kittys_order_and_inverts() {
    // kitty's table has 297 marks; the first and last pin the order, and every mark decodes to its index.
    assert_eq!(ROW_COLUMN_DIACRITICS.len(), 297);
    assert_eq!(ROW_COLUMN_DIACRITICS[0], '\u{0305}');
    assert_eq!(ROW_COLUMN_DIACRITICS[7], '\u{033F}');
    assert_eq!(ROW_COLUMN_DIACRITICS[296], '\u{1D244}');
    for (value, diacritic) in ROW_COLUMN_DIACRITICS.iter().enumerate() {
        assert_eq!(diacritic_value(*diacritic), Some(value as u32));
    }
    assert_eq!(diacritic_value('a'), None);
}

#[test]
fn a_cell_without_the_placeholder_character_decodes_to_nothing() {
    // Only U+10EEEE starts a placeholder; ordinary text with the same colour is text.
    let mut cell = placeholder((1, 2, 3), &[mark(0), mark(0)]);
    cell.ch = 'x';
    assert!(decode_row([(0, &cell)]).is_empty());
}

#[test]
fn foreground_colour_and_third_mark_form_the_image_id() {
    // The 24-bit foreground is the low bytes; a third diacritic supplies the most significant byte.
    let cell = placeholder((0x0A, 0x0B, 0x0C), &[mark(2), mark(5), mark(0x7F)]);
    let runs = decode_row([(4, &cell)]);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].image_id, 0x7F0A_0B0C);
    assert_eq!(
        (runs[0].grid_col, runs[0].image_row, runs[0].image_col, runs[0].cell_count),
        (4, 2, 5, 1)
    );
}

#[test]
fn an_indexed_foreground_names_a_small_image_id() {
    // A 256-colour foreground names images 0..=255.
    let mut cell = placeholder((0, 0, 0), &[mark(0), mark(0)]);
    cell.fg = Color::Indexed(42);
    assert_eq!(decode_row([(0, &cell)])[0].image_id, 42);
}

#[test]
fn the_underline_colour_is_the_placement_id() {
    // SGR 58 on a placeholder selects the placement; without it the placement id is 0.
    let mut cell = placeholder((0, 0, 9), &[mark(0), mark(0)]);
    assert_eq!(decode_row([(0, &cell)])[0].placement_id, 0);
    cell.set_underline_color(Some(Color::Rgb(0, 1, 2)));
    assert_eq!(decode_row([(0, &cell)])[0].placement_id, 0x0102);
}

#[test]
fn cells_with_no_marks_continue_the_left_neighbours_row() {
    // Kitty: with no diacritics, a cell takes the left cell's row and next column, and its id high byte.
    let first = placeholder((0, 0, 7), &[mark(3), mark(4), mark(1)]);
    let second = placeholder((0, 0, 7), &[]);
    let third = placeholder((0, 0, 7), &[]);
    let runs = decode_row([(0, &first), (1, &second), (2, &third)]);
    assert_eq!(runs.len(), 1, "one contiguous run");
    assert_eq!(
        (runs[0].image_id, runs[0].image_row, runs[0].image_col, runs[0].cell_count),
        (0x0100_0007, 3, 4, 3)
    );
}

#[test]
fn a_row_only_mark_inherits_the_column_from_the_same_row() {
    // One diacritic is the row; the column continues only when the left cell has that same row.
    let first = placeholder((0, 0, 7), &[mark(2), mark(6)]);
    let same_row = placeholder((0, 0, 7), &[mark(2)]);
    let other_row = placeholder((0, 0, 7), &[mark(3)]);
    let runs = decode_row([(0, &first), (1, &same_row), (2, &other_row)]);
    assert_eq!(runs.len(), 2);
    assert_eq!((runs[0].image_row, runs[0].image_col, runs[0].cell_count), (2, 6, 2));
    assert_eq!((runs[1].image_row, runs[1].image_col, runs[1].cell_count), (3, 0, 1));
}

#[test]
fn inheritance_breaks_when_the_colours_change() {
    // A different image or placement colour on the left cell means nothing is inherited.
    let first = placeholder((0, 0, 7), &[mark(1), mark(1), mark(9)]);
    let other_image = placeholder((0, 0, 8), &[]);
    let mut other_placement = placeholder((0, 0, 8), &[]);
    other_placement.set_underline_color(Some(Color::Indexed(3)));
    let runs = decode_row([(0, &first), (1, &other_image), (2, &other_placement)]);
    assert_eq!(runs.len(), 3);
    assert_eq!(
        (runs[1].image_id, runs[1].image_row, runs[1].image_col),
        (8, 0, 0),
        "no high byte, origin"
    );
    assert_eq!((runs[2].placement_id, runs[2].image_row, runs[2].image_col), (3, 0, 0));
}

#[test]
fn explicit_non_consecutive_columns_start_a_new_run() {
    // Two explicit marks whose column does not follow the left cell's start a separate run.
    let first = placeholder((0, 0, 7), &[mark(0), mark(0)]);
    let jump = placeholder((0, 0, 7), &[mark(0), mark(5)]);
    let runs = decode_row([(0, &first), (1, &jump)]);
    assert_eq!(runs.len(), 2);
    assert_eq!((runs[1].grid_col, runs[1].image_col), (1, 5));
}

#[test]
fn a_gap_in_grid_columns_ends_a_run() {
    // Placeholders separated by a non-placeholder cell are not contiguous, even when the encoding continues.
    let first = placeholder((0, 0, 7), &[mark(0), mark(0)]);
    let later = placeholder((0, 0, 7), &[mark(0), mark(1)]);
    let runs = decode_row([(0, &first), (2, &later)]);
    assert_eq!(runs.len(), 2);
}

#[test]
fn natural_size_derives_the_cell_box_from_source_pixels() {
    // Without c/r, the box is the image's pixel size rounded up to whole cells.
    assert_eq!(placement_box(None, None, 100, 35, 10.0, 20.0), (10, 2));
    assert_eq!(placement_box(Some(16), None, 100, 35, 10.0, 20.0), (16, 2));
    assert_eq!(placement_box(Some(4), Some(3), 100, 35, 10.0, 20.0), (4, 3));
}

#[test]
fn the_image_is_fitted_into_its_box_preserving_aspect_and_centred() {
    // A 2:1 image in a 4x4-cell box of 10x10 px cells fills the width and is centred vertically.
    let fit = fit_in_box(4, 4, 200, 100, 10.0, 10.0);
    assert_eq!((fit.x_px, fit.y_px, fit.width_px, fit.height_px), (0.0, 10.0, 40.0, 20.0));
}

#[test]
fn a_run_maps_to_its_slice_of_the_fitted_image() {
    // Box 4x4 cells (10 px), fitted image at y 10..30. Row 1 cells 1..3 cover x 10..30, y 10..20:
    // half of the image's height and the middle half of its width.
    let fit = fit_in_box(4, 4, 200, 100, 10.0, 10.0);
    let slice = run_slice(&fit, 1, 1, 2, 10.0, 10.0).expect("row 1 overlaps the image");
    assert_eq!((slice.dest_x, slice.dest_y, slice.dest_w, slice.dest_h), (0.0, 0.0, 20.0, 10.0));
    assert_eq!(slice.source_uv, [0.25, 0.0, 0.75, 0.5]);
}

#[test]
fn letterbox_rows_draw_nothing_and_partial_cells_are_clipped() {
    // Row 0 is entirely letterbox; row 3 likewise. Row 2 is clipped at the image's bottom edge.
    let fit = fit_in_box(4, 4, 200, 100, 10.0, 10.0);
    assert!(run_slice(&fit, 0, 0, 4, 10.0, 10.0).is_none());
    assert!(run_slice(&fit, 3, 0, 4, 10.0, 10.0).is_none());
    let bottom = run_slice(&fit, 2, 0, 4, 10.0, 10.0).expect("row 2 overlaps");
    assert_eq!((bottom.dest_y, bottom.dest_h), (0.0, 10.0));
    assert_eq!(bottom.source_uv, [0.0, 0.5, 1.0, 1.0]);
}

#[test]
fn cells_outside_the_box_draw_nothing() {
    // Encoded rows or columns past the placement box are ignored rather than stretched.
    let fit = fit_in_box(4, 4, 200, 100, 10.0, 10.0);
    assert!(run_slice(&fit, 9, 0, 1, 10.0, 10.0).is_none());
    assert!(run_slice(&fit, 1, 9, 1, 10.0, 10.0).is_none());
}
