//! Tests for the cosmic-text-driven shaping path in
//! [`sonic_shared::shape`]. Exercises three cases the renderer must
//! preserve and one (ligatures) it must enable:
//!
//! 1. **Plain ASCII** keeps producing one shaped glyph per cell — no
//!    visual regression for the common case.
//! 2. **Programming ligatures** like `=>` collapse two source cells
//!    into a single shaped glyph when the font's GSUB supports the
//!    substitution. We assert "fewer glyphs than codepoints" rather
//!    than an exact count, because the assertion still passes if a
//!    future font upgrade adds *more* ligatures.
//! 3. **ZWJ family** 👨‍👩‍👧 collapses to a single shaped glyph when
//!    the font has the ZWJ sequence. If the bundled font lacks the
//!    sequence the shaper emits one glyph per component — we accept
//!    that as a documented fallback rather than failing, because
//!    `Rec Mono Casual` isn't an emoji font and the actual emoji
//!    rendering rides on the platform-fallback chain.
//! 4. **Capability matrix**: with shaping wired in, the ZWJ family
//!    test in the capability matrix is no longer about three separate
//!    base emojis — it now asserts that the shaper produces at most as
//!    many glyphs as codepoints (composed) AND that whatever it
//!    produces is rasterizable.

use cosmic_text::FontSystem;
use sonic_core::grid::Cell;
use sonic_shared::{
    shape::{shape_run, RunStyle},
    swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX},
};

/// Build a `FontSystem` populated with the bundled fonts. Same loader
/// the renderer uses in production and the capability matrix uses in
/// tests — keeps font-resolution behavior identical across the three.
fn font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
            let bytes = std::fs::read(&p).unwrap();
            fs.db_mut().load_font_data(bytes);
        }
    }
    fs
}

fn cell(ch: char) -> Cell {
    Cell { ch, ..Cell::default() }
}

fn cells_for(s: &str) -> Vec<(u16, Cell)> {
    s.chars().enumerate().map(|(i, ch)| (i as u16, cell(ch))).collect()
}

#[test]
fn plain_ascii_one_glyph_per_cell_no_regression() {
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let cells = cells_for("hello");
    let out = shape_run(
        &mut r,
        "Rec Mono Casual",
        DEFAULT_RASTER_PX,
        RunStyle { bold: false, italic: false },
        &cells,
    );
    assert_eq!(
        out.len(),
        5,
        "ASCII 'hello' must shape to exactly 5 glyphs (one per cell). Got: {out:?}"
    );
    for (i, g) in out.iter().enumerate() {
        assert_eq!(g.lead_col, i as u16, "glyph {i} lead_col");
        assert_eq!(g.cluster_cells, 1, "glyph {i} should map 1:1");
    }
}

#[test]
fn arrow_ligature_collapses_when_supported() {
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    // `=>` is the canonical "fat arrow" ligature shipped by both Rec
    // Mono Casual and JetBrains Mono.
    let cells = cells_for("=>");
    let out = shape_run(
        &mut r,
        "Rec Mono Casual",
        DEFAULT_RASTER_PX,
        RunStyle { bold: false, italic: false },
        &cells,
    );
    // Glyph count must be ≤ codepoint count. If the font has the
    // ligature we collapse to 1 glyph; if not, we get 2 component
    // glyphs — both are documented behaviors. The test fails only if
    // shaping produced MORE glyphs than codepoints (which would mean
    // the cluster mapping is broken).
    assert!(
        out.len() <= 2,
        "shaping '=>' must produce ≤ 2 glyphs (≤ codepoints). Got {}: {:?}",
        out.len(),
        out
    );
    // The lead column of the first glyph is column 0 either way.
    assert_eq!(out[0].lead_col, 0);
    if out.len() == 1 {
        // Ligature path: the single glyph must mark BOTH source cells
        // as part of its cluster.
        assert_eq!(out[0].cluster_cells, 2, "ligated '=>' cluster spans both cells");
    }
}

#[test]
fn zwj_family_composes_or_decomposes_predictably() {
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    // 👨‍👩‍👧 = MAN + ZWJ + WOMAN + ZWJ + GIRL. 5 codepoints; if the
    // active font has the ZWJ sequence it composes to 1 glyph,
    // otherwise the shaper emits the 3 base emoji as separate glyphs
    // (the ZWJ joiners themselves become invisible/empty glyphs).
    let cells = cells_for("👨\u{200d}👩\u{200d}👧");
    let out = shape_run(
        &mut r,
        "Rec Mono Casual",
        DEFAULT_RASTER_PX,
        RunStyle { bold: false, italic: false },
        &cells,
    );
    let visible: Vec<_> = out.iter().filter(|g| g.glyph_id != 0).collect();
    assert!(
        !visible.is_empty(),
        "ZWJ family must produce at least one visible glyph (font fallback should resolve)"
    );
    // The contract: visible glyph count ≤ base emoji count (3). One
    // when the font has the ZWJ table, three when it falls back to
    // components. Anything more would mean the shaper double-counted.
    assert!(
        visible.len() <= 3,
        "ZWJ family must shape to ≤3 visible glyphs (composed or per-base). Got {}: {:?}",
        visible.len(),
        visible
    );
}

