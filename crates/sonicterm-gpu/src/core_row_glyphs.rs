//! Row glyphs responsibilities of the renderer, preserving its single owner.

use super::*;

/// The renderer's glyph inputs for one row: atlas, row cache, fonts and shaping configuration.
pub(crate) struct GlyphShaping<'frame> {
    pub(crate) atlas: &'frame mut GlyphAtlas,
    pub(crate) row_cache: &'frame mut sonicterm_text::row_glyph_cache::RowGlyphCache,
    pub(crate) font_stack: Option<&'frame sonicterm_engine::FontStack>,
    pub(crate) wt_raster: Option<&'frame mut sonicterm_engine::FontStack>,
    pub(crate) style_rev: u64,
    pub(crate) theme: &'frame Theme,
    pub(crate) fg_default: ChromeColor,
    pub(crate) raster_px: f32,
    pub(crate) cell_size: (f32, f32),
    pub(crate) surface: (f32, f32),
    pub(crate) baseline_y_in_cell: f32,
    pub(crate) hovered_url_accent: [f32; 4],
    pub(crate) software_presenter: bool,
}

impl GlyphShaping<'_> {
    /// The same inputs for one row of a pane pass, reborrowed so the pass can use them again.
    pub(super) fn reborrow(&mut self) -> GlyphShaping<'_> {
        GlyphShaping {
            atlas: &mut *self.atlas,
            row_cache: &mut *self.row_cache,
            font_stack: self.font_stack,
            wt_raster: self.wt_raster.as_deref_mut(),
            style_rev: self.style_rev,
            theme: self.theme,
            fg_default: self.fg_default,
            raster_px: self.raster_px,
            cell_size: self.cell_size,
            surface: self.surface,
            baseline_y_in_cell: self.baseline_y_in_cell,
            hovered_url_accent: self.hovered_url_accent,
            software_presenter: self.software_presenter,
        }
    }
}

/// Where one glyph row sits: its pane, grid, viewport slot and origin, hover, and the content
/// key its pane pass computed for it before any admission.
pub(crate) struct GlyphRow<'grid> {
    pub(crate) pane_id: sonicterm_text::row_glyph_cache::PaneId,
    pub(crate) grid: &'grid Grid,
    pub(crate) view_top_abs: u64,
    pub(crate) slot: u16,
    pub(crate) origin: (f32, f32),
    pub(crate) snapped_cell_x: &'grid [f32],
    pub(crate) pane_hovered_url: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    /// The row's content key from [`emitted_row_key`]; 0 for a row the grid no longer holds.
    pub(crate) key: u64,
}

/// The frame buffers one glyph row appends to.
pub(crate) struct GlyphFrame<'frame> {
    pub(crate) glyph_instances: &'frame mut Vec<GlyphInstance>,
    pub(crate) underlines:
        &'frame mut Vec<(f32, f32, u16, u16, sonicterm_text::row_glyph_cache::UnderlineRun)>,
    pub(crate) missing_tofu: &'frame mut Vec<(f32, f32, f32, f32, ChromeColor)>,
    pub(crate) missing_chars_this_frame: &'frame mut Vec<char>,
    pub(crate) row_spans: &'frame mut Vec<RowGlyphSpan>,
}

impl GlyphFrame<'_> {
    /// The same buffers for one row, reborrowed so the pass can append the next row too.
    pub(super) fn reborrow(&mut self) -> GlyphFrame<'_> {
        GlyphFrame {
            glyph_instances: &mut *self.glyph_instances,
            underlines: &mut *self.underlines,
            missing_tofu: &mut *self.missing_tofu,
            missing_chars_this_frame: &mut *self.missing_chars_this_frame,
            row_spans: &mut *self.row_spans,
        }
    }
}

