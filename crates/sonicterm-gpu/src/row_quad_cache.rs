//! Per-row quad cache for background / underline / hyperlink tint
//! quads — Phase P2 of Epic #300 (wezterm-parity perf).
//!
//! Mirrors the shape of `sonicterm_text::row_glyph_cache::RowGlyphCache`
//! (PR #140) but caches the `QuadInstance` slice each row emits for
//! its background fill instead of the glyph instances. Both layers
//! cache for the same reason: dense-cell streams (cat large file,
//! tail -f, htop) re-emit the same per-row geometry every frame even
//! though the row content hasn't changed since the last redraw.
//!
//! Moved from `sonicterm-shared::render::row_quad_cache` in M7e of the
//! workspace refactor — caches `QuadInstance`, so it belongs on the
//! GPU side of the layer split.
//!
//! On a cache hit the renderer can `extend_from_slice` the cached
//! `Vec<QuadInstance>` directly into the frame's quad vector and skip
//! the per-cell run-length-encode + `cell_bg_rgba` lookup loop in
//! `emit_cell_bg_quads_clipped`. For an 80×24 grid that scrapes one of
//! the worst hot paths in the vtebench `dense_cells` micro — see the
//! 300× wezterm gap noted in CLAUDE.md §14.
//!
//! Per-pane keying: a `pane_id` is folded into the cache key so split
//! panes never read each other's slot when they happen to have the
//! same `(abs_row, content_hash)` pair. This matches `RowGlyphCache`'s
//! decision and keeps invalidation pane-local where it matters.
//!
//! What is NOT cached:
//! * Cursor quads — frame-specific position; drawn after the per-row
//!   pass anyway.
//! * Rows that overlap an active text selection — selection painting
//!   adds quads outside this module's per-row scope. The cache key
//!   includes the selection bbox overlap so cache entries are
//!   automatically distinct between "selected" and "not selected"
//!   states for a row; a selection-state change drops the previous
//!   entry naturally without `invalidate_all`.
//! * Search-match / quick-select overlays — same story; emitted into
//!   a separate `quads_overlay` buffer and not part of this cache.

use crate::quad::QuadInstance;
use sonicterm_types::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

const CACHE_HEADROOM_FACTOR: usize = 4;

/// Opaque per-pane identifier (mirrors `sonicterm_text::row_glyph_cache::PaneId`).
pub type PaneId = u64;

/// One cached row's background-quad output. Replayed verbatim into
/// the frame's quad vector when the cache hits.
#[derive(Clone, Default, Debug)]
pub struct CachedRowQuads {
    /// `QuadInstance` records covering this row's coalesced background
    /// runs. Already in NDC; pushed straight into the frame quad vec.
    pub quads: Vec<QuadInstance>,
}

/// Per-row quad cache. Keys are `(pane_id, abs_row, hash)` — a row's
/// cached output is only valid for the same pane AND same absolute row
/// AND same content / styling / geometry / selection-overlap.
#[derive(Default, Debug)]
pub struct LineQuadCache {
    /// (pane_id, abs_row, hash) -> cached quads.
    entries: HashMap<(PaneId, u64, u64), CachedRowQuads>,
    /// Soft cap so long sessions with heavy scrollback don't grow
    /// without bound. Sized to `rows * CACHE_HEADROOM_FACTOR` like
    /// `RowGlyphCache`.
    cap: usize,
}

impl LineQuadCache {
    /// Construct an empty cache. Call [`resize`](Self::resize) before use.
    #[must_use]
    pub fn new() -> Self {
        Self { entries: HashMap::new(), cap: 0 }
    }

    /// Resize to match the current visible grid height (sum across
    /// panes when multiple grids share this cache).
    #[inline]
    pub fn resize(&mut self, rows: u16) {
        let new_cap = usize::from(rows).saturating_mul(CACHE_HEADROOM_FACTOR).max(1);
        if self.cap != new_cap {
            self.cap = new_cap;
            self.entries.clear();
        }
    }

    /// Drop every cache entry. Called on theme / font / scale / resize
    /// — anything that invalidates colors or geometry across the grid.
    #[inline]
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    /// Drop every cache entry belonging to a specific pane.
    #[inline]
    pub fn invalidate_pane(&mut self, pane_id: PaneId) {
        self.entries.retain(|(p, _, _), _| *p != pane_id);
    }

