//! Per-row glyph cache layered on top of the dirty-bitset foundation
//! landed in PR #130.
//!
//! The terminal renderer (see `render.rs::render`) walks every visible
//! row, groups cells into style runs, and feeds each run through
//! `flush_shape_run` which routes glyphs through the swash rasterizer +
//! `GlyphAtlas`, appending one `GlyphInstance` per cell into a
//! frame-local `Vec`. On a typical idle frame the same row content is
//! re-shaped over and over: a 60-line tmux pane that only changes its
//! clock cell still re-shapes the other 59 rows on every redraw, which
//! is the wall-clock cost the profiler kept attributing to "atlas
//! lookup + push_glyph".
//!
//! This cache memoises the per-row output (glyph instances, underline
//! coalescing, missing-tofu list) keyed on a hash of every input that
//! affects what those instances should be: absolute row position,
//! cells, style-run flags, cell dimensions, scale factor, and selection
//! overlap. When the next frame asks for the same row with the same
//! key, we splice the cached `Vec` straight into the frame buffer and
//! skip the shaping pass.
//!
//! Cursor movement does NOT need to be folded into the hash: the cursor
//! is drawn as a quad (`render::render` builds it from the cursor
//! position after the text pass, not from cached glyphs), and the row
//! it moves out of is marked dirty by Grid's existing
//! `mark_all_dirty()` hook in PR #130, so its cache entry is dropped
//! before it is reused.
//!
//! Selection IS folded in because selection inverts fg/bg per cell,
//! changing the colour we hand to the glyph atlas. The hash includes
//! the selection bbox whenever any of its rows overlap row `r`. A
//! coarser approach — "invalidate the entire cache on any selection
//! change" — was rejected because click-drag updates the selection on
//! every mouse-move sample, which would mean a full re-shape of the
//! whole viewport ~60×/sec while dragging.
//!
//! Atlas churn is handled with a single "invalidate everything" hook
//! (`invalidate_all`) that the renderer fires whenever the atlas is
//! re-allocated (font / theme / scale change rebuilds `GlyphAtlas`,
//! and the LRU `glyph_atlas::evict` can land cache entries with stale
//! UV coordinates). Bounding the cache by visible row count keeps the
//! memory cost trivial.

use crate::text_pipeline::GlyphInstance;
use glyphon::Color as GColor;
use sonic_core::grid::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

const CACHE_HEADROOM_FACTOR: usize = 4;

/// One cached row's render artefacts. Replayed verbatim into the
/// frame's glyph_instances / underlines / missing_chars vectors when
/// the cache hits.
#[derive(Clone, Default, Debug)]
pub struct CachedRow {
    pub glyphs: Vec<GlyphInstance>,
    /// Underline runs for this row, stored as `(start_col, end_col)` —
    /// the row index is implied by the key.
    pub underlines: Vec<(u16, u16)>,
    /// Missing-glyph tofu quads for this row.
    /// Tuple: `(x, y, w, h, color)`.
    pub tofu: Vec<(f32, f32, f32, f32, GColor)>,
    /// Codepoints that were missing this row — published into
    /// `last_missing_chars` so the unicode-e2e gate stays meaningful.
    pub missing_chars: Vec<char>,
}

/// Per-row glyph cache. Keys are `(view_top_abs + r, hash)` — a row's
/// cached output is only valid if the renderer is currently looking at
/// the same absolute row AND that row's content / styling / selection
/// overlap is unchanged.
#[derive(Default, Debug)]
pub struct RowGlyphCache {
    /// (abs_row, hash) -> cached artefacts.
    entries: HashMap<(u64, u64), CachedRow>,
    /// Soft cap so that long-running sessions with heavy scrollback
    /// don't grow without bound. The renderer calls `resize(grid.rows)`
    /// each frame; we keep ~4× headroom for scroll jiggle and call it
    /// good.
    cap: usize,
}

