//! Clipped class, pane, scrollbar and cursor damage geometry.

use super::*;

/// Add the damage of every narrow class set in `inputs.change` to `dirt`.
pub(super) fn add_class_damage(dirt: &mut DamageParts, inputs: ClassDamageInputs<'_>) {
    let ClassDamageInputs {
        change,
        previous,
        key,
        panes,
        active_index,
        active_view_top_abs,
        tab_bar_top,
        status_band,
        scale,
        scrollbar_mode,
        previous_recolor,
        geometry,
    } = inputs;
    let surface = geometry.surface;
    if let (true, Some((top, height))) = (change.status_band, status_band) {
        // The strip's glyphs are clipped to its band, so the band is exactly its ink.
        let band_top = top.floor() as i32;
        let band_h =
            ((top + height).ceil() as i64 - i64::from(band_top)).clamp(0, i64::from(u32::MAX));
        dirt.add_clipped(
            PixelRect { x: surface.x, y: band_top, w: surface.w, h: band_h as u32 },
            surface,
        );
    }
    if change.tab_band || change.focus {
        if let Some(top) = tab_bar_top {
            // The bar is pinned to the surface bottom; ink above its top edge is padded in.
            let band_top = (top - geometry.vertical_ink_pad.max(0.0)).floor() as i32;
            let band_h =
                (i64::from(surface.bottom()) - i64::from(band_top)).clamp(0, i64::from(u32::MAX));
            dirt.add_clipped(
                PixelRect { x: surface.x, y: band_top, w: surface.w, h: band_h as u32 },
                surface,
            );
        }
    }
    if change.cursor || change.focus {
        // When: change.cursor or change.focus is set, the cursor's rows, its glide rows and last
        // frame's recolored glyphs repaint.
        for cell in [previous.window.cursor_cell, key.window.cursor_cell].into_iter().flatten() {
            match panes.iter().find(|pane| pane.id == cell.pane_id) {
                Some(pane) => {
                    // The cursor moves no content, so its row is enough on either screen.
                    if let Some(rect) = row_strip(pane, cell.slot, geometry) {
                        dirt.add_clipped(rect, surface);
                    }
                }
                // The cursor's pane is not planned, so its pixels cannot be located.
                None => dirt.add_clipped(surface, surface),
            }
        }
        let active_clip = panes.get(active_index).and_then(|pane| pane.full_clip);
        if let Some(rect) = resolve_recolor(previous_recolor.bounds, active_clip, surface) {
            dirt.add_clipped(rect, surface);
        }
        // A gliding cursor draws between cells: each frame's sampled position damages the rows
        // its drawn cell crosses, so the previous sample is erased and the new one drawn. The
        // landing frame has no sample; its cell is the cursor cell damaged above.
        for (glide, cell) in [
            (previous.window.cursor_glide, previous.window.cursor_cell),
            (key.window.cursor_glide, key.window.cursor_cell),
        ] {
            let Some(glide) = glide else {
                // When: this frame has no glide sample, the cursor drew on its cell, damaged above.
                continue;
            };
            // A glide is sampled only for a drawn cell; an unplanned pane's cell damaged the
            // whole surface above, so only a planned pane adds rows here.
            let pane = cell.and_then(|cell| panes.iter().find(|pane| pane.id == cell.pane_id));
            if let Some(rect) = pane.and_then(|pane| glide_damage(pane, glide, geometry)) {
                dirt.add_clipped(rect, surface);
            }
        }
    }
    if change.selection {
        if let (Some(pane), Some(identity)) = (panes.get(active_index), key.panes.get(active_index))
        {
            for slot in changed_selection_slots(
                previous.window.selection.as_ref(),
                key.window.selection.as_ref(),
                active_view_top_abs,
                identity.rows,
                identity.cols,
            ) {
                if let Some(rect) = slot_damage(pane, slot, geometry) {
                    dirt.add_clipped(rect, surface);
                }
            }
        }
    }
    if change.scrollbar {
        // When: `change.scrollbar` is set, some pane's opacity bucket moved, so its drawn track is repainted.
        for ((before, after), pane) in previous.panes.iter().zip(&key.panes).zip(panes) {
            if before.scrollbar_bucket == after.scrollbar_bucket || pane.full_clip.is_none() {
                // When: the bucket is unchanged or the pane is off the surface, no track is redrawn.
                continue;
            }
            // A bucket-only change keeps every geometry input, so old and new tracks coincide.
            if let Some(track) = pane_scrollbar_geometry(
                pane.chrome,
                pane.row_count,
                pane.scrollback_len + u64::from(pane.row_count),
                pane.view_top_abs,
                scrollbar_mode,
                scale,
            ) {
                let track = track.track_rect;
                let rect = crate::cursor::outward_rect((track.x, track.y, track.w, track.h));
                dirt.add_clipped(rect, surface);
            }
        }
    }
}