/// The content key of the row at `slot` of a view whose top is `view_top_abs`: its cells and
/// every non-positional shaping input, with the row's active hover fragment. Counts the row's
/// cells as hashed. Returns 0 when the grid no longer holds that row.
pub(crate) fn emitted_row_key(
    shaping: &GlyphShaping<'_>,
    grid: &Grid,
    view_top_abs: u64,
    slot: u16,
    pane_hovered_url: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
) -> u64 {
    let Some(row) = grid.row_at_abs(view_top_abs.saturating_add(u64::from(slot))) else {
        // When: the absolute row is outside the scrollback still held, nothing is emitted for it.
        return 0;
    };
    crate::frame_stats::note_row_cells_hashed(|| row.iter().len());
    let inputs = sonicterm_text::row_glyph_cache::RowKeyInputs {
        style_rev: shaping.style_rev,
        cell_w: shaping.cell_size.0,
        cell_h: shaping.cell_size.1,
        baseline_y_in_cell: shaping.baseline_y_in_cell,
        raster_px: shaping.raster_px,
        software_presenter: shaping.software_presenter,
        hover_span: hovered_url_row_key_span(pane_hovered_url, slot),
    };
    shaping.row_cache.content_key(row.iter(), grid.cols, &inputs)
}

/// Append one row's glyphs, underlines and tofu to the frame, replaying them from the row cache
/// when its key and atlas identity match (and, for software blocks, their sizes still hold),
/// else shaping them into records and admitting the row when it is complete. Both paths project
/// through [`project_cached_row`]. Returns whether the row was replayed; a row the grid no longer
/// holds draws nothing and is not a replay.
pub(crate) fn emit_row_glyphs(
    shaping: GlyphShaping<'_>,
    placement: GlyphRow<'_>,
    mut frame: GlyphFrame<'_>,
) -> bool {
    let GlyphShaping {
        atlas,
        row_cache,
        font_stack,
        mut wt_raster,
        theme,
        fg_default,
        cell_size: (cell_w, cell_h),
        surface,
        baseline_y_in_cell,
        hovered_url_accent,
        software_presenter,
        ..
    } = shaping;
    let GlyphRow {
        pane_id,
        grid,
        view_top_abs,
        slot,
        origin,
        snapped_cell_x,
        pane_hovered_url,
        key,
    } = placement;
    let row_abs = view_top_abs.saturating_add(u64::from(slot));
    let Some(row) = grid.row_at_abs(row_abs) else {
        // When: `grid.row_at_abs(row_abs)` is None — that
        // absolute row is outside the scrollback still held.
        return false;
    };
    let at = RowPlacement {
        slot,
        origin,
        cols: grid.cols,
        snapped_cell_x,
        cell_size: (cell_w, cell_h),
        baseline_y_in_cell,
        surface,
    };
    let atlas_identity = row_cache_atlas_identity(atlas);
    let cached_row = row_cache.get(pane_id, key, atlas_identity, |cached| {
        !software_presenter || software_blocks_fit(cached, &at)
    });
    crate::frame_stats::note_row_cache(cached_row.is_some());
    if let Some(cached) = cached_row {
        // When: `cached_row` is Some — the content key and atlas identity match and any software
        // block still fits, so the shaped records are projected at this row's position.
        project_cached_row(cached, &at, software_presenter, frame.reborrow());
        return true;
    }
    // Miss: build position-free records in a row-local buffer, project them, and admit them when
    // complete. Keeping the row's work local is what lets it be cached without rescanning the
    // frame buffers.
    let mut records = sonicterm_text::row_glyph_cache::CachedRow::default();
    let mut ul_start: Option<(u16, UnderlineStyle, Color)> = None;
    let mut last_visible_col: u16 = 0;
    // First pass: per-cell underline coalescing; underlines are a cell-level decoration,
    // independent of shaping.
    for (col, cell) in row.iter().enumerate() {
        if cell.flags.contains(CellFlags::WIDE_CONT) {
            // When: `WIDE_CONT` — the trailing half of a wide
            // glyph, whose decoration belongs to its lead cell.
            continue;
        }
        last_visible_col = col as u16;
        if let Some((style, color)) = underline_key(cell) {
            match ul_start {
                Some((_, active_style, active_color))
                    if active_style == style && active_color == color =>
                {
                    // When: the guard holds — same style and
                    // colour, so the open run simply continues.
                }
                Some((s, active_style, active_color)) => {
                    records.underlines.push(sonicterm_text::row_glyph_cache::UnderlineRun {
                        start_col: s,
                        end_col: (col as u16).saturating_sub(1),
                        style: active_style,
                        color: active_color,
                    });
                    ul_start = Some((col as u16, style, color));
                }
                None => {
                    ul_start = Some((col as u16, style, color));
                }
            }
        } else if let Some((s, style, color)) = ul_start.take() {
            // When: `ul_start.take()` is Some — this cell has no
            // underline, so the open run ends and is emitted.
            records.underlines.push(sonicterm_text::row_glyph_cache::UnderlineRun {
                start_col: s,
                end_col: (col as u16).saturating_sub(1),
                style,
                color,
            });
        }
    }
    if let Some((s, style, color)) = ul_start.take() {
        records.underlines.push(sonicterm_text::row_glyph_cache::UnderlineRun {
            start_col: s,
            end_col: last_visible_col,
            style,
            color,
        });
    }

    // Second pass: group cells into style runs and shape each one. Every run is built, whatever
    // an earlier run reported, and its completeness is then folded into the row's: a failed run
    // keeps the row out of the cache but never stops a later valid run from drawing.
    let row_hovered_url = hovered_url_for_pane_row(pane_hovered_url, pane_id, slot);
    let mut complete = true;
    // Visible cells borrowed from the grid; each style run is a slice of this list.
    let cells = crate::row_runs::visible_cells(row);
    for run in crate::row_runs::row_shape_runs(&cells) {
        let run_complete = GpuRenderer::build_shape_run(
            atlas,
            &mut records,
            slot,
            run.style,
            run.cells,
            theme,
            fg_default,
            cell_w,
            cell_h,
            origin.1,
            snapped_cell_x,
            font_stack,
            wt_raster.as_deref_mut(),
            row_hovered_url,
            hovered_url_accent,
            software_presenter,
        );
        complete &= run_complete;
    }
    project_cached_row(&records, &at, software_presenter, frame.reborrow());
    if complete {
        // Every run shaped and every attempted glyph reached a stable atlas outcome, so the row
        // is admitted; an incomplete row was drawn and is shaped again next time.
        row_cache.insert(pane_id, key, row_cache_atlas_identity(atlas), records);
    }
    false
}

