//! Cell-row → shaped-glyph run pipeline.
//!
//! Bridges between the terminal grid (a 2-D array of styled cells, one
//! codepoint per cell) and the GPU atlas (one tile per *glyph*, where
//! "glyph" means whatever the shaper produced — which may collapse N
//! codepoints into 1, e.g. the `=>` ligature or the 👨‍👩‍👧 ZWJ family).
//!
//! ## Why this exists
//!
//! Before this module the renderer treated each cell as an independent
//! `(char, bold, italic) → glyph` lookup. That's wrong for two cases
//! every modern terminal supports:
//!
//! - **ZWJ sequences** like 👨‍👩‍👧 — three emoji codepoints joined by
//!   U+200D that the font's GSUB table composes into a single family
//!   glyph. Without shaping the user sees a man, a woman, and a girl
//!   in three separate cells. With shaping they see one family glyph
//!   in the lead cell (and the trailing cells render nothing — the
//!   wide glyph already covers them visually).
//! - **Programming ligatures** like `=>`, `!=`, `>=`, `->`, `===` —
//!   contextual substitutions the font's GSUB applies when these
//!   character sequences appear together. JetBrains Mono and Rec Mono
//!   Casual both ship these.
//!
//! ## The hand-off contract
//!
//! Input: a slice of `(col, &Cell)` covering one **style run** — a
//! maximal stretch of consecutive non-WIDE_CONT cells that share
//! `(bold, italic)`. The caller groups cells into style runs because
//! cosmic-text shapes a single `Buffer` at a single (weight, style),
//! so a run is the largest unit we can shape together.
//!
//! Output: a `Vec<ShapedGlyph>` — one per glyph the shaper produced.
//! Each entry knows
//!   - which *lead cell* (column) it sits over (the column of the
//!     first cell whose byte range falls inside the glyph's cluster),
//!   - the resolved `font_slot` (matched back through the
//!     `SwashRasterizer`'s fallback chain so atlas keys don't
//!     collide across faces),
//!   - the `glyph_id` cosmic-text emitted (the shaped id, *not* the
//!     charmap-of-first-codepoint id), and
//!   - swash-ready advance metrics for instance placement.
//!
//! ## Fallback
//!
//! If the font lacks a ZWJ/ligature substitution, cosmic-text emits one
//! component glyph per source codepoint — the output is then 1:1 with
//! cells and the rest of the pipeline behaves exactly as before. The
//! shaper-driven path is therefore safe to enable unconditionally: it
//! is a strict superset of the char-based path.

use cosmic_text::{AttrsList, BufferLine, Ellipsize, Hinting, LineEnding, Shaping, Wrap};
use sonic_types::{Cell, CellFlags};

use crate::swash_rasterizer::SwashRasterizer;
use crate::terminal_font_attrs;

/// Characters that commonly participate in programming ligatures across
/// the fonts Sonic ships (Rec Mono Casual, JetBrains Mono). If a run
/// contains ANY of these, the ASCII fast path must defer to the shaper
/// so contextual GSUB substitutions (`=>`, `!=`, `>=`, `->`, `<-`,
/// `::`, `||`, `&&`, etc.) actually render as the composed ligature
/// glyph instead of two separate cells.
///
/// Kept deliberately small — adding harmless ASCII (digits, letters)
/// here would needlessly defeat the fast path.
#[inline]
fn is_ligature_trigger(b: u8) -> bool {
    matches!(b, b'=' | b'!' | b'<' | b'>' | b'-' | b'_' | b':' | b'|' | b'&' | b'*')
}

/// True when every cell in `cells` is a plain printable-ASCII codepoint
/// (0x20..=0x7E) with no `extras` cluster AND the run contains none of
/// the characters that commonly trigger programming ligatures. The
/// renderer can then bypass cosmic-text entirely and emit one
/// `GlyphKey` per cell via the pre-shaping char→glyph lookup path.
/// ASCII shells (the steady-state for almost every interactive session)
/// hit this hundreds of times per frame; shaping was previously running
/// unconditionally.
///
/// Runs that contain a ligature-trigger byte (`=`, `!`, `<`, `>`, `-`,
/// `_`, `:`, `|`, `&`, `*`) MUST go through the shaper even when
/// otherwise pure ASCII — `=>`, `!=`, `>=`, `->`, `::`, `||`, `&&`,
/// etc. are pure ASCII and would otherwise silently miss ligature
/// shaping in the actual render path. The bias is deliberately toward
/// "shape it" — a few extra cosmic-text calls on prompts containing
/// `=` cost less than a wrong rendering.
#[inline]
pub fn run_is_ascii_fast(cells: &[(u16, Cell)]) -> bool {
    cells.iter().all(|(_, c)| {
        c.extras().is_none()
            && {
                let n = c.ch as u32;
                (0x20..=0x7E).contains(&n) && !is_ligature_trigger(n as u8)
            }
            // Reject anything carrying cluster intent through a flag
            // we don't model in the fast path. WIDE_CONT shouldn't
            // reach a run at all (caller filters), but be defensive.
            && !c.flags.contains(CellFlags::WIDE_CONT)
            && !c.flags.contains(CellFlags::WIDE)
    })
}

