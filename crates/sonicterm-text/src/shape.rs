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

/// Pure helper for the #594 grouping gate. Returns the inclusive cluster
/// end index `j` for the group starting at `i`. `j == i` means "no
/// group, fall through to the singleton path".
///
/// Three independent guards, all required to extend:
///   1. `is_trigger[k]` -- cheap, ordering-independent prefilter (avoids
///      touching CJK / alphanumeric runs).
///   2. **Shaper cluster boundary** (`starts[k+1] < ends[k]`): the next
///      glyph's source-byte range must overlap the current one's. When
///      `next.start >= current.end` we are crossing into a different
///      shaper cluster -- e.g. `==<=` where `==` is one cluster and `<=`
///      is another -- and merging would over-collapse two independent
///      ligatures into one (Haiku #594 Step-5 blocker).
///   3. At least one member of the extended span has `is_gsub_sub[k]`
///      true, so plain `==` on a calt-less font does NOT get falsely
///      collapsed into a composed glyph.
///
/// Extracted to module scope so #594 / #587 tests can exercise the gate
/// against synthetic placeholder-first and visible-last orderings without
/// needing a JetBrains-Mono-style font in the bundle.
pub(crate) fn extend_ligature_group(
    is_trigger: &[bool],
    is_gsub_sub: &[bool],
    starts: &[usize],
    ends: &[usize],
    i: usize,
) -> usize {
    let n = is_trigger.len();
    debug_assert_eq!(n, is_gsub_sub.len());
    debug_assert_eq!(n, starts.len());
    debug_assert_eq!(n, ends.len());
    if i >= n || !is_trigger[i] {
        return i;
    }
    let mut j = i;
    // Extend across consecutive trigger glyphs that ALSO live in the
    // same shaper cluster (overlapping source byte ranges). The moment
    // `next.start >= current.end` we are at a cluster boundary and must
    // stop -- otherwise two adjacent trigger clusters would merge.
    while j + 1 < n && is_trigger[j + 1] && starts[j + 1] < ends[j] {
        j += 1;
    }
    if !(i..=j).any(|k| is_gsub_sub[k]) {
        return i;
    }
    j
}

#[cfg(test)]
mod gate_tests {
    use super::extend_ligature_group;

    // Helper: glyphs at consecutive byte positions, one per cell.
    // Models the typical cosmic-text output where each source cell
    // produces one glyph and consecutive glyphs share a single
    // 1-byte-wide cluster span (the cluster collapses internally).
    fn same_cluster_starts_ends(n: usize) -> (Vec<usize>, Vec<usize>) {
        // All glyphs in the same shaper cluster share an overlapping
        // byte range. Model as start=0, end=n for every glyph -- the
        // simplest representation that satisfies `starts[k+1] < ends[k]`
        // for every pair.
        (vec![0; n], vec![n; n])
    }

    // Visible-last (Rec Mono St.Helens style): lead is itself gsub_sub.
    // The original #587 gate (lead must be gsub_sub) and the broadened
    // #594 gate must BOTH accept this pattern -- 2-cell `<=` collapses.
    #[test]
    fn visible_last_two_cell_groups() {
        let trig = vec![true, true];
        let sub = vec![true, true];
        let (s, e) = same_cluster_starts_ends(2);
        assert_eq!(extend_ligature_group(&trig, &sub, &s, &e, 0), 1);
    }

    // Placeholder-first (JetBrains Mono / Rec Mono Casual style): lead
    // is a trigger char but glyph_id == charmap(ch), so is_gsub_sub is
    // false. Only the TRAILING glyph carries the substitution. The
    // original #587 gate rejected this and emitted 2 overlapping
    // ShapedGlyphs (#594). The broadened gate MUST accept it.
    #[test]
    fn placeholder_first_two_cell_groups() {
        let trig = vec![true, true];
        let sub = vec![false, true];
        let (s, e) = same_cluster_starts_ends(2);
        assert_eq!(extend_ligature_group(&trig, &sub, &s, &e, 0), 1);
    }

    // Mid-cluster placeholder (e.g. `===` where only the middle is
    // visible-substituted). ANY-member rule still collapses.
    #[test]
    fn mid_cluster_placeholder_three_cell_groups() {
        let trig = vec![true, true, true];
        let sub = vec![false, true, false];
        let (s, e) = same_cluster_starts_ends(3);
        assert_eq!(extend_ligature_group(&trig, &sub, &s, &e, 0), 2);
    }

    // Plain `==` on a font WITHOUT calt for `==`: both triggers, NEITHER
    // substituted. The broadened gate must NOT false-positive into a
    // composed glyph -- fall through to singleton path.
    #[test]
    fn no_calt_pair_falls_through_to_singletons() {
        let trig = vec![true, true];
        let sub = vec![false, false];
        let (s, e) = same_cluster_starts_ends(2);
        assert_eq!(extend_ligature_group(&trig, &sub, &s, &e, 0), 0);
    }