/// One pane's glyph rows for a pass: its grid, plan, origin, column edges and hover.
pub(crate) struct PaneGlyphRows<'pane> {
    pub(crate) pane_id: sonicterm_text::row_glyph_cache::PaneId,
    pub(crate) grid: &'pane Grid,
    pub(crate) planned: &'pane PlannedPane,
    pub(crate) origin: (f32, f32),
    pub(crate) snapped_cell_x: &'pane [f32],
    pub(crate) pane_hovered_url: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
}

/// Where a pane pass records each emitted row's ink and its underline owners.
pub(crate) struct PaneGlyphSinks<'sink> {
    pub(crate) row_ink: &'sink mut crate::row_ink::RowInkTable,
    pub(crate) ink_surface: PixelRect,
    pub(crate) underline_owners: &'sink mut Vec<usize>,
    pub(crate) injected_row_glyph: Option<InjectedRowGlyph>,
    /// Test inspector: each slot is appended where its row is emitted; `None` in production.
    pub(crate) emitted_slots: Option<&'sink mut Vec<u16>>,
    /// Reused buffer for one pane's row keys while its rows are pinned.
    pub(crate) row_keys: &'sink mut Vec<u64>,
    /// Each emitted row's tofu record, staged until the frame presents; `None` in seam tests.
    pub(crate) missing_rows: Option<&'sink mut Vec<crate::missing_glyphs::StagedMissing>>,
    /// The fonts this pass shapes with, tagged on each record.
    pub(crate) fonts: Option<(u64, u64)>,
}

/// Start one glyph assembly pass over `panes`, each a planned pane with its grid's columns:
/// one row-cache pass for the panes the plan draws (it releases panes not drawn or resized,
/// clears every stage and pin list, and tracks new panes) and a fresh ink stage, so records an
/// unpresented frame staged are discarded. Production and the fallback tests both start every
/// pass here, before any pane pins or admits a row.
pub(crate) fn begin_glyph_pass<'plan>(
    row_cache: &mut sonicterm_text::row_glyph_cache::RowGlyphCache,
    row_ink: &mut crate::row_ink::RowInkTable,
    panes: impl IntoIterator<Item = (&'plan PlannedPane, u16)>,
) {
    let drawn: Vec<(sonicterm_text::row_glyph_cache::PaneId, u16, u16)> = panes
        .into_iter()
        .filter(|(planned, _)| planned.full_clip.is_some())
        .map(|(planned, cols)| (planned.id, planned.row_count, cols))
        .collect();
    row_cache.begin_frame(&drawn);
    row_ink.begin_frame();
}

