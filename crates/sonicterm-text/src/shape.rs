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
use sonicterm_types::{Cell, CellFlags};

use crate::swash_rasterizer::SwashRasterizer;
use crate::terminal_font_attrs;

/// Characters that commonly participate in programming ligatures across
/// the fonts SonicTerm ships (Rec Mono Casual, JetBrains Mono). If a run
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
/// `family` is the primary family name (e.g. "Rec Mono St.Helens"); the
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
    shape_run_with_cell_w(rasterizer, family, font_size, style, cells, 0.0)
}

/// Nerd Font PUA codepoint ranges. If a font reports a glyph for a
/// codepoint that falls in one of these ranges, the renderer should
/// allocate two cells for the glyph even when the advance width is
/// ambiguous. Source: Nerd Fonts v3 cheat-sheet groupings.
///
/// Used by the singleton post-pass in [`shape_run_with_cell_w`] as a
/// fallback when the advance-threshold heuristic declines (advance
/// ≤ 1.5 * cell_w). See issue #595 for the diagnosis chain.
const NERD_FONT_RANGES: &[std::ops::RangeInclusive<u32>] = &[
    0xE000..=0xE0D7, // Powerline + Powerline Extra
    0xE200..=0xE2A9, // Font Linux
    0xE300..=0xE3D6, // Pomicons
    0xE5FA..=0xE62F, // Codicons (≠ unicode-width WIDE)
    0xE700..=0xE7E0, // Devicons
    0xEE00..=0xEE0B, // IEC Power symbols
    0xF000..=0xF2FF, // Font Awesome
    0xF300..=0xF381, // Font Logos
    0xF400..=0xF533, // Octicons
    0xF500..=0xFD46, // Material Design
];

#[inline]
fn is_nerd_font_pua(ch: char) -> bool {
    let n = ch as u32;
    NERD_FONT_RANGES.iter().any(|r| r.contains(&n))
}