impl RowGlyphCache {
    pub fn new() -> Self {
        Self { entries: HashMap::new(), cap: 0 }
    }

    /// Resize the cache to match the current visible grid height. Cheap
    /// no-op unless the row count changes; when it does, this updates the
    /// soft cap and drops stale entries from the old viewport geometry.
    #[inline]
    pub fn resize(&mut self, rows: u16) {
        let new_cap = usize::from(rows).saturating_mul(CACHE_HEADROOM_FACTOR).max(1);
        if self.cap != new_cap {
            self.cap = new_cap;
            self.entries.clear();
        }
    }

    /// Drop every cache entry. Called on font / theme / scale / resize
    /// / atlas-rebuild events — anything that invalidates UVs or
    /// colours across the whole grid.
    #[inline]
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    /// Drop the cache entry for absolute row `abs_row` regardless of
    /// hash. Called per-dirty-row from the renderer using
    /// `grid.dirty_rows()`. Cheap: visible rows ≤ a few hundred.
    #[inline]
    pub fn invalidate_row_abs(&mut self, abs_row: u64) {
        self.entries.retain(|(r, _), _| *r != abs_row);
    }

    /// Number of cached rows. Useful for tests and tracing.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a cached row by key. None on miss.
    #[inline]
    pub fn get(&self, abs_row: u64, hash: u64) -> Option<&CachedRow> {
        self.entries.get(&(abs_row, hash))
    }

    /// Insert (or replace) a cached row. If the cache is at capacity,
    /// the entire cache is cleared first — terminal rows are
    /// homogeneous so an LRU buys little here and HashMap doesn't carry
    /// insertion order natively. The cap tracks the visible grid height
    /// via `resize` so this only fires after a long scroll session.
    pub fn insert(&mut self, abs_row: u64, hash: u64, row: CachedRow) {
        if self.entries.len() >= self.cap {
            self.entries.clear();
        }
        self.entries.insert((abs_row, hash), row);
    }
}

