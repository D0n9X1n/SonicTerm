//! Per-row ink records: where each presented terminal row's primitives actually drew.
//!
//! A partial frame redraws only the rows whose ink can reach its damage. A glyph is drawn at its
//! natural size with no pane clip, so a row's padded strip does not bound it; this table keeps the
//! outward-rounded union of what each row last presented, per `(pane, slot)`, and the content it
//! drew it from, so a row whose content has since changed is never trusted.

use sonicterm_render_model::boundary::grid::grid::Grid;
use sonicterm_render_model::PixelRect;
use sonicterm_types::{retained_hash_table_bytes, ResourceAmount};

/// A record's key: the pane id and the viewport slot the row was drawn at.
pub(crate) type RowInkKey = (u64, u16);

/// What one presented row drew, and the content it drew it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowInk {
    /// The union of the row's glyphs, background and decoration quads; empty when it drew nothing.
    pub rect: PixelRect,
    /// The absolute row the slot showed.
    pub abs_row: u64,
    /// That row's content stamp when presented; `None` when the grid no longer held the row.
    pub content_seq: Option<u64>,
}

/// Below this capacity a table or staging buffer is never shrunk.
const SHRINK_FLOOR: usize = 64;

/// Committed records per `(pane, slot)` and one frame's staged records.
///
/// Records are staged during assembly and committed only when the frame presents, so they always
/// describe the pixels on screen. A row a frame did not emit keeps its record, as it keeps its
/// pixels.
#[derive(Debug, Default)]
pub(crate) struct RowInkTable {
    committed: std::collections::HashMap<RowInkKey, RowInk>,
    staged: Vec<(RowInkKey, RowInk)>,
}

impl RowInkTable {
    /// Discard whatever an unpresented frame staged, before a new frame stages its rows; the
    /// allocation is kept for the new frame.
    pub(crate) fn begin_frame(&mut self) {
        self.staged.clear();
    }

    /// Stage the one record of the row at `slot` of `pane_id`, emitted this frame, and return its
    /// staging index; the row's later primitives merge into it through [`Self::merge_staged`].
    pub(crate) fn stage(&mut self, pane_id: u64, slot: u16, ink: RowInk) -> usize {
        self.staged.push(((pane_id, slot), ink));
        self.staged.len() - 1
    }

    /// Union `rect` into the staged record at `index`; an empty rectangle adds no area.
    pub(crate) fn merge_staged(&mut self, index: usize, rect: PixelRect) {
        if let Some((_, ink)) = self.staged.get_mut(index) {
            ink.rect = union_non_empty(ink.rect, rect);
        }
    }

    /// The staging index of `slot` within `range`, the indices one pane's rows were staged at in
    /// ascending slot order.
    pub(crate) fn staged_index(&self, range: std::ops::Range<usize>, slot: u16) -> Option<usize> {
        let start = range.start;
        self.staged
            .get(range)?
            .binary_search_by_key(&slot, |((_, staged_slot), _)| *staged_slot)
            .ok()
            .map(|offset| start + offset)
    }

    /// Records staged so far this frame.
    pub(crate) fn staged_len(&self) -> usize {
        self.staged.len()
    }

    /// The committed ink of `slot` of `pane_id`, if it still describes that slot's content: the
    /// slot shows the same absolute row, and that row's content stamp is the one presented.
    pub(crate) fn valid_rect(
        &self,
        pane_id: u64,
        slot: u16,
        abs_row: u64,
        content_seq: Option<u64>,
    ) -> Option<PixelRect> {
        self.committed
            .get(&(pane_id, slot))
            .filter(|ink| ink.abs_row == abs_row && ink.content_seq == content_seq)
            .map(|ink| ink.rect)
    }

    /// Commit a presented frame's stages, replacing each emitted slot's record, then prune every
    /// record whose pane is not in `surviving` (`(pane_id, row_count)`) or whose slot is at or past
    /// that pane's row count, and release capacity the table no longer needs. The staging buffer
    /// keeps its allocation for the next frame unless it exceeds four times the committed rows.
    pub(crate) fn commit(&mut self, surviving: &[(u64, u16)]) {
        // Each slot was staged once, so its record replaces the committed one.
        for (key, ink) in self.staged.drain(..) {
            self.committed.insert(key, ink);
        }
        self.committed.retain(|(pane_id, slot), _| {
            surviving.iter().any(|(survivor, rows)| survivor == pane_id && slot < rows)
        });
        let len = self.committed.len();
        if shrink_target(len, self.committed.capacity()).is_some() {
            self.committed.shrink_to((2 * len).max(SHRINK_FLOOR));
        }
        // The committed rows bound what a frame can stage; a narrow frame staging few rows does
        // not shrink the buffer the next full frame would regrow.
        if let Some(target) = shrink_target(len, self.staged.capacity()) {
            self.staged.shrink_to(target);
        }
    }

    /// Stage the one record of the row at `slot` of a view whose top is `view_top_abs`, with the
    /// absolute row it showed and that row's content stamp in `grid`; returns its staging index.
    pub(crate) fn stage_row(
        &mut self,
        pane_id: u64,
        slot: u16,
        grid: &Grid,
        view_top_abs: u64,
        rect: PixelRect,
    ) -> usize {
        let abs_row = view_top_abs.saturating_add(u64::from(slot));
        let content_seq = grid.row_content_seq_at_abs(abs_row);
        self.stage(pane_id, slot, RowInk { rect, abs_row, content_seq })
    }

