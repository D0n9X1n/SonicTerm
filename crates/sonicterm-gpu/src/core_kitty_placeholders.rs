//! Kitty Unicode-placeholder drawing: find placeholder runs in a pane's visible rows and turn each
//! into an image placement that shows the run's slice of the fitted image.
//!
//! Placeholder cells are ordinary text, so they scroll, clear and enter scrollback with the grid.
//! This phase runs only for panes holding a virtual placement and only when assembly proceeds, so
//! panes without kitty images, and unchanged frames, pay nothing.

use sonicterm_render_model::kitty_placeholder::{decode_row, fit_in_box, placement_box, run_slice};
use sonicterm_render_model::{InlineImage, KittyShow};

use super::*;

/// One run's part of a fitted image, in surface pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PlaceholderSlice {
    /// The run's visible part of the image, before pane and surface clipping.
    pub(super) dest: PaneRect,
    /// The normalised image rect `[u0, v0, u1, v1]` that `dest` shows.
    pub(super) source_uv: [f32; 4],
}

/// The virtual placement a placeholder cell names: the exact `(image, placement)` pair, or, only
/// for placement id 0, any virtual placement of the image.
fn find_virtual<'img>(
    images: &'img [InlineImage],
    image_id: u32,
    placement_id: u32,
) -> Option<(&'img InlineImage, u32, u32)> {
    let virtual_of = |image: &'img InlineImage| match image.kitty {
        Some(tag) if tag.image_id == image_id => match tag.show {
            KittyShow::Virtual { cols, rows } => Some((image, tag.placement_id, cols, rows)),
            KittyShow::Upload | KittyShow::Anchored => None,
        },
        _ => None,
    };
    let mut candidates = images.iter().filter_map(virtual_of);
    let found = if placement_id == 0 {
        candidates.next()
    } else {
        // When: placement_id is nonzero, only that placement serves; another placement of the image never stands in.
        candidates.find(|(_, candidate, _, _)| *candidate == placement_id)
    };
    found.map(|(image, _, cols, rows)| (image, cols, rows))
}

/// Whether `images` holds any virtual placement, so the pane's rows are worth scanning.
pub(super) fn has_virtual_placement(images: &[InlineImage]) -> bool {
    images.iter().any(
        |image| matches!(image.kitty, Some(tag) if matches!(tag.show, KittyShow::Virtual { .. })),
    )
}

/// Every placeholder run in the pane's visible rows, as `(image, slice)` pairs in row order.
pub(super) fn placeholder_slices<'img>(
    grid: &Grid,
    planned: &PlannedPane,
    images: &'img [InlineImage],
    origin: (f32, f32),
    cell_size: (f32, f32),
) -> Vec<(&'img InlineImage, PlaceholderSlice)> {
    let rows = planned.rows().filter_map(|(slot, abs_row)| Some((slot, grid.row_at_abs(abs_row)?)));
    slices_for_rows(rows, images, origin, cell_size)
}

/// The placeholder slices of `rows`, each given as its viewport slot and grid line.
fn slices_for_rows<'img, 'grid>(
    rows: impl Iterator<Item = (u16, &'grid sonicterm_render_model::boundary::grid::line::Line)>,
    images: &'img [InlineImage],
    origin: (f32, f32),
    cell_size: (f32, f32),
) -> Vec<(&'img InlineImage, PlaceholderSlice)> {
    let (cell_w, cell_h) = cell_size;
    let mut slices = Vec::new();
    for (slot, row) in rows {
        let cells = row.iter().enumerate().map(|(col, cell)| (col as u16, cell));
        for run in decode_row(cells) {
            let Some((image, cols, rows)) = find_virtual(images, run.image_id, run.placement_id)
            else {
                // When: find_virtual matches no placement, the run names a deleted or unknown image and draws nothing.
                continue;
            };
            let (box_cols, box_rows) = placement_box(
                (cols != 0).then_some(cols),
                (rows != 0).then_some(rows),
                image.width,
                image.height,
                cell_w,
                cell_h,
            );
            let fit = fit_in_box(box_cols, box_rows, image.width, image.height, cell_w, cell_h);
            let Some(slice) =
                run_slice(&fit, run.image_row, run.image_col, run.cell_count, cell_w, cell_h)
            else {
                // When: run_slice is None, the run shows only letterbox or lies outside the box.
                continue;
            };
            let run_x = origin.0 + f32::from(run.grid_col) * cell_w;
            let run_y = origin.1 + f32::from(slot) * cell_h;
            slices.push((
                image,
                PlaceholderSlice {
                    dest: PaneRect::new(
                        run_x + slice.dest_x,
                        run_y + slice.dest_y,
                        slice.dest_w,
                        slice.dest_h,
                    ),
                    source_uv: slice.source_uv,
                },
            ));
        }
    }
    slices
}

#[cfg(test)]
#[path = "core_kitty_placeholders_tests.rs"]
mod core_kitty_placeholders_tests;