/// Compute the cache key for a row.
///
/// Inputs folded in:
/// * `view_top_abs + r` — moving the viewport reuses cached rows
///   whose absolute position matches.
/// * row cell contents (Cell already derives Hash).
/// * `style_rev` — opaque counter bumped on theme / palette / default
///   fg/bg changes; lets the renderer invalidate without iterating.
/// * `cell_w`, `cell_h`, `scale_factor` — geometry changes redraw
///   every cell at a new physical position.
/// * `selection` bbox — but only when it overlaps row `r`. A
///   selection outside this row's range doesn't change its rendering.
#[allow(clippy::too_many_arguments)]
pub fn row_hash(
    view_top_abs: u64,
    r: usize,
    row_cells: &[Cell],
    style_rev: u64,
    cell_w: f32,
    cell_h: f32,
    scale_factor: f32,
    selection: Option<(u16, u16, u16, u16)>,
) -> u64 {
    let mut h = DefaultHasher::new();
    (view_top_abs + r as u64).hash(&mut h);
    row_cells.hash(&mut h);
    style_rev.hash(&mut h);
    cell_w.to_bits().hash(&mut h);
    cell_h.to_bits().hash(&mut h);
    scale_factor.to_bits().hash(&mut h);
    if let Some((s_row, s_col, e_row, e_col)) = selection {
        // Normalise so (start, end) order doesn't perturb the hash.
        let (lo, hi) = if (s_row, s_col) <= (e_row, e_col) {
            ((s_row, s_col), (e_row, e_col))
        } else {
            ((e_row, e_col), (s_row, s_col))
        };
        // Only fold the bbox in if it overlaps row `r`. A selection
        // that doesn't touch this row has no effect on its glyphs, so
        // including it would needlessly invalidate cache entries
        // every time the user clicks elsewhere.
        let r16 = r as u16;
        if r16 >= lo.0 && r16 <= hi.0 {
            0x5E1E_C7104_u64.hash(&mut h);
            lo.0.hash(&mut h);
            lo.1.hash(&mut h);
            hi.0.hash(&mut h);
            hi.1.hash(&mut h);
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonic_core::grid::Cell;

    fn cells(n: usize) -> Vec<Cell> {
        (0..n)
            .map(|i| Cell {
                ch: char::from_u32(b'a' as u32 + i as u32).unwrap(),
                ..Cell::default()
            })
            .collect()
    }

    #[test]
    fn hash_stable_for_identical_inputs() {
        let c = cells(4);
        let a = row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None);
        let b = row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_with_cells() {
        let c1 = cells(4);
        let mut c2 = c1.clone();
        c2[0].ch = 'Z';
        assert_ne!(
            row_hash(0, 0, &c1, 0, 8.0, 16.0, 2.0, None),
            row_hash(0, 0, &c2, 0, 8.0, 16.0, 2.0, None),
        );
    }

    #[test]
    fn hash_changes_with_style_rev() {
        let c = cells(4);
        assert_ne!(
            row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
            row_hash(0, 0, &c, 1, 8.0, 16.0, 2.0, None),
        );
    }

    #[test]
    fn hash_changes_with_geometry() {
        let c = cells(4);
        assert_ne!(
            row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
            row_hash(0, 0, &c, 0, 9.0, 16.0, 2.0, None),
        );
        assert_ne!(
            row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
            row_hash(0, 0, &c, 0, 8.0, 16.0, 1.0, None),
        );
    }

    #[test]
    fn hash_changes_when_selection_overlaps_row() {
        let c = cells(4);
        let none = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, None);
        let overlap = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, Some((1, 0, 3, 4)));
        assert_ne!(none, overlap);
    }

    #[test]
    fn hash_ignores_selection_that_does_not_overlap_row() {
        let c = cells(4);
        let none = row_hash(0, 8, &c, 0, 8.0, 16.0, 2.0, None);
        let elsewhere = row_hash(0, 8, &c, 0, 8.0, 16.0, 2.0, Some((1, 0, 3, 4)));
        assert_eq!(none, elsewhere);
    }

    #[test]
    fn selection_endpoints_normalised() {
        let c = cells(4);
        let a = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, Some((1, 0, 3, 4)));
        let b = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, Some((3, 4, 1, 0)));
        assert_eq!(a, b);
    }

    #[test]
    fn abs_row_in_hash() {
        let c = cells(4);
        assert_ne!(
            row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
            row_hash(10, 0, &c, 0, 8.0, 16.0, 2.0, None),
        );
    }

    #[test]
    fn cache_get_insert_invalidate() {
        let mut cache = RowGlyphCache::new();
        let row = CachedRow::default();
        cache.insert(5, 0xabcd, row.clone());
        assert!(cache.get(5, 0xabcd).is_some());
        assert!(cache.get(5, 0xdead).is_none());
        cache.invalidate_row_abs(5);
        assert!(cache.get(5, 0xabcd).is_none());
        cache.insert(7, 0xabcd, row);
        assert_eq!(cache.len(), 1);
        cache.invalidate_all();
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_cap_resets_when_full() {
        let mut cache = RowGlyphCache::new();
        cache.resize(1);
        for i in 0..4 {
            cache.insert(i as u64, i as u64, CachedRow::default());
        }
        assert_eq!(cache.len(), 4);
        // Inserting one more should clear and start fresh (per the
        // simple "drop everything at cap" policy documented above).
        cache.insert(99, 99, CachedRow::default());
        assert_eq!(cache.len(), 1);
        assert!(cache.get(99, 99).is_some());
    }

    #[test]
    fn resize_updates_cap_from_visible_rows() {
        let mut cache = RowGlyphCache::new();
        cache.resize(300);
        for i in 0..1100 {
            cache.insert(i, i, CachedRow::default());
        }
        assert_eq!(cache.len(), 1100);
    }
}
