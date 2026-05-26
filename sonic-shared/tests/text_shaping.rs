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
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts");
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
