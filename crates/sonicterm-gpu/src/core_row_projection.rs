//! Row projection responsibilities of the renderer, preserving its single owner.

use super::*;

/// A glyph record of `kind` at `lead_col` from an atlas tile: its region, raster offset and size,
/// `rgba` and its colour and subpixel flags, with no shaping offset and no marker fit.
pub(super) fn tile_record(
    kind: sonicterm_text::row_glyph_cache::RowGlyphKind,
    lead_col: u16,
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> sonicterm_text::row_glyph_cache::RowGlyph {
    sonicterm_text::row_glyph_cache::RowGlyph {
        uv: info.uv,
        color: rgba,
        raster_offset: [info.px_offset[0] as f32, info.px_offset[1] as f32],
        shape_offset: [0.0; 2],
        raster_size: [info.px_size[0] as f32, info.px_size[1] as f32],
        lead_col,
        end_col: lead_col.saturating_add(1),
        kind_and_bits: sonicterm_text::row_glyph_cache::RowGlyphBits {
            kind,
            is_color: info.is_color,
            is_subpixel: info.is_subpixel,
            marker_fit_eligible: false,
            is_wide: false,
            has_extras: false,
            cluster_cells: 1,
        }
        .pack(),
    }
}

/// Record a shaped or fallback glyph's resolved shaping offset and the inputs of its marker fit,
/// decided now from the lead cell and applied only at projection.
pub(super) fn set_shaped_bits(
    record: &mut sonicterm_text::row_glyph_cache::RowGlyph,
    shape_offset: [f32; 2],
    marker_fit_eligible: bool,
    shaped: &sonicterm_text::shape::ShapedGlyph,
    is_wide: bool,
    lead_cell: &Cell,
) {
    let mut bits = record.bits();
    bits.marker_fit_eligible = marker_fit_eligible;
    bits.is_wide = is_wide;
    bits.has_extras = lead_cell.extras().is_some();
    bits.cluster_cells = shaped.cluster_cells.max(1);
    record.kind_and_bits = bits.pack();
    record.shape_offset = shape_offset;
}

/// Test seam: make the next `shape_text_for_frame` call on this thread return `Err` once.
#[cfg(test)]
pub(crate) fn fail_next_shape_for_test() {
    FAIL_NEXT_SHAPE.with(|armed| armed.set(true));
}

/// `shaped`, or an error in its place when the test seam is armed, consuming the seam.
#[cfg(test)]
pub(super) fn inject_shape_failure<T>(shaped: Result<T>) -> Result<T> {
    if FAIL_NEXT_SHAPE.with(|armed| armed.replace(false)) {
        // When: the test seam is armed, this shaping call fails as a real error would.
        return Err(anyhow!("injected shaping failure"));
    }
    shaped
}

/// Production never injects a shaping failure: the shaping result passes through.
#[cfg(not(test))]
#[inline(always)]
pub(super) fn inject_shape_failure<T>(shaped: Result<T>) -> Result<T> {
    shaped
}

/// The cell at `col` in a style run whose columns strictly increase, found by binary search;
/// `None` when the run holds no cell at that column.
pub(super) fn run_cell_at<'cell>(cells: &[(u16, &'cell Cell)], col: u16) -> Option<&'cell Cell> {
    cells.binary_search_by_key(&col, |(cell_col, _)| *cell_col).ok().map(|index| cells[index].1)
}

/// Where one cached row is drawn this frame: its slot, origin, column edges, cell size, baseline,
/// surface and presenter. Everything the content key omits lives here, so one row's records
/// project correctly wherever it is drawn.
#[derive(Clone, Copy)]
pub(crate) struct RowPlacement<'edges> {
    pub(crate) slot: u16,
    pub(crate) origin: (f32, f32),
    pub(crate) cols: u16,
    pub(crate) snapped_cell_x: &'edges [f32],
    pub(crate) cell_size: (f32, f32),
    pub(crate) baseline_y_in_cell: f32,
    pub(crate) surface: (f32, f32),
}