    // Singleton trigger: cluster of size 1 should never trigger the
    // group path regardless of substitution state.
    #[test]
    fn singleton_trigger_never_groups() {
        let (s, e) = same_cluster_starts_ends(1);
        assert_eq!(extend_ligature_group(&[true], &[true], &s, &e, 0), 0);
        assert_eq!(extend_ligature_group(&[true], &[false], &s, &e, 0), 0);
    }

    // Non-trigger lead: never enters the group path. Protects CJK and
    // alphanumeric runs from being touched by the post-pass.
    #[test]
    fn non_trigger_lead_short_circuits() {
        let trig = vec![false, true, true];
        let sub = vec![false, true, true];
        let (s, e) = same_cluster_starts_ends(3);
        assert_eq!(extend_ligature_group(&trig, &sub, &s, &e, 0), 0);
    }

    // #594 Step-5 negative regression: TWO adjacent trigger clusters
    // (e.g. `==<=` where `==` shapes to one cluster and `<=` shapes to
    // another). Only the SECOND cluster has a gsub_sub member. Without
    // the cluster-boundary guard, the ANY-member rule would walk the
    // whole trigger run and collapse `==<=` into a single composed
    // glyph, eating the literal `==`. With the guard, group 0 must
    // fall through to singletons (i == 0 returned).
    #[test]
    fn adjacent_clusters_only_one_substituted_does_not_merge() {
        //   idx:           0     1     2     3
        //   chars:        '='   '='   '<'   '='
        //   cluster A:  [0, 2)        — `==` literal, no gsub
        //   cluster B:        [2, 4)  — `<=` ligated, gsub on raw[3]
        let trig = vec![true, true, true, true];
        let sub = vec![false, false, false, true];
        // Cluster A spans bytes 0..2, cluster B spans 2..4. The guard
        // `starts[k+1] < ends[k]` is true within each cluster (0 < 2)
        // and false at the boundary (2 < 2 is false), so the walk stops.
        let starts = vec![0, 0, 2, 2];
        let ends = vec![2, 2, 4, 4];

        // From i=0 (start of cluster A): no gsub in [0..=1], must NOT
        // extend into cluster B. Result == i, fall through.
        assert_eq!(extend_ligature_group(&trig, &sub, &starts, &ends, 0), 0);

        // From i=2 (start of cluster B): gsub present on raw[3], extend
        // within B, stop at end of input. Result == 3.
        assert_eq!(extend_ligature_group(&trig, &sub, &starts, &ends, 2), 3);
    }

    // Mirror of the above with cluster A substituted, cluster B literal.
    // From i=0 we must group A only (==0 stops at j=1), and from i=2
    // we must fall through to singletons. Catches a symmetric bug where
    // the gate accidentally extended right through the boundary.
    #[test]
    fn adjacent_clusters_first_substituted_does_not_merge() {
        let trig = vec![true, true, true, true];
        let sub = vec![false, true, false, false];
        let starts = vec![0, 0, 2, 2];
        let ends = vec![2, 2, 4, 4];

        assert_eq!(extend_ligature_group(&trig, &sub, &starts, &ends, 0), 1);
        assert_eq!(extend_ligature_group(&trig, &sub, &starts, &ends, 2), 2);
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
    // Hoist the per-glyph trigger/sub bitvecs out of the loop -- the
    // grouping helper reads them by index and they don't change. Also
    // hoist cluster-boundary `starts`/`ends` so the gate can detect
    // shaper-cluster transitions (Haiku #594 Step-5 blocker: without
    // this, two adjacent trigger clusters where only one has a gsub_sub
    // member would falsely merge).
    let trig: Vec<bool> = meta.iter().map(|m| m.is_trigger).collect();
    let sub: Vec<bool> = meta.iter().map(|m| m.is_gsub_sub).collect();
    let starts: Vec<usize> = raw.iter().map(|r| r.start).collect();
    let ends: Vec<usize> = raw.iter().map(|r| r.end).collect();
    let mut i = 0;
    while i < raw.len() {
        // See `extend_ligature_group` (module-level) for the rule and
        // the unit-test fixtures covering visible-last (Rec Mono
        // St.Helens), placeholder-first (JetBrains Mono / Rec Mono
        // Casual), and the #594 adjacent-cluster negative case.
        let j = extend_ligature_group(&trig, &sub, &starts, &ends, i);

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
            // Pick the VISIBLE substituted glyph (any gsub_sub member),
            // regardless of position. For "visible-last" fonts this is
            // raw[j]; for "placeholder-first" fonts (#594) it's an
            // earlier index. Fall back to raw[j] if -- defensively --
            // no member is flagged (cannot happen given the any_sub
            // gate above, but keeps the code total).
            let visible = (i..=j).find(|k| meta[*k].is_gsub_sub).unwrap_or(j);
            let slot = meta[visible].slot.unwrap_or(0);
            out.push(ShapedGlyph {
                lead_col,
                ch: ch_first,
                font_slot: slot,
                glyph_id: raw[visible].glyph_id,
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