/// One glyph the shaper produced for a style run.
#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    /// Column of the **lead cell** of the cluster this glyph belongs
    /// to. For 1:1 (plain ASCII) this is just the cell's own column;
    /// for a ligature (`=>` over two cells) both glyphs point at the
    /// leftmost cell — when there's only one glyph for the cluster,
    /// the second cell gets *no* glyph and rasterizes as a blank.
    pub lead_col: u16,
    /// First codepoint of the cluster (informational; baked into
    /// `GlyphKey.ch` purely for diagnostics).
    pub ch: char,
    /// Resolved slot in the SwashRasterizer fallback chain.
    pub font_slot: u8,
    /// Glyph id inside that font, as produced by cosmic-text /
    /// rustybuzz. `0` means notdef — caller should treat as missing.
    pub glyph_id: u16,
    /// Number of cluster source cells this glyph collapses. `1` =
    /// plain 1:1; `>1` = ligature / ZWJ; `0` should not occur.
    pub cluster_cells: u16,
}

/// Style a [`Cell`] contributes to a shape run. `(bold, italic)` —
/// these are the two attributes a font shaper *re-resolves* the face
/// for, so they bracket a single shape pass. Color is per-instance,
/// not per-glyph, so it's not part of the style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStyle {
    /// True if the run is bold.
    pub bold: bool,
    /// True if the run is italic.
    pub italic: bool,
}

impl RunStyle {
    /// Pull the (bold, italic) bits out of a cell.
    pub fn from_cell(c: &Cell) -> Self {
        Self {
            bold: c.flags.contains(CellFlags::BOLD),
            italic: c.flags.contains(CellFlags::ITALIC),
        }
    }
}