/// The ink-padded strip one viewport slot of `pane` draws, on either screen. Class damage that
/// moves no content (the cursor, its glide) and row emission use it; content damage on the
/// alternate screen widens to the whole pane through [`slot_damage`].
pub(super) fn row_strip(
    pane: &PlannedPane,
    slot: u16,
    geometry: DamageGeometry,
) -> Option<PixelRect> {
    dirty_rows_damage_rect_with_ink_pad(
        [usize::from(slot)],
        pane.full_rect,
        pane.layout.x,
        pane.layout.y,
        pane.cols,
        geometry.cell_w,
        geometry.cell_h,
        geometry.vertical_ink_pad,
        geometry.surface.w,
        geometry.surface.h,
    )
}

/// The damage one viewport slot of `pane` occupies: the ink-padded row, or the whole clipped
/// pane on the alternate screen.
pub(super) fn slot_damage(
    pane: &PlannedPane,
    slot: u16,
    geometry: DamageGeometry,
) -> Option<PixelRect> {
    pane_damage_rect_with_ink_pad(
        pane.is_alt,
        [usize::from(slot)],
        pane.full_rect,
        pane.layout.x,
        pane.layout.y,
        pane.cols,
        geometry.cell_w,
        geometry.cell_h,
        geometry.vertical_ink_pad,
        geometry.surface.w,
        geometry.surface.h,
    )
}

/// The ink-padded rows of `pane` that a cursor drawn at the glide sample `glide` crosses: from
/// the row holding its top edge to the row holding its bottom edge, both in whole cells. Every
/// cursor shape is drawn inside that cell box, and each row is damaged at the pane's full width,
/// so the sample's column needs no tracing. Damage past the pane is clipped by its rect.
pub(super) fn glide_damage(
    pane: &PlannedPane,
    glide: crate::cursor_glide::GlideCell,
    geometry: DamageGeometry,
) -> Option<PixelRect> {
    let steps = crate::cursor_glide::GlideCell::at(1, 0, 0).row;
    let first_row = glide.row.div_euclid(steps);
    // The cell box ends one cell below its top; a top on a row edge stops at that row.
    let last_row = (glide.row + steps - 1).div_euclid(steps);
    // A sample stays between two of the pane's cells; the bound only keeps the walk finite, and
    // one row past the last lets a cursor in the bottom padding be damaged, clipped by the pane.
    let rows = first_row.max(0)..=last_row.min(i32::from(pane.row_count));
    dirty_rows_damage_rect_with_ink_pad(
        rows.map(|row| row as usize),
        pane.full_rect,
        pane.layout.x,
        pane.layout.y,
        pane.cols,
        geometry.cell_w,
        geometry.cell_h,
        geometry.vertical_ink_pad,
        geometry.surface.w,
        geometry.surface.h,
    )
}

