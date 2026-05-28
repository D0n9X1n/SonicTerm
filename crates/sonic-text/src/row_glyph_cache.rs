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
//! **Per-pane keying (PR prerequisite for #199):** the cache is keyed
//! by `(pane_id, abs_row, hash)`. Before this change the key was just
//! `(abs_row, hash)`, which assumed a single grid. Once #199 introduces
//! the per-pane render loop, every pane would otherwise read/write the
//! same slot for any matching `(abs_row, hash)` pair and corrupt each
//! other's glyphs. Folding the pane identifier into the key makes the
//! cache safe for the multi-pane traversal without changing today's
//! single-pane behaviour (callers pass `0` as the placeholder pane id
//! until #199 wires real pane identifiers through).
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

use crate::GlyphInstance;
use cosmic_text::Color as GColor;
use sonic_types::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

const CACHE_HEADROOM_FACTOR: usize = 4;

/// Opaque per-pane identifier used as part of the cache key. Today the
/// renderer only has one pane so callers pass `0`; once the per-pane
/// render loop lands (#199) every pane will pass its own stable id.
pub type PaneId = u64;

/// One cached row's render artefacts. Replayed verbatim into the
/// frame's glyph_instances / underlines / missing_chars vectors when
/// the cache hits.
#[derive(Clone, Default, Debug)]
pub struct CachedRow {
    /// Glyph instances composing the row.
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

/// Per-row glyph cache. Keys are `(pane_id, abs_row, hash)` — a row's
/// cached output is only valid if the renderer is currently looking at
/// the same pane AND the same absolute row AND that row's content /
/// styling / selection overlap is unchanged. Separating panes in the
/// key prevents identical `(abs_row, hash)` pairs from colliding when
/// the per-pane render loop (#199) walks more than one grid in a
/// single frame.
#[derive(Default, Debug)]
pub struct RowGlyphCache {
    /// (pane_id, abs_row, hash) -> cached artefacts.
    entries: HashMap<(PaneId, u64, u64), CachedRow>,
    /// Soft cap so that long-running sessions with heavy scrollback
    /// don't grow without bound. The renderer calls `resize(grid.rows)`
    /// each frame; we keep ~4× headroom for scroll jiggle and call it
    /// good. With multiple panes, callers should call `resize` with the
    /// sum of every pane's visible row count so the cap scales with the
    /// total addressable working set rather than a single pane.
    cap: usize,
}

impl RowGlyphCache {
    /// Construct an empty cache. Call [`resize`](Self::resize) before use.
    pub fn new() -> Self {
        Self { entries: HashMap::new(), cap: 0 }
    }

    /// Resize the cache to match the current visible grid height. Cheap
    /// no-op unless the row count changes; when it does, this updates the
    /// soft cap and drops stale entries from the old viewport geometry.
    ///
    /// For multi-pane callers (#199), pass the sum of every pane's
    /// visible row count.
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

    /// Drop every cache entry belonging to a specific pane. Useful when
    /// a pane is closed or its grid is replaced wholesale; cheaper than
    /// `invalidate_all` because peer panes keep their entries.
    #[inline]
    pub fn invalidate_pane(&mut self, pane_id: PaneId) {
        self.entries.retain(|(p, _, _), _| *p != pane_id);
    }

    /// Drop the cache entry for absolute row `abs_row` in pane
    /// `pane_id` regardless of hash. Called per-dirty-row from the
    /// renderer using `grid.dirty_rows()`. Cheap: visible rows ≤ a few
    /// hundred per pane.
    #[inline]
    pub fn invalidate_row_abs(&mut self, pane_id: PaneId, abs_row: u64) {
        self.entries.retain(|(p, r, _), _| !(*p == pane_id && *r == abs_row));
    }

    /// Number of cached rows. Useful for tests and tracing.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no rows are cached.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a cached row by key. None on miss.
    #[inline]
    pub fn get(&self, pane_id: PaneId, abs_row: u64, hash: u64) -> Option<&CachedRow> {
        self.entries.get(&(pane_id, abs_row, hash))
    }

    /// Insert (or replace) a cached row. If the cache is at capacity,
    /// the entire cache is cleared first — terminal rows are
    /// homogeneous so an LRU buys little here and HashMap doesn't carry
    /// insertion order natively. The cap tracks the visible grid height
    /// via `resize` so this only fires after a long scroll session.
    pub fn insert(&mut self, pane_id: PaneId, abs_row: u64, hash: u64, row: CachedRow) {
        if self.entries.len() >= self.cap {
            self.entries.clear();
        }
        self.entries.insert((pane_id, abs_row, hash), row);
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
///
/// Note: the pane identifier is NOT folded into this hash because it
/// is a separate component of the cache key tuple (see
/// `RowGlyphCache`). Keeping pane separation in the tuple rather than
/// the hash makes per-pane invalidation cheap and avoids spurious
/// re-shaping if two panes happen to share content.
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

// Unit tests live in `tests/row_glyph_cache.rs` and
// `tests/row_glyph_cache_pane_isolation.rs`.
