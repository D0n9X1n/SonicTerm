//! Partial-frame row selection and revision-qualified acknowledgements.

use super::*;

impl PlannedPane {
    /// Iterate resolved viewport slots without retaining copied rows or hidden-history entries.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = (u16, u64)> + '_ {
        (0..self.row_count).map(|slot| (slot, self.view_top_abs.saturating_add(u64::from(slot))))
    }
}

/// The absolute row at the top of a pane's view: its requested top, clamped to the live top.
pub(crate) fn resolved_view_top(viewport_top_abs: Option<u64>, scrollback_len: u64) -> u64 {
    viewport_top_abs.unwrap_or(scrollback_len).min(scrollback_len)
}

/// Map a grid dirty row, a live-buffer index, to the viewport slot that draws it.
///
/// Live row `live_row` is absolute row `scrollback_len + live_row`; a view whose top is
/// `view_top_abs` draws it at slot `scrollback_len + live_row - view_top_abs` when that is
/// below `rows`, and draws it nowhere otherwise. The view top is clamped to the live top, as
/// [`FramePlan::build`] clamps it, so an unscrolled view maps every live row to itself.
pub(crate) fn live_row_to_slot(
    scrollback_len: u64,
    view_top_abs: u64,
    rows: u16,
    live_row: usize,
) -> Option<u16> {
    let view_top_abs = view_top_abs.min(scrollback_len);
    let row_abs = scrollback_len.checked_add(u64::try_from(live_row).ok()?)?;
    let slot = row_abs.checked_sub(view_top_abs)?;
    u16::try_from(slot).ok().filter(|slot| *slot < rows)
}

impl FramePlan {
    /// Widen a presenting plan's damage by the cursor recolors: whenever the record assembly
    /// produced (`current`) differs from the last presented one (`previous`), both bounds are
    /// damaged, so a replaced, removed or new recolored glyph is repainted; with the cursor or
    /// focus class set, the current bounds are damaged even when the record is unchanged.
    /// `Unbounded` damages the active pane. A partial plan's widened damage can reach a row it did
    /// not emit; [`Self::partial_reaches_unemitted_ink`] reports that, and assembly falls back.
    pub(crate) fn widen_for_recolor(&mut self, previous: RecolorRecord, current: RecolorRecord) {
        if self.mode == RenderMode::Noop {
            // When: `mode` is Noop, the plan presents nothing and no damage is read.
            return;
        }
        let changed = previous != current;
        let class = self.change.cursor || self.change.focus;
        let mut records = Vec::with_capacity(2);
        if changed {
            records.push(previous.bounds);
        }
        if changed || class {
            records.push(current.bounds);
        }
        let active_clip = self.panes.get(self.active_index).and_then(|pane| pane.full_clip);
        for bounds in records {
            let Some(rect) = resolve_recolor(bounds, active_clip, self.surface) else {
                // When: the bounds are empty, nothing was recolored on that side.
                continue;
            };
            self.add_damage(rect);
        }
    }
}

impl FramePlan {
    /// Widen a full plan's damage by the tab-title ink the last presented frame and this frame
    /// drew. A title glyph can reach above the padded tab band, so whenever the tab-band or
    /// focus class is set, or the ink changed, both sides' ink is repainted. `Unbounded` ink
    /// damages the whole surface. As with the recolor, a partial plan checks the widened damage
    /// against the rows it did not emit.
    pub(crate) fn widen_for_tab_ink(&mut self, previous: RecolorBounds, current: RecolorBounds) {
        if self.mode == RenderMode::Noop {
            // When: `mode` is Noop, the plan presents nothing and no damage is read.
            return;
        }
        if previous == current && !self.change.tab_band && !self.change.focus {
            // When: the ink is unchanged and no tab-band or focus class is set, it is on screen.
            return;
        }
        for bounds in [previous, current] {
            if let Some(rect) = resolve_recolor(bounds, Some(self.surface), self.surface) {
                self.add_damage(rect);
            }
        }
    }
}

impl FramePlan {
    /// Union `rect`, clipped to the surface, into the damage and record it as a part.
    pub(super) fn add_damage(&mut self, rect: PixelRect) {
        if let Some(clipped) = rect.intersect(self.surface) {
            self.damage = if self.damage.is_empty() {
                clipped
            } else {
                // When: damage is non-empty, the single rectangle grows to the union.
                self.damage.union(clipped)
            };
            self.damage_parts.push(clipped);
        }
    }
}

impl FramePlan {
    /// The rows a presented frame acknowledges for the exact planned pane and revision: a `Full`
    /// plan every row, a `Partial` plan each dirty live row whose slot it emitted, a `Noop` none. A
    /// dirty live row below a scrolled view has no slot, so a partial frame keeps its bit.
    pub(crate) fn acknowledged_rows(
        &self,
        index: usize,
        id: u64,
        revision: u64,
    ) -> Option<sonicterm_render_model::AckRows> {
        let pane = self
            .panes
            .get(index)
            .filter(|pane| pane.id == id && pane.expected_revision == revision)?;
        match self.mode {
            RenderMode::Noop => None,
            RenderMode::Full => Some(sonicterm_render_model::AckRows::All),
            RenderMode::Partial => Some(sonicterm_render_model::AckRows::Rows(
                pane.dirty_live_rows
                    .iter()
                    .copied()
                    .filter(|&row| {
                        live_row_slot(
                            pane.is_alt,
                            pane.scrollback_len,
                            pane.view_top_abs,
                            pane.row_count,
                            row,
                        )
                        .is_some_and(|slot| {
                            pane.emit_rows.get(usize::from(slot)).copied().unwrap_or(false)
                        })
                    })
                    .collect(),
            )),
        }
    }
}

