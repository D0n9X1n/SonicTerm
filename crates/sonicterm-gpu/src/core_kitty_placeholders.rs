//! Kitty Unicode-placeholder drawing: find placeholder runs in a pane's visible rows and turn each
//! into an image placement that shows the run's slice of the fitted image.
//!
//! Placeholder cells are ordinary text, so they scroll, clear and enter scrollback with the grid.
//! This phase runs only for panes holding a virtual placement and only when assembly proceeds, so
//! panes without kitty images, and unchanged frames, pay nothing.

use sonicterm_render_model::kitty_placeholder::{
    anchored_size_px, decode_row, fit_in_box, placement_box, run_slice,
};
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
) -> Option<(&'img InlineImage, sonicterm_render_model::KittyTag)> {
    let virtual_of = |image: &'img InlineImage| match image.kitty {
        Some(tag) if tag.image_id == image_id => match tag.show {
            KittyShow::Virtual { .. } => Some((image, tag)),
            KittyShow::Upload | KittyShow::Anchored { .. } => None,
        },
        _ => None,
    };
    let mut candidates = images.iter().filter_map(virtual_of);
    if placement_id == 0 {
        candidates.next()
    } else {
        // When: placement_id is nonzero, only that placement serves; another placement of the image never stands in.
        candidates.find(|(_, tag)| tag.placement_id == placement_id)
    }
}

/// Whether `images` holds any virtual placement, so the pane's rows are worth scanning.
pub(super) fn has_virtual_placement(images: &[InlineImage]) -> bool {
    images.iter().any(
        |image| matches!(image.kitty, Some(tag) if matches!(tag.show, KittyShow::Virtual { .. })),
    )
}

/// How far a pane's text has moved since its images were anchored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AnchorScroll {
    /// The grid's `lines_scrolled` now; an image moves up by this minus its `scroll_mark`.
    pub(super) lines_scrolled: u64,
    /// Rows the viewport shows above the live screen when scrolled back; images move down by it.
    pub(super) rows_back: u64,
}

/// Where an image anchored at the cursor draws: its anchor cell, moved with the text scrolled
/// since, and its kitty box or, untagged, its decoded pixel size.
fn anchored_dest(
    image: &InlineImage,
    origin: (f32, f32),
    cell_size: (f32, f32),
    scroll: AnchorScroll,
) -> PaneRect {
    if image.scroll_mark > scroll.lines_scrolled {
        // When: scroll_mark is past lines_scrolled, the image was anchored on an alternate screen
        // that is no longer shown, so it is not drawn.
        return PaneRect::new(0.0, 0.0, 0.0, 0.0);
    }
    let (cell_w, cell_h) = cell_size;
    let scrolled = scroll.lines_scrolled - image.scroll_mark;
    let row = f64::from(image.row) - scrolled as f64 + scroll.rows_back as f64;
    let (width, height) = match image.kitty {
        Some(tag) => {
            // When: the image is a kitty placement, its box and transmitted size decide its size.
            let KittyShow::Anchored { cols, rows } = tag.show else {
                // When: the tag is not an anchored show, draws_at_anchor already excluded it.
                return PaneRect::new(0.0, 0.0, 0.0, 0.0);
            };
            // The transmitted size, before any downscale for retention, is the size kitty draws.
            let (source_w, source_h) = match tag.source_px {
                (0, _) | (_, 0) => (image.width, image.height),
                size => size,
            };
            anchored_size_px(cols, rows, source_w, source_h, cell_w, cell_h)
        }
        None => (image.width as f32, image.height as f32),
    };
    PaneRect::new(
        origin.0 + f32::from(image.col) * cell_w,
        origin.1 + (row * f64::from(cell_h)) as f32,
        width,
        height,
    )
}

/// Append one pane's image placements to `out`, in painter order: images drawn at their anchor
/// first, then one placement per placeholder run in `rows`.
///
/// Placeholder runs are only looked for when the pane holds a virtual placement, so a pane without
/// kitty images never scans its rows.
pub(super) fn push_pane_placements<'img, 'grid>(
    out: &mut Vec<InlineImagePlacement<'img>>,
    images: &'img [InlineImage],
    rows: impl Iterator<Item = (u16, &'grid sonicterm_render_model::boundary::grid::line::Line)>,
    origin: (f32, f32),
    content_clip: PaneRect,
    cell_size: (f32, f32),
    scroll: AnchorScroll,
) {
    for image in images.iter().filter(|image| image.draws_at_anchor()) {
        let dest = anchored_dest(image, origin, cell_size, scroll);
        out.push(InlineImagePlacement {
            image,
            origin_x: origin.0,
            origin_y: origin.1,
            content_clip,
            painter_order: out.len(),
            slice: Some(PlaceholderSlice { dest, source_uv: [0.0, 0.0, 1.0, 1.0] }),
        });
    }
    if !has_virtual_placement(images) {
        // When: has_virtual_placement is false, no row can hold a drawable placeholder, so none is scanned.
        return;
    }
    for (image, slice) in slices_for_rows(rows, images, origin, cell_size) {
        out.push(InlineImagePlacement {
            image,
            origin_x: origin.0,
            origin_y: origin.1,
            content_clip,
            painter_order: out.len(),
            slice: Some(slice),
        });
    }
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
            let Some((image, tag)) = find_virtual(images, run.image_id, run.placement_id) else {
                // When: find_virtual matches no placement, the run names a deleted or unknown image and draws nothing.
                continue;
            };
            let KittyShow::Virtual { cols, rows } = tag.show else {
                // When: tag.show is not Virtual, find_virtual broke its contract; draw nothing rather than guess.
                continue;
            };
            // The box and fit use the transmitted size, so a downscaled image keeps its natural size.
            let (source_w, source_h) = tag.source_px;
            let (box_cols, box_rows) = placement_box(
                (cols != 0).then_some(cols),
                (rows != 0).then_some(rows),
                source_w,
                source_h,
                cell_w,
                cell_h,
            );
            let natural = cols == 0 && rows == 0;
            let fit = fit_in_box(box_cols, box_rows, source_w, source_h, cell_w, cell_h, natural);
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

/// The identity of every pane's inline media, for the unchanged-frame check.
///
/// Covers each entry's pixel id, anchor, size and kitty tag, so a placement-only change — a new
/// box, a deleted placement — is never skipped as unchanged. Placeholder cell changes are covered
/// by the grid's own identities, so no row is read here.
pub(super) fn inline_media_hash<'img>(
    panes: impl Iterator<Item = (u64, &'img [InlineImage])>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for (pane_id, images) in panes {
        pane_id.hash(&mut hash);
        images.len().hash(&mut hash);
        for image in images {
            (image.id, image.row, image.col, image.scroll_mark, image.width, image.height)
                .hash(&mut hash);
            image.kitty.hash(&mut hash);
        }
    }
    hash.finish()
}

#[cfg(test)]
#[path = "core_kitty_placeholders_tests.rs"]
mod core_kitty_placeholders_tests;