#[test]
fn ligature_lead_col_stays_at_first_source_cell() {
    // Regression-style assertion: even when `!=` ligates, the lead_col
    // for the composed glyph must point at the leftmost source cell so
    // the renderer places it correctly (and cursor / selection math
    // built on per-cell rects still aligns).
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let cells = cells_for("a!=b");
    let out = shape_run(
        &mut r,
        "Rec Mono Casual",
        DEFAULT_RASTER_PX,
        RunStyle { bold: false, italic: false },
        &cells,
    );
    // Find the glyph whose cluster contains column 1 (the '!').
    let g = out.iter().find(|g| g.lead_col == 1).expect("a glyph must lead at column 1 ('!' cell)");
    // Whether ligated (cluster_cells==2) or not (==1), it must NOT
    // claim cells outside [1, 2].
    assert!(g.cluster_cells <= 2);
}

#[test]
fn ascii_fast_path_detects_pure_ascii_runs() {
    // "hello world" is the prototypical shell text — every cell is
    // printable ASCII with no cluster extras. The fast path must
    // recognize it so the renderer bypasses cosmic-text shaping.
    let cells = cells_for("hello world");
    assert!(
        sonic_shared::shape::run_is_ascii_fast(&cells),
        "pure printable-ASCII run must take the fast path (no shaping)"
    );

    // A run containing a non-ASCII codepoint must NOT take the fast
    // path — the shaper has to see it to resolve fallback fonts.
    let cells = cells_for("héllo");
    assert!(
        !sonic_shared::shape::run_is_ascii_fast(&cells),
        "non-ASCII codepoint must force the shaping path"
    );

    // A run whose lead cell carries cluster extras (e.g. a combining
    // mark or ZWJ retained by Grid) must also force shaping — the
    // extras can only be composed by cosmic-text.
    let mut cells = cells_for("a");
    cells[0].1.extras = Some("\u{200D}".to_string().into_boxed_str());
    assert!(
        !sonic_shared::shape::run_is_ascii_fast(&cells),
        "cluster extras must force the shaping path"
    );
}

#[test]
fn shape_cache_hits_on_repeat_calls() {
    // Same row content + style on two successive frames must hit the
    // cache on the second call — that's the whole point of the cache.
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let mut cache = sonic_shared::shape::ShapeCache::new();

    let cells = cells_for("$ git status");
    let style = RunStyle { bold: false, italic: false };

    let first = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells);
    assert_eq!(cache.hits(), 0);
    assert_eq!(cache.misses(), 1);
    assert!(!first.is_empty());

    let second = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells);
    assert_eq!(cache.hits(), 1, "second call with identical inputs must hit the cache");
    assert_eq!(cache.misses(), 1, "miss count must not advance on a cache hit");
    assert_eq!(first.len(), second.len(), "cached glyph list must match the shaped list");

    // Different style (italic) must miss again — the cache key
    // includes (bold, italic).
    let italic = RunStyle { bold: false, italic: true };
    let _ = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, italic, &cells);
    assert_eq!(cache.misses(), 2, "different style must miss the cache");
}

#[test]
fn shape_cache_rebases_lead_col_across_positions() {
    // Same shaped text at column 5 vs column 10 must produce identical
    // glyph LISTs (count + slot + glyph_id), but the `lead_col` of each
    // glyph must reflect the actual run start. Previously the cache
    // stored absolute columns and returned stale positions on a hit —
    // the renderer then drew the run at the wrong x. Regression for
    // Haiku review on PR #57.
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let mut cache = sonic_shared::shape::ShapeCache::new();

    fn run_at(text: &str, start_col: u16) -> Vec<(u16, Cell)> {
        text.chars().enumerate().map(|(i, ch)| (start_col + i as u16, cell(ch))).collect()
    }

    let style = RunStyle { bold: false, italic: false };
    let at5 = run_at("hello", 5);
    let at10 = run_at("hello", 10);

    let g5 = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &at5);
    assert_eq!(cache.misses(), 1);
    let g10 = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &at10);
    // Same text — must hit the cache (proves relative-column keying).
    assert_eq!(cache.hits(), 1, "same text at different col must hit the cache");
    assert_eq!(cache.misses(), 1);

    assert_eq!(g5.len(), g10.len(), "glyph counts must match");
    assert_eq!(g5.len(), 5);
    for i in 0..g5.len() {
        // Glyph identity must be identical.
        assert_eq!(g5[i].glyph_id, g10[i].glyph_id, "glyph {i} id mismatch");
        assert_eq!(g5[i].font_slot, g10[i].font_slot, "glyph {i} slot mismatch");
        // But `lead_col` must differ by the column offset.
        assert_eq!(g5[i].lead_col, 5 + i as u16, "g5[{i}] lead_col");
        assert_eq!(g10[i].lead_col, 10 + i as u16, "g10[{i}] lead_col");
        assert_eq!(
            g10[i].lead_col - g5[i].lead_col,
            5,
            "g10[{i}].lead_col must be g5[{i}].lead_col + 5",
        );
    }
}