    /// Per slot of a `rows`-row view whose top is `view_top_abs`, the committed record of
    /// `pane_id` that still describes the slot's content in `grid`; `None` when missing or stale.
    pub(crate) fn valid_records(
        &self,
        pane_id: u64,
        grid: &Grid,
        view_top_abs: u64,
        rows: u16,
    ) -> Vec<Option<PixelRect>> {
        (0..rows)
            .map(|slot| {
                let abs_row = view_top_abs.saturating_add(u64::from(slot));
                self.valid_rect(pane_id, slot, abs_row, grid.row_content_seq_at_abs(abs_row))
            })
            .collect()
    }

    /// The committed record of `slot` of `pane_id` as it stands, whatever content it describes.
    pub(crate) fn committed_rect(&self, pane_id: u64, slot: u16) -> Option<PixelRect> {
        self.committed.get(&(pane_id, slot)).map(|ink| ink.rect)
    }

    /// Drop every record of a pane that was closed.
    pub(crate) fn drop_pane(&mut self, pane_id: u64) {
        self.committed.retain(|(owner, _), _| *owner != pane_id);
        self.staged.retain(|((owner, _), _)| *owner != pane_id);
    }

    /// Committed records.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.committed.len()
    }

    /// The committed table's usable capacity.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.committed.capacity()
    }

    /// The staging buffer's capacity, in entries.
    #[cfg(test)]
    pub(crate) fn staged_capacity(&self) -> usize {
        self.staged.capacity()
    }

    /// The table's allocated buckets, control bytes and trailing group, plus the staging buffer;
    /// items are committed records.
    pub(crate) fn retained_amount(&self) -> ResourceAmount {
        let entry = std::mem::size_of::<(RowInkKey, RowInk)>();
        ResourceAmount {
            bytes: retained_hash_table_bytes::<RowInkKey, RowInk>(self.committed.capacity())
                .saturating_add(self.staged.capacity().saturating_mul(entry)),
            items: self.committed.len(),
        }
    }
}

/// The capacity to shrink to when `len` fills under a quarter of a `capacity` above the floor.
fn shrink_target(len: usize, capacity: usize) -> Option<usize> {
    (capacity > SHRINK_FLOOR && len < capacity / 4).then(|| (2 * len).max(SHRINK_FLOOR))
}

/// The union of two rectangles, where an empty one adds no area.
pub(crate) fn union_non_empty(left: PixelRect, right: PixelRect) -> PixelRect {
    if right.is_empty() {
        left
    } else if left.is_empty() {
        // When: `left` is empty, it contributes no area and its origin is meaningless.
        right
    } else {
        // When: neither `left` nor `right` is_empty, the record grows to their bounding rectangle.
        left.union(right)
    }
}

/// The ink one emitted row's glyph loop drew: its glyph spans' ink and its tofu outlines, as
/// `(left, top, width, height)` in surface pixels.
pub(crate) fn emitted_row_ink(
    spans: &[crate::cursor::RowGlyphSpan],
    tofu: impl IntoIterator<Item = (f32, f32, f32, f32)>,
) -> InkEdges {
    let mut ink = InkEdges::default();
    for [left, top, right, bottom] in spans.iter().filter_map(|span| span.ink_px) {
        ink.add_px((left, top, right - left, bottom - top));
    }
    for rect in tofu {
        ink.add_px(rect);
    }
    ink
}

/// The union of a row's primitive rectangles in surface pixels, kept as floats until committed.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InkEdges {
    /// `[left, top, right, bottom]` over every finite rectangle with area.
    edges: Option<[f32; 4]>,
    /// A non-finite rectangle was added, so the row's ink cannot be bounded.
    unbounded: bool,
}

impl InkEdges {
    /// Add `(left, top, width, height)` in surface pixels.
    pub(crate) fn add_px(&mut self, (left, top, width, height): (f32, f32, f32, f32)) {
        if !(left.is_finite() && top.is_finite() && width.is_finite() && height.is_finite()) {
            // When: `is_finite` fails for `left`, `top`, `width` or `height`, the row is unbounded.
            self.unbounded = true;
            return;
        }
        if width <= 0.0 || height <= 0.0 {
            // When: `width` or `height` is not positive, the rectangle draws nothing.
            return;
        }
        let added = [left, top, left + width, top + height];
        self.edges = Some(self.edges.map_or(added, |acc| {
            [acc[0].min(added[0]), acc[1].min(added[1]), acc[2].max(added[2]), acc[3].max(added[3])]
        }));
    }

    /// The outward-rounded rectangle; the whole `surface` when unbounded, empty when nothing drew.
    pub(crate) fn to_rect(self, surface: PixelRect) -> PixelRect {
        if self.unbounded {
            // When: `unbounded` is set, the surface bounds every pixel a non-finite primitive touches.
            return surface;
        }
        self.edges.map_or(PixelRect { x: 0, y: 0, w: 0, h: 0 }, |[left, top, right, bottom]| {
            crate::cursor::outward_rect((left, top, right - left, bottom - top))
        })
    }
}

#[cfg(test)]
#[path = "row_ink_tests.rs"]
mod row_ink_tests;