    /// Drop the cache entry for absolute row `abs_row` in pane
    /// `pane_id` regardless of hash. Called per-dirty-row from the
    /// renderer using `grid.dirty_rows()`.
    #[inline]
    pub fn invalidate_row_abs(&mut self, pane_id: PaneId, abs_row: u64) {
        self.entries.retain(|(p, r, _), _| !(*p == pane_id && *r == abs_row));
    }

    /// Number of cached rows. Useful for tests and tracing.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no rows are cached.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a cached row by key. None on miss.
    #[inline]
    #[must_use]
    pub fn get(&self, pane_id: PaneId, abs_row: u64, hash: u64) -> Option<&CachedRowQuads> {
        self.entries.get(&(pane_id, abs_row, hash))
    }

    /// Insert (or replace) a cached row's quads. If the cache is at
    /// capacity, clears wholesale (same strategy as RowGlyphCache).
    pub fn insert(&mut self, pane_id: PaneId, abs_row: u64, hash: u64, cached: CachedRowQuads) {
        if self.entries.len() >= self.cap {
            self.entries.clear();
        }
        self.entries.insert((pane_id, abs_row, hash), cached);
    }
}

/// Compute the cache key for a row's quad output.
///
/// Inputs folded in:
/// * `view_top_abs + r` — absolute row position (so scrollback reuse
///   hits naturally when the viewport moves).
/// * row cell contents (Cell already derives Hash; bg is derived from
///   the cell's color attribute so hashing the cell covers it).
/// * `style_rev` — opaque counter bumped on theme / palette change.
/// * `cell_w`, `cell_h` — geometry change repositions every quad.
/// * `origin_x`, `origin_y` — pane origin (so a re-laid-out pane
///   moving on screen invalidates its rows).
/// * `pane_w`, `pane_h` — pane rect dimensions (clipping affects the
///   last quad on the row when the pane is narrower than the grid).
/// * `selection_overlap` — when the active selection touches row `r`,
///   the renderer must NOT serve a cached entry whose quads predate
///   the selection (selection paint sits on top, but the per-row pass
///   skips its own bg work for those rows to avoid double-paint). A
///   non-overlapping selection has no effect.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn row_quad_hash(
    view_top_abs: u64,
    r: usize,
    row_cells: &[Cell],
    style_rev: u64,
    cell_w: f32,
    cell_h: f32,
    origin_x: f32,
    origin_y: f32,
    pane_w: f32,
    pane_h: f32,
    selection: Option<(u16, u16, u16, u16)>,
    scale_factor: f32,
) -> u64 {
    let mut h = DefaultHasher::new();
    (view_top_abs + r as u64).hash(&mut h);
    row_cells.hash(&mut h);
    style_rev.hash(&mut h);
    cell_w.to_bits().hash(&mut h);
    cell_h.to_bits().hash(&mut h);
    origin_x.to_bits().hash(&mut h);
    origin_y.to_bits().hash(&mut h);
    pane_w.to_bits().hash(&mut h);
    pane_h.to_bits().hash(&mut h);
    // #489 belt-and-suspenders: bg quads now encode snapped positions,
    // and `snap_to_device_pixels` depends on `scale_factor`. Including
    // it in the hash makes a DPI flip invalidate the cache deterministically
    // even if `line_quad_cache.invalidate_all()` is ever skipped on a
    // `rebuild_for_scale()` path.
    scale_factor.to_bits().hash(&mut h);
    if let Some((s_row, s_col, e_row, e_col)) = selection {
        let (lo, hi) = if (s_row, s_col) <= (e_row, e_col) {
            ((s_row, s_col), (e_row, e_col))
        } else {
            ((e_row, e_col), (s_row, s_col))
        };
        let r16 = r as u16;
        if r16 >= lo.0 && r16 <= hi.0 {
            0x71D5_5E1E_u64.hash(&mut h);
            lo.0.hash(&mut h);
            lo.1.hash(&mut h);
            hi.0.hash(&mut h);
            hi.1.hash(&mut h);
        }
    }
    h.finish()
}

// Unit tests live in `crates/sonicterm-shared/tests/line_quad_cache_hit_miss.rs`.