/// Shape a contiguous style run of `(col, cell)` pairs (no WIDE_CONT
/// entries) through cosmic-text and return one [`ShapedGlyph`] per
/// shaper-emitted glyph.
///
/// `family` is the primary family name (e.g. "Rec Mono Casual"); the
/// shaper falls back through the FontSystem's fontdb on a charmap
/// miss, matching the renderer's own fallback expectations.
///
/// Returns an empty vec if the run is empty.
pub fn shape_run(
    rasterizer: &mut SwashRasterizer,
    family: &str,
    font_size: f32,
    style: RunStyle,
    cells: &[(u16, Cell)],
) -> Vec<ShapedGlyph> {
    if cells.is_empty() {
        return Vec::new();
    }

    // Build a string for the run + a byte→col index. We keep the byte
    // index so the cluster `start` cosmic-text returns can be mapped
    // back to a column without re-walking the string.
    let mut text = String::with_capacity(cells.len() * 2);
    let mut byte_to_col: Vec<u16> = Vec::with_capacity(cells.len() * 2);
    for (col, cell) in cells {
        let start_len = text.len();
        text.push(cell.ch);
        // Cluster extras (ZWJ U+200D, combining marks). These are
        // zero-width per Unicode width but MUST be in the shaped string
        // so the font's GSUB sees the full cluster — that's the whole
        // point of preserving them through the grid.
        if let Some(extras) = cell.extras() {
            for ch in extras.chars() {
                text.push(ch);
            }
        }
        let appended = text.len() - start_len;
        for _ in 0..appended {
            byte_to_col.push(*col);
        }
    }
    // Sentinel so a cluster end == text.len() maps cleanly.
    byte_to_col.push(cells.last().map(|(c, _)| *c).unwrap_or(0));

    let weight = if style.bold { cosmic_text::Weight::BOLD } else { cosmic_text::Weight::NORMAL };
    let cstyle = if style.italic { cosmic_text::Style::Italic } else { cosmic_text::Style::Normal };
    let attrs = terminal_font_attrs(family).weight(weight).style(cstyle);

    // BufferLine + shape_in_buffer is the lowest-level shaping entry
    // point cosmic-text exposes that gives us LayoutGlyph back. We
    // avoid the full Buffer because we don't need wrap/scroll — runs
    // are short.
    let attrs_list = AttrsList::new(&attrs);
    let mut line = BufferLine::new(text.as_str(), LineEnding::None, attrs_list, Shaping::Advanced);

    // Shape: snapshot the slot table out of the rasterizer BEFORE
    // borrowing its font_system so we don't keep the rasterizer
    // borrowed across the layout call (which needs &mut FontSystem
    // too). We re-resolve slot ids from the shaped LayoutGlyph below.
    let font_system = rasterizer.font_system_mut();
    let layout_lines = line.layout(
        font_system,
        font_size,
        Some(f32::INFINITY),
        Wrap::None,
        Ellipsize::None,
        None,
        8,
        Hinting::Enabled,
    );

    // Collect raw glyph data first (own everything, drop the layout
    // borrow), then resolve slots via the rasterizer in a second pass.
    struct RawGlyph {
        start: usize,
        end: usize,
        glyph_id: u16,
        font_id: fontdb::ID,
    }
    let mut raw: Vec<RawGlyph> = Vec::new();
    for ll in layout_lines.iter() {
        for g in &ll.glyphs {
            raw.push(RawGlyph {
                start: g.start,
                end: g.end,
                glyph_id: g.glyph_id,
                font_id: g.font_id,
            });
        }
    }

    let mut out: Vec<ShapedGlyph> = Vec::new();
    let last_col = cells.last().map(|(c, _)| *c).unwrap_or(0);
    for g in raw {
        let lead_col = if g.start < byte_to_col.len() { byte_to_col[g.start] } else { last_col };
        let mut cluster_cells: u16 = 0;
        let end = g.end.min(byte_to_col.len());
        if g.start < end {
            let mut last_seen: Option<u16> = None;
            for c in &byte_to_col[g.start..end] {
                if Some(*c) != last_seen {
                    cluster_cells += 1;
                    last_seen = Some(*c);
                }
            }
        }
        if cluster_cells == 0 {
            cluster_cells = 1;
        }
        let ch_first = text[g.start..end].chars().next().unwrap_or('\0');
        // Resolve the cosmic-text-chosen font back to a slot in our
        // fallback chain. Two production failure modes if we trust the
        // shaped (slot, glyph_id) pair blindly for 1:1 cells:
        //
        //   1. `slot_for_font_id` returns None — cosmic-text shaped
        //      through an OS-resolved font that isn't in our
        //      PLATFORM_FALLBACK chain. Previously `unwrap_or(0)`
        //      pinned the shaped id to slot 0 (primary, e.g. Rec Mono
        //      Casual). Rasterizing a CJK glyph_id with the primary
        //      font produces an unrelated glyph at that index — bug:
        //      '中' rendered as '臭'.
        //
        //   2. `slot_for_font_id` returns Some(N), but cosmic-text and
        //      our `lookup_id(family[N], …)` resolve DIFFERENT files
        //      that share the family name (PingFang SC ships several
        //      variants; fontdb's `Name` query returns one variant,
        //      cosmic-text's shaping may have used another). The two
        //      files have different glyph orderings, so the shaped
        //      `glyph_id` points to a different *Chinese* glyph in the
        //      file we eventually rasterize through — bug: '中'
        //      rendered as '恶'.
        //
        // Both modes hit 1:1 cells (cluster_cells == 1) — for those the
        // shaped id buys us nothing (it would be a charmap lookup
        // either way), so zero the glyph_id and let the renderer take
        // the char-based fallback path (resolve_slot + charmap().map(ch)
        // against the actually-loaded font). Composed clusters
        // (ligatures `=>`, ZWJ emoji 👨‍👩‍👧) keep the shaped id —
        // cluster_cells > 1 and the shaped id is the ONLY way to get
        // the composed glyph; for those we accept the slot risk because
        // the composed visual is otherwise unreachable.
        let (slot, glyph_id) =
            match rasterizer.slot_for_font_id(g.font_id, style.bold, style.italic) {
                Some(s) if cluster_cells > 1 => (s, g.glyph_id),
                Some(s) => (s, 0),
                None => (0, 0),
            };
        out.push(ShapedGlyph { lead_col, ch: ch_first, font_slot: slot, glyph_id, cluster_cells });
    }
    out
}