/// Assemble one pane's emitted glyph rows in two phases. The pin phase computes the content key
/// of every row the plan emits, once, and pins those keys with the pane's committed slot keys
/// before any admission, so eviction can never drop a row this pass or the presented frame
/// shows. The emit phase then, in slot order, replays or shapes each row, stages its key for the
/// slot, appends the test row-glyph seam and stages the row's ink record. The cache's
/// `begin_frame` for this pass must already have run.
pub(crate) fn assemble_pane_glyph_rows(
    mut shaping: GlyphShaping<'_>,
    pane: PaneGlyphRows<'_>,
    mut frame: GlyphFrame<'_>,
    mut sinks: PaneGlyphSinks<'_>,
) {
    let PaneGlyphRows { pane_id, grid, planned, origin, snapped_cell_x, pane_hovered_url } = pane;
    let view_top_abs = planned.view_top_abs;
    // Pin phase, into the frame scratch's key buffer, borrowed in place so an unwind leaves it
    // in the scratch.
    let keys: &mut Vec<u64> = &mut *sinks.row_keys;
    keys.clear();
    keys.extend(planned.rows().map(|(slot, _)| {
        if planned.emit_rows[usize::from(slot)] {
            emitted_row_key(&shaping, grid, view_top_abs, slot, pane_hovered_url)
        } else {
            // When: the plan does not emit `slot`, its key is never needed this pass.
            0
        }
    }));
    shaping.row_cache.pin(pane_id, keys);
    // Emit phase.
    for (slot, _) in planned.rows() {
        if !planned.emit_rows[usize::from(slot)] {
            // When: the plan does not emit `slot`, its retained pixels and record stay.
            continue;
        }
        let key = keys[usize::from(slot)];
        let (spans_before, tofu_before, underlines_before) =
            (frame.row_spans.len(), frame.missing_tofu.len(), frame.underlines.len());
        let missing_before = frame.missing_chars_this_frame.len();
        let _replayed = emit_row_glyphs(
            shaping.reborrow(),
            GlyphRow {
                pane_id,
                grid,
                view_top_abs,
                slot,
                origin,
                snapped_cell_x,
                pane_hovered_url,
                key,
            },
            frame.reborrow(),
        );
        if let Some(slots) = sinks.emitted_slots.as_deref_mut() {
            // A test inspector is attached: this slot's row was just emitted.
            slots.push(slot);
        }
        if key != 0 {
            // The row exists, so its key is this slot's staged key until the frame settles.
            shaping.row_cache.stage_slot(pane_id, slot, key);
        }
        if let Some(missing_rows) = sinks.missing_rows.as_deref_mut() {
            // Every emitted slot is staged, so an emptied or vanished row clears its record.
            let drawn = &frame.missing_chars_this_frame[missing_before..];
            let (chars, truncated) = if key == 0 {
                (Vec::new(), false)
            } else {
                // When: key is nonzero, the slot shows a real row whose tofu is recorded.
                crate::missing_glyphs::distinct_capped(drawn)
            };
            missing_rows.push(crate::missing_glyphs::StagedMissing {
                pane_id,
                slot,
                record: crate::missing_glyphs::SlotMissing {
                    key,
                    fonts: sinks.fonts,
                    chars,
                    truncated,
                },
            });
        }
        // The row's ink: its glyphs' union and its tofu outlines. Its underlines join when they
        // are drawn below.
        push_injected_row_glyph(
            shaping.atlas,
            sinks.injected_row_glyph,
            pane_id,
            slot,
            frame.glyph_instances,
            frame.row_spans,
            shaping.surface,
        );
        let ink = crate::row_ink::emitted_row_ink(
            &frame.row_spans[spans_before..],
            frame.missing_tofu[tofu_before..]
                .iter()
                .map(|(left, top, width, height, _)| (*left, *top, *width, *height)),
        );
        let staged = sinks.row_ink.stage_row(
            pane_id,
            slot,
            grid,
            view_top_abs,
            ink.to_rect(sinks.ink_surface),
        );
        sinks.underline_owners.extend((underlines_before..frame.underlines.len()).map(|_| staged));
    }
}

#[cfg(test)]
#[path = "core_row_glyphs_tests.rs"]
mod core_row_glyphs_tests;