/// Same as [`shape_run`] but also takes the per-cell pixel width
/// (`cell_w_px`, the raster-px width of one terminal cell at the
/// current font size). When `cell_w_px > 0.0`, singleton glyphs (those
/// whose `cluster_cells == 1` after the #587 ligature grouping
/// post-pass) are widened to `cluster_cells = 2` if either:
///
///   (a) `LayoutGlyph.w > 1.5 * cell_w_px` (advance heuristic — the
///       shaper itself laid the glyph wider than one-and-a-half cells,
///       so painting it at 1-cell width would visually overflow into
///       the next column), OR
///   (b) the source codepoint falls inside [`NERD_FONT_RANGES`] AND the
///       resolved font slot reports a non-zero charmap glyph for the
///       codepoint (fallback for icons whose advance is honest-1-cell
///       but whose drawn ink is still wider than a cell — common for
///       Powerline / Devicons in many NF builds).
///
/// Branch (a) is primary because it's font-agnostic and future-proof;
/// (b) is a known-bad-actor table for cases the advance check misses.
/// Both branches clamp to 2 cells — we never over-allocate.
///
/// Per the issue #595 implementation note, the NF-family detection
/// gate is intentionally NOT applied: false-positives on rare wide
/// PUA glyphs in non-NF fonts are an acceptable trade for the fix
/// actually firing on the user-affected installs.
///
/// Callers that don't have a meaningful cell width (overlay text,
/// help-line layout, synthetic test runs) pass `0.0` to disable both
/// branches and fall back to the original behaviour.
pub fn shape_run_with_cell_w(
    rasterizer: &mut SwashRasterizer,
    family: &str,
    font_size: f32,
    style: RunStyle,
    cells: &[(u16, Cell)],
    cell_w_px: f32,
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
        /// Shaper-reported advance width in raster pixels. Used by the
        /// singleton post-pass (#595 branch (a)) to widen icon glyphs
        /// whose advance overflows one cell.
        w: f32,
    }
    let mut raw: Vec<RawGlyph> = Vec::new();
    for ll in layout_lines.iter() {
        for g in &ll.glyphs {
            raw.push(RawGlyph {
                start: g.start,
                end: g.end,
                glyph_id: g.glyph_id,
                font_id: g.font_id,
                w: g.w,
            });
        }
    }

    // ── Post-pass: collapse placeholder-visible groups (issue #585) ──
    //
    // For some `calt` ligatures (e.g. JetBrains Mono `<=`, `===`)
    // cosmic-text returns N glyphs per source cluster: the LAST is the
    // visible substituted glyph, the preceding ones are placeholders.
    // If we emit one ShapedGlyph per raw entry the renderer paints the
    // ligature N times, each at width=1 cell -- so `<=` shows up as two
    // stacked glyphs in cells 0 and 1 instead of one 2-cell ligature.
    //
    // Diagnosis chain on the issue:
    //   - WezTerm study (Mike): `<=` cluster returns multiple glyphs
    //     and the renderer must group them into one 2-cell ligature.
    //   - Haiku Step-1: identified the placeholder pattern.
    //   - Opus Step-2 APPROVED-DIAG: spelled out the 2-condition gate.
    //
    // Detection gate (per Opus Step-2, conservative on purpose so CJK
    // and ZWJ paths are untouched):
    //   (a) GSUB-substituted: `charmap(ch) != shaped_glyph_id` on the
    //       slot cosmic-text picked (same probe rule 2 below uses for
    //       the #563 fix).
    //   (b) Source char is in the ligature-trigger set (`=`, `!`, `<`,
    //       `>`, `-`, `:`, `|`, `&`). Tighter than `is_ligature_trigger`
    //       (no `_` or `*`) to keep underscore-heavy idents safe.
    //
    // Grouping rule: consecutive raw glyphs that all pass (a) and (b)
    // collapse into ONE ShapedGlyph using the LAST raw glyph's
    // `glyph_id`/`font_slot` (the visible substituted form). `lead_col`
    // = first source cell of the group; `cluster_cells` = distinct
    // source cells the group spans. Singletons (size 1) flow through
    // the existing per-glyph 3-rule gate unchanged.
    fn is_group_trigger(ch: char) -> bool {
        matches!(ch, '=' | '!' | '<' | '>' | '-' | ':' | '|' | '&')
    }

    // Per-raw-glyph metadata snapshot. Pre-computed so the grouping
    // loop doesn't re-borrow the rasterizer per pair.
    struct GlyphMeta {
        slot: Option<u8>,
        ch_first: char,
        is_gsub_sub: bool,
        is_trigger: bool,
    }
    let mut meta: Vec<GlyphMeta> = Vec::with_capacity(raw.len());
    for g in &raw {
        let end = g.end.min(byte_to_col.len());
        let ch_first =
            if g.start < end { text[g.start..end].chars().next().unwrap_or(' ') } else { ' ' };
        let slot = rasterizer.slot_for_font_id(g.font_id, style.bold, style.italic);
        let is_gsub_sub = match slot {
            Some(s) => {
                let charmap_id = rasterizer
                    .charmap_glyph_for_slot(s, ch_first, style.bold, style.italic)
                    .unwrap_or(0);
                charmap_id != g.glyph_id
            }
            None => false,
        };
        let is_trigger = is_group_trigger(ch_first);
        meta.push(GlyphMeta { slot, ch_first, is_gsub_sub, is_trigger });
    }

    let mut out: Vec<ShapedGlyph> = Vec::new();
    let last_col = cells.last().map(|(c, _)| *c).unwrap_or(0);
    let mut i = 0;
    while i < raw.len() {
        // Candidate group iff this glyph itself passes (a) and (b).
        let mut j = i;
        if meta[i].is_gsub_sub && meta[i].is_trigger {
            while j + 1 < raw.len() && meta[j + 1].is_gsub_sub && meta[j + 1].is_trigger {
                j += 1;
            }
        }

        if j > i {
            // ── Multi-glyph group → emit as a single composed glyph ──
            // cosmic-text may give each glyph in the group the SAME
            // (start,end) byte cluster or distinct consecutive ones;
            // compute the source span as [min(starts), max(ends)).
            let first_start = raw[i..=j].iter().map(|r| r.start).min().unwrap_or(raw[i].start);
            let last_end_raw = raw[i..=j].iter().map(|r| r.end).max().unwrap_or(raw[j].end);
            let last_end = last_end_raw.min(byte_to_col.len());
            let lead_col =
                if first_start < byte_to_col.len() { byte_to_col[first_start] } else { last_col };
            let mut cluster_cells: u16 = 0;
            if first_start < last_end {
                let mut last_seen: Option<u16> = None;
                for c in &byte_to_col[first_start..last_end] {
                    if Some(*c) != last_seen {
                        cluster_cells += 1;
                        last_seen = Some(*c);
                    }
                }
            }
            if cluster_cells == 0 {
                cluster_cells = 1;
            }
            let ch_first = meta[i].ch_first;
            // meta[j].slot is Some(_) by construction (is_gsub_sub true
            // requires a resolved slot).
            let slot = meta[j].slot.unwrap_or(0);
            out.push(ShapedGlyph {
                lead_col,
                ch: ch_first,
                font_slot: slot,
                glyph_id: raw[j].glyph_id,
                cluster_cells,
            });
            i = j + 1;
            continue;
        }

        // ── Singleton path — original 3-rule gate (#563) ──
        let g = &raw[i];
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
        let ch_first = meta[i].ch_first;
        // Reuse meta[i] (slot + charmap-disagreement) to avoid a second
        // rasterizer lookup per glyph. See the historical block above
        // (kept intact) for the full rationale on each rule.
        let (slot, glyph_id) = match meta[i].slot {
            Some(s) if cluster_cells > 1 => (s, g.glyph_id),
            Some(s) => {
                if meta[i].is_gsub_sub {
                    // Real GSUB substitution (`calt` 1:1) -- preserve.
                    (s, g.glyph_id)
                } else {
                    // Trivial 1:1; zero so renderer charmaps itself
                    // (CJK family-variant safety, see (3b) above).
                    (s, 0)
                }
            }
            None => (0, 0),
        };
        // ── #595: singleton 1-cell nerd-font widening ──
        // After the #587 grouping has run, any glyph that is still a
        // singleton (cluster_cells == 1) may still be a wide icon the
        // shaper sized to >1 cell. Two-branch detection (see
        // `shape_run_with_cell_w` docs):
        //   (a) advance heuristic — primary, font-agnostic.
        //   (b) Nerd Font PUA table — fallback for honest-1-cell
        //       advances whose ink overflows.
        //
        // Gate: if the grid already marked the lead cell WIDE (CJK,
        // emoji), skip — the renderer's WIDE/WIDE_CONT path already
        // allocates the second cell and widening here would
        // double-count. Singletons that lack the WIDE flag but still
        // shape wider than the cell are exactly the #595 bug
        // population (nerd-font PUA icons whose ink the grid
        // intentionally left at width=1).
        if cluster_cells == 1 && cell_w_px > 0.0 {
            let lead_is_wide = cells
                .iter()
                .find(|(c, _)| *c == lead_col)
                .map(|(_, cell)| cell.flags.contains(CellFlags::WIDE))
                .unwrap_or(false);
            if !lead_is_wide {
                let mut widen = false;
                // (a) advance threshold
                if g.w > cell_w_px * 1.5 {
                    widen = true;
                }
                // (b) PUA range — only consult if (a) declined.
                if !widen && is_nerd_font_pua(ch_first) {
                    if let Some(s) = meta[i].slot {
                        let charmap_id = rasterizer
                            .charmap_glyph_for_slot(s, ch_first, style.bold, style.italic)
                            .unwrap_or(0);
                        if charmap_id != 0 {
                            widen = true;
                        }
                    }
                }
                if widen {
                    cluster_cells = 2;
                }
            }
        }
        out.push(ShapedGlyph { lead_col, ch: ch_first, font_slot: slot, glyph_id, cluster_cells });
        i += 1;
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
        self.get_or_shape_with_cell_w(rasterizer, family, font_size, style, cells, 0.0)
    }

    /// Same as [`Self::get_or_shape`] but threads the per-cell pixel
    /// width into the shaper so the singleton nerd-font widening
    /// post-pass (#595) can fire. See [`shape_run_with_cell_w`] for
    /// the heuristic. `cell_w_px == 0.0` disables the widening.
    ///
    /// `cell_w_px` is NOT part of the cache key — for a given
    /// (family, font_size) it is fixed, so adding it would only churn
    /// the key without affecting lookups.
    pub fn get_or_shape_with_cell_w(
        &mut self,
        rasterizer: &mut SwashRasterizer,
        family: &str,
        font_size: f32,
        style: RunStyle,
        cells: &[(u16, Cell)],
        cell_w_px: f32,
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
        let shaped = shape_run_with_cell_w(rasterizer, family, font_size, style, cells, cell_w_px);
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