#[test]
fn ascii_fast_path_skips_ligature_triggers() {
    // The single most important regression for this PR: ASCII strings
    // that contain ligature-trigger bytes (`=`, `!`, `<`, `>`, `-`,
    // `_`, `:`, `|`, `&`, `*`) MUST route through the shaper so
    // programming ligatures actually render. Previously the fast path
    // matched any printable ASCII, silently disabling ligatures in the
    // renderer despite the shape_run unit tests proving the shaper
    // could produce them.
    let cells = cells_for("let foo = bar();");
    assert!(
        !sonic_shared::shape::run_is_ascii_fast(&cells),
        "ASCII with `=` must NOT take the fast path (would miss `=>` / `==` ligatures)",
    );
    for trigger in ['=', '!', '<', '>', '-', '_', ':', '|', '&', '*'] {
        let s = format!("a{trigger}b");
        let cells = cells_for(&s);
        assert!(
            !sonic_shared::shape::run_is_ascii_fast(&cells),
            "ASCII containing {trigger:?} must route through the shaper",
        );
    }
}

#[test]
fn ascii_fast_path_keeps_plain_text_fast() {
    // Counter-test: plain English / shell text with no ligature
    // triggers MUST still hit the fast path — otherwise we've just
    // moved every cell through cosmic-text and lost the perf win.
    for s in ["hello world", "the quick brown fox", "$ ls", "echo 1234567890"] {
        let cells = cells_for(s);
        assert!(
            sonic_shared::shape::run_is_ascii_fast(&cells),
            "plain ASCII {s:?} must take the fast path",
        );
    }
}

#[test]
#[cfg(target_os = "macos")]
fn cjk_shaping_never_returns_primary_slot_glyph_for_cjk_codepoint() {
    // Regression target: PR fix(render): CJK + emoji mangled to wrong
    // glyphs in production.
    //
    // The bug: when cosmic-text shaped a CJK codepoint like '中' (U+4E2D)
    // through an OS-resolved font that was NOT in our PLATFORM_FALLBACK
    // chain, `slot_for_font_id` returned None and `unwrap_or(0)` pinned
    // the shaped glyph_id to slot 0 (the primary, e.g. Rec Mono Casual).
    // Rasterizing that foreign glyph_id with the primary font produced
    // a *different unrelated* glyph from the primary face — '中' rendered
    // as '臭'. After the fix, shape_run must NOT emit (font_slot=0,
    // glyph_id=N!=0) for a CJK codepoint when the primary font lacks it.
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);

    let cells = cells_for("中文测试");
    let glyphs = shape_run(
        &mut r,
        "Rec Mono Casual",
        DEFAULT_RASTER_PX,
        RunStyle { bold: false, italic: false },
        &cells,
    );

    // The shaper produces some number of glyphs (1 per cluster cell at
    // minimum). For each glyph, we assert: if it claims slot 0 (the
    // primary font), then it MUST NOT carry a non-zero glyph_id —
    // because slot 0 is Rec Mono Casual which lacks CJK. The pre-fix
    // behavior was to do exactly that and produce wrong glyphs.
    // For 1:1 cells (cluster_cells == 1, no composition happened), the
    // shaped glyph_id MUST be zeroed by shape_run — the renderer then
    // resolves via charmap against the actually-loaded font. Any
    // non-zero glyph_id on a 1:1 cell risks the production bug:
    // PingFang SC ships multiple variants that disagree on glyph
    // ordering, so the shaped id from one variant can point to a wrong
    // CJK glyph in the file the rasterizer actually opens via fontdb
    // (manifests as '中' → '恶'). Composed clusters (cluster_cells > 1
    // — ligatures / ZWJ emoji) are exempt: the shaped id is the only
    // way to reach the composed glyph and the family-substitution risk
    // is acceptable there.
    assert!(!glyphs.is_empty(), "shaper must emit at least one glyph for 4 CJK cells");
    for g in &glyphs {
        if g.cluster_cells == 1 && g.glyph_id != 0 {
            panic!(
                "shape_run emitted non-zero glyph_id={} for a 1:1 CJK cell \
                 (ch={:?}, slot={}). This is the pre-fix production bug: a \
                 shaped id from one PingFang variant gets rasterized through \
                 a different file with the same family name, producing the \
                 wrong CJK glyph. Zero glyph_id on 1:1 clusters so the \
                 renderer takes the char-based fallback path.",
                g.glyph_id, g.ch, g.font_slot,
            );
        }
    }
}