/// Cached shape() output keyed by (text, style, font_family, px).
/// The renderer holds one of these across frames and reuses the glyph
/// list when a row's content+style hasn't changed since the last frame.
///
/// **Column-relative storage.** Cached glyphs store `lead_col` as an
/// offset from the run's first column (so the entry for `"hello"`
/// shaped at column 5 is identical to the entry for `"hello"` shaped
/// at column 10). On retrieval, the cache rebases each glyph's
/// `lead_col` to the caller's actual run start. Without this, the same
/// text at a different column would either cache-miss unnecessarily or
/// — worse — return stale absolute columns and the renderer would draw
/// the run at the original column instead of where it now belongs.
///
/// Cache invalidation is implicit: an unchanged row produces the same
/// `(text, style, font_family, px)` and therefore the same key.
///
/// **Eviction.** Backed by `lru::LruCache` with capacity
/// [`ShapeCache::CAPACITY`]. On overflow, the least-recently-used entry
/// is evicted (not the whole cache). This avoids the cold-cache stall
/// that the previous clear-on-overflow strategy caused when scrolling
/// through long files.
pub struct ShapeCache {
    map: lru::LruCache<ShapeCacheKey, Vec<ShapedGlyph>>,
    hits: u64,
    misses: u64,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct ShapeCacheKey {
    text: String,
    bold: bool,
    italic: bool,
    family: String,
    px: u32,
}

impl Default for ShapeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ShapeCache {
    /// Maximum number of distinct shaped runs retained before LRU
    /// eviction kicks in. 4096 is ~8× the prior clear-on-overflow cap
    /// and comfortably covers a screen's worth of unique rows with
    /// headroom for scrollback-driven churn.
    pub const CAPACITY: usize = 4096;

    /// Construct an empty cache pre-sized to [`Self::CAPACITY`] entries.
    pub fn new() -> Self {
        Self {
            map: lru::LruCache::new(
                // PANIC: safe — `Self::CAPACITY` is a const literal > 0.
                // Verified at compile time by the surrounding test
                // `capacity_is_non_zero` (see tests/shape.rs).
                std::num::NonZeroUsize::new(Self::CAPACITY).expect("CAPACITY is non-zero"),
            ),
            hits: 0,
            misses: 0,
        }
    }

    /// Cumulative cache-hit count.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cumulative cache-miss count.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Capacity (max entries before LRU eviction). Exposed for tests.
    pub fn capacity(&self) -> usize {
        self.map.cap().get()
    }

    /// True if the cache currently holds an entry for the same
    /// (text, style, family, px) key built from these cells. Test-only
    /// helper — does NOT update LRU recency.
    #[doc(hidden)]
    pub fn contains_run(
        &self,
        family: &str,
        font_size: f32,
        style: RunStyle,
        cells: &[(u16, Cell)],
    ) -> bool {
        let key = Self::make_key(family, font_size, style, cells);
        self.map.peek(&key).is_some()
    }

    fn make_key(
        family: &str,
        font_size: f32,
        style: RunStyle,
        cells: &[(u16, Cell)],
    ) -> ShapeCacheKey {
        let mut text = String::with_capacity(cells.len() * 2);
        for (_, c) in cells {
            text.push(c.ch);
            if let Some(ex) = c.extras() {
                for ch in ex.chars() {
                    text.push(ch);
                }
            }
        }
        ShapeCacheKey {
            text,
            bold: style.bold,
            italic: style.italic,
            family: family.to_string(),
            px: (font_size * 100.0).round() as u32,
        }
    }

    /// Lookup-or-shape. Bounded at [`Self::CAPACITY`] entries; on
    /// overflow the least-recently-used entry is evicted.
    ///
    /// Cached glyphs are stored with `lead_col` rebased to the run's
    /// first column (i.e. relative offsets starting from 0). On both
    /// miss and hit, the returned vec has `lead_col` values rebased to
    /// the caller's actual `cells[0].0` so the renderer can place each
    /// glyph at its real screen column without further bookkeeping.
    pub fn get_or_shape(
        &mut self,
        rasterizer: &mut SwashRasterizer,
        family: &str,
        font_size: f32,
        style: RunStyle,
        cells: &[(u16, Cell)],
    ) -> Vec<ShapedGlyph> {
        let base_col = cells.first().map(|(c, _)| *c).unwrap_or(0);
        let key = Self::make_key(family, font_size, style, cells);
        if let Some(v) = self.map.get(&key) {
            self.hits += 1;
            // Rebase relative columns to the caller's run start.
            return v
                .iter()
                .map(|g| ShapedGlyph { lead_col: g.lead_col.saturating_add(base_col), ..*g })
                .collect();
        }
        self.misses += 1;
        let shaped = shape_run(rasterizer, family, font_size, style, cells);
        // Store with column-relative `lead_col` (subtract the run's
        // base column) so the same shaped text at a different column
        // produces an identical cache entry on the next call.
        let stored: Vec<ShapedGlyph> = shaped
            .iter()
            .map(|g| ShapedGlyph { lead_col: g.lead_col.saturating_sub(base_col), ..*g })
            .collect();
        self.map.put(key, stored);
        shaped
    }

    /// Drop every cached entry.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

// Unit tests live in `tests/shape.rs`.