impl FramePlan {
    /// `(pane id, row count)` of every planned pane with pixels on the surface: the rows whose ink
    /// records a presented frame keeps.
    pub(crate) fn drawn_row_counts(&self) -> Vec<(u64, u16)> {
        self.panes
            .iter()
            .filter(|pane| pane.full_clip.is_some())
            .map(|pane| (pane.id, pane.row_count))
            .collect()
    }
}

impl FramePlan {
    /// Turn a partial plan into the `Full` plan assembly falls back to: every row emitted and
    /// every pane acknowledged whole. Its damage stays the composed rectangle, as a hardware
    /// `Full` keeps it, and the reassembled frame counts as one full frame. The fallback itself is
    /// counted where assembly consumes it, since the forced pass usually plans `Full` already.
    pub(crate) fn force_full(&mut self) {
        if self.mode != RenderMode::Partial {
            // When: `mode` is not Partial, every row is already emitted or none is presented.
            return;
        }
        self.mode = RenderMode::Full;
        for pane in &mut self.panes {
            pane.emit_rows.fill(true);
        }
        crate::frame_stats::note_full_frame(true);
    }
}

impl FramePlan {
    /// Whether a partial plan's final damage, after the recolor and tab-ink widening, reaches the
    /// committed ink of a row it did not emit. The scissor would then reset pixels that row drew
    /// without redrawing them, so assembly must fall back to `Full`. A missing record counts as
    /// reaching, conservatively.
    pub(crate) fn partial_reaches_unemitted_ink(&self) -> bool {
        if self.mode != RenderMode::Partial {
            // When: `mode` is not Partial, every drawn row was emitted.
            return false;
        }
        self.panes.iter().filter(|pane| pane.full_clip.is_some()).any(|pane| {
            pane.emit_rows.iter().enumerate().any(|(slot, emitted)| {
                !*emitted
                    && pane
                        .row_ink
                        .get(slot)
                        .copied()
                        .flatten()
                        .is_none_or(|record| meets(record, self.damage))
            })
        })
    }
}

/// The viewport slot that draws live row `live_row`: on the alternate screen, which keeps no
/// scrollback, the row itself; otherwise through [`live_row_to_slot`]. `None` when no slot does.
pub(crate) fn live_row_slot(
    is_alt: bool,
    scrollback_len: u64,
    view_top_abs: u64,
    rows: u16,
    live_row: usize,
) -> Option<u16> {
    if is_alt {
        u16::try_from(live_row).ok().filter(|slot| *slot < rows)
    } else {
        // When: `is_alt` is false, a primary view may be scrolled back, so the live row maps through its view top.
        live_row_to_slot(scrollback_len, view_top_abs, rows, live_row)
    }
}

/// Whether two rectangles share a positive pixel area.
pub(super) fn meets(left: PixelRect, right: PixelRect) -> bool {
    left.intersect(right).is_some()
}

/// Whether every row of `pane` a partial frame could leave unemitted has a valid record. Dirty
/// slots are always emitted, and a pane with no pixels on the surface draws no row.
pub(super) fn records_complete(pane: &PlannedPane) -> bool {
    pane.full_clip.is_none()
        || (0..pane.row_count).all(|slot| {
            pane.row_ink.get(usize::from(slot)).is_some_and(Option::is_some)
                || pane.dirty_slots.contains(&slot)
        })
}

/// The rows a partial plan assembles for `pane`: every dirty slot, every slot whose padded strip
/// meets `damage`, and every slot whose valid record meets `damage` or a `reach` rectangle (the
/// drawn cursor cell and the last frame's recolor bounds). A record is not clipped to its pane, so
/// a neighbour's overhanging glyph is assembled. A pane off the surface assembles nothing.
pub(super) fn partial_emit_rows(
    pane: &PlannedPane,
    damage: PixelRect,
    reach: [Option<PixelRect>; 2],
    geometry: DamageGeometry,
) -> Vec<bool> {
    let mut emit = vec![false; usize::from(pane.row_count)];
    if pane.full_clip.is_none() {
        // When: `full_clip` is None, neither row loop draws this pane, so `emit` stays empty of rows.
        return emit;
    }
    for slot in &pane.dirty_slots {
        if let Some(row) = emit.get_mut(usize::from(*slot)) {
            *row = true;
        }
    }
    for slot in 0..pane.row_count {
        let index = usize::from(slot);
        if emit[index] {
            // When: `emit[index]` is set, the dirty slot is assembled whatever its record says.
            continue;
        }
        let strip_meets =
            slot_damage(pane, slot, geometry).is_some_and(|strip| meets(strip, damage));
        let record_meets = pane.row_ink.get(index).copied().flatten().is_some_and(|record| {
            meets(record, damage) || reach.iter().flatten().any(|target| meets(record, *target))
        });
        emit[index] = strip_meets || record_meets;
    }
    emit
}

#[cfg(test)]
#[path = "frame_plan_rows_tests.rs"]
mod frame_plan_rows_tests;