/// The viewport slots whose selection quads differ between `before` and `after`, computed with
/// the draw's own `selection_quad_rects` in unit cells so each quad's y is its slot.
pub(super) fn changed_selection_slots(
    before: Option<&Selection>,
    after: Option<&Selection>,
    view_top_abs: u64,
    rows: u16,
    cols: u16,
) -> Vec<u16> {
    let quads = |selection: Option<&Selection>| -> Vec<(u16, u32, u32)> {
        selection
            .map(|selection| {
                crate::core::selection_quad_rects(
                    selection,
                    view_top_abs,
                    rows,
                    cols,
                    0.0,
                    0.0,
                    1.0,
                    1.0,
                    &[],
                )
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(left, top, width, _)| (top as u16, left.to_bits(), width.to_bits()))
            .collect()
    };
    let (old_quads, new_quads) = (quads(before), quads(after));
    let mut slots: Vec<u16> = old_quads
        .iter()
        .filter(|quad| !new_quads.contains(quad))
        .chain(new_quads.iter().filter(|quad| !old_quads.contains(quad)))
        .map(|quad| quad.0)
        .collect();
    slots.sort_unstable();
    slots.dedup();
    slots
}

/// Where recolor `bounds` draw: nothing when empty, the rectangle itself, or, when unbounded,
/// the active pane's clip (the whole surface without one).
pub(super) fn resolve_recolor(
    bounds: RecolorBounds,
    active_clip: Option<PixelRect>,
    surface: PixelRect,
) -> Option<PixelRect> {
    match bounds {
        RecolorBounds::Empty => None,
        RecolorBounds::Rect(rect) => Some(rect),
        RecolorBounds::Unbounded => Some(active_clip.unwrap_or(surface)),
    }
}

impl DamageParts {
    /// The union of every part added, if any.
    pub(super) fn rect(&self) -> Option<PixelRect> {
        self.union.rect()
    }
}

/// The scrollbar geometry a pane draws: the track at the right edge of its padded `chrome`,
/// `max(8 * scale, 1)` pixels wide and clamped to the chrome. Drawing and damage both read it.
pub(crate) fn pane_scrollbar_geometry(
    chrome: PaneRect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: ScrollbarMode,
    scale: f32,
) -> Option<sonicterm_render_model::boundary::ui::scrollbar::ScrollbarGeometry> {
    use sonicterm_render_model::boundary::ui::scrollbar;
    // Authored at 8 logical px and scaled with DPI so the bar keeps a constant physical size, min 1px.
    let width_px = (8.0 * scale).max(1.0);
    let rect = scrollbar::Rect::new(chrome.x, chrome.y, chrome.w, chrome.h);
    scrollbar::compute(viewport_rows, total_rows, view_top, rect, mode, width_px)
}

#[cfg(test)]
pub(crate) fn pane_scrollbar_identity<I>(mode: ScrollbarMode, panes: I) -> Vec<(u64, u16)>
where
    I: IntoIterator<Item = (u64, usize, u16, f32)>,
{
    let mut identity: Vec<_> = panes
        .into_iter()
        .map(|(id, scrollback, rows, alpha)| {
            (id, effective_scrollbar_bucket(mode, scrollback as u64, rows, alpha))
        })
        .collect();
    identity.sort_unstable_by_key(|(id, _)| *id);
    identity
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn dirty_rows_damage_rect_with_ink_pad<I>(
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    if cols == 0 || cell_w <= 0.0 || cell_h <= 0.0 || surface_w == 0 || surface_h == 0 {
        // When: cell metrics or `surface_w`/`surface_h` are degenerate, there are no drawable row pixels.
        return None;
    }
    let pane_bounds = pane_rect.intersect(PixelRect { x: 0, y: 0, w: surface_w, h: surface_h })?;
    // Include pane padding because negative glyph bearings can leave ink beyond the first cell.
    let left = (origin_x.floor() as i32).min(pane_bounds.x);
    let right = ((origin_x + f32::from(cols) * cell_w).ceil() as i32).max(pane_bounds.right());
    let row_w = (right - left).max(1) as u32;
    let mut damage = DamageRect::empty();
    for row in dirty_rows {
        let ink_pad = vertical_ink_pad.max(0.0);
        // Floor the start and ceil the next row edge so fractional metrics leave no unpainted seam.
        let top = (origin_y + row as f32 * cell_h - ink_pad).floor() as i32;
        let next_top = (origin_y + (row as f32 + 1.0) * cell_h + ink_pad).ceil() as i32;
        damage.add_clipped(
            PixelRect { x: left, y: top, w: row_w, h: (next_top - top).max(1) as u32 },
            pane_bounds,
        );
    }
    damage.rect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn pane_damage_rect_with_ink_pad<I>(
    is_alt: bool,
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    if is_alt {
        // When: `is_alt` is dirty, moved terminal content requires replacement of the whole clipped pane.
        return dirty_rows.into_iter().next().and_then(|_| {
            pane_rect.intersect(PixelRect { x: 0, y: 0, w: surface_w, h: surface_h })
        });
    }
    dirty_rows_damage_rect_with_ink_pad(
        dirty_rows,
        pane_rect,
        origin_x,
        origin_y,
        cols,
        cell_w,
        cell_h,
        vertical_ink_pad,
        surface_w,
        surface_h,
    )
}

#[cfg(test)]
#[path = "frame_plan_damage_tests.rs"]
mod frame_plan_damage_tests;