/// Project one glyph record onto the surface at `at`, per kind, in the order the shaping paths
/// built rectangles before records existed: cell origin, raster offset (natural position),
/// shaping offset, marker fit when eligible, device-pixel snap, then NDC. A block fills the
/// columns it spans: on the GPU at the fractional cell height, on the software presenter at the
/// integer target its tile was rasterized for.
pub(crate) fn project_row_glyph(
    glyph: &sonicterm_text::row_glyph_cache::RowGlyph,
    at: &RowPlacement<'_>,
    software_presenter: bool,
) -> GlyphInstance {
    use sonicterm_text::row_glyph_cache::RowGlyphKind;
    let bits = glyph.bits();
    let (cell_w, cell_h) = at.cell_size;
    let (surface_width_px, surface_height_px) = at.surface;
    let top_inset = at.origin.1;
    let cell_left_px = at.snapped_cell_x[usize::from(glyph.lead_col)];
    let cell_top_px = top_inset + f32::from(at.slot) * cell_h;
    let rect = match bits.kind {
        RowGlyphKind::Block => {
            let cell_right = at.snapped_cell_x[usize::from(glyph.end_col)];
            if software_presenter {
                let cell_bottom = top_inset + (f32::from(at.slot) + 1.0) * cell_h;
                software_block_glyph_target_rect(cell_left_px, cell_top_px, cell_right, cell_bottom)
            } else {
                // When: `software_presenter` is false, the block keeps the fractional cell box.
                (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h)
            }
        }
        RowGlyphKind::Natural => {
            let natural = (
                cell_left_px + glyph.raster_offset[0],
                cell_top_px + at.baseline_y_in_cell + glyph.raster_offset[1],
                glyph.raster_size[0],
                glyph.raster_size[1],
            );
            sonicterm_render_model::geometry::snap_to_device_pixels(natural, 1.0)
        }
        RowGlyphKind::Fallback | RowGlyphKind::Shaped => {
            let natural = (
                cell_left_px + glyph.raster_offset[0],
                cell_top_px + at.baseline_y_in_cell + glyph.raster_offset[1],
                glyph.raster_size[0],
                glyph.raster_size[1],
            );
            let positioned =
                positioned_shaped_glyph_rect(natural, glyph.shape_offset[0], glyph.shape_offset[1]);
            let fitted = if bits.marker_fit_eligible {
                // A standalone status marker is fitted inside its own cell.
                let cell_right = at
                    .snapped_cell_x
                    .get(usize::from(glyph.lead_col) + 1)
                    .copied()
                    .unwrap_or(cell_left_px + cell_w);
                fit_status_marker_rect(
                    positioned,
                    (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h),
                )
            } else {
                // When: `marker_fit_eligible` is false, the glyph keeps its natural overhang, as ligature halves must.
                positioned
            };
            sonicterm_render_model::geometry::snap_to_device_pixels(fitted, 1.0)
        }
    };
    GlyphInstance {
        rect: px_to_ndc(rect.0, rect.1, rect.2, rect.3, surface_width_px, surface_height_px),
        uv: glyph.uv,
        color: glyph.color,
        flags: glyph_flags(bits.is_color, bits.is_subpixel),
    }
}

/// Append one cached row to the frame at `at`: underlines at its slot, projected glyphs as the
/// row's own span, tofu boxes and missing characters. The hit path and the miss path both call
/// this, so a replayed row draws exactly what shaping it again would.
pub(crate) fn project_cached_row(
    row: &sonicterm_text::row_glyph_cache::CachedRow,
    at: &RowPlacement<'_>,
    software_presenter: bool,
    frame: GlyphFrame<'_>,
) {
    let (pad, top_inset) = at.origin;
    for run in &row.underlines {
        frame.underlines.push((pad, top_inset, at.cols, at.slot, *run));
    }
    let glyph_base = frame.glyph_instances.len();
    frame
        .glyph_instances
        .extend(row.glyphs.iter().map(|glyph| project_row_glyph(glyph, at, software_presenter)));
    let cell_h = at.cell_size.1;
    for tofu in &row.tofu {
        frame.missing_tofu.push((
            at.snapped_cell_x[usize::from(tofu.lead_col)] + tofu.inset,
            top_inset + f32::from(at.slot) * cell_h + tofu.inset,
            tofu.width,
            tofu.height,
            ChromeColor::from(tofu.color),
        ));
    }
    frame.missing_chars_this_frame.extend_from_slice(&row.missing_chars);
    let (surface_width_px, surface_height_px) = at.surface;
    frame.row_spans.push(RowGlyphSpan::new(
        frame.glyph_instances,
        glyph_base..frame.glyph_instances.len(),
        surface_width_px,
        surface_height_px,
    ));
}

/// Whether every block record of a software-presenter row still rasterizes at its stored size
/// when drawn at `at`. The software target rectangle is integer, so a fractional cell height or
/// column edge can make the same block one pixel taller or wider at another position; such a
/// row misses and is shaped again. The current top, the separately computed bottom and the
/// current snapped column edges are used in that order, as shaping uses them.
pub(crate) fn software_blocks_fit(
    row: &sonicterm_text::row_glyph_cache::CachedRow,
    at: &RowPlacement<'_>,
) -> bool {
    use sonicterm_text::row_glyph_cache::RowGlyphKind;
    let (_, cell_h) = at.cell_size;
    let top_inset = at.origin.1;
    row.glyphs.iter().filter(|glyph| glyph.bits().kind == RowGlyphKind::Block).all(|glyph| {
        let (Some(&left), Some(&right)) = (
            at.snapped_cell_x.get(usize::from(glyph.lead_col)),
            at.snapped_cell_x.get(usize::from(glyph.end_col)),
        ) else {
            // When: `lead_col` or `end_col` is outside `snapped_cell_x`, the record cannot be placed here.
            return false;
        };
        let top = top_inset + f32::from(at.slot) * cell_h;
        let bottom = top_inset + (f32::from(at.slot) + 1.0) * cell_h;
        let (_, _, width, height) = software_block_glyph_target_rect(left, top, right, bottom);
        [width, height] == glyph.raster_size
    })
}

#[cfg(test)]
#[path = "core_row_projection_tests.rs"]
mod core_row_projection_tests;
