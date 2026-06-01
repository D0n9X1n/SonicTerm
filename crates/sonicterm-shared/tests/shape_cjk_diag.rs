//! Diagnostic test for CJK shaping — runs against the OS FontSystem
//! (which on macOS includes PingFang SC) to see what cosmic-text
//! actually emits for "中文测试".

use cosmic_text::FontSystem;
use sonicterm_core::grid::{Cell, CellFlags};
use sonicterm_shared::shape::{shape_run, RunStyle};
use sonicterm_shared::swash_rasterizer::SwashRasterizer;

fn wide_cell(ch: char) -> Cell {
    Cell::plain(
        ch,
        sonicterm_core::grid::Color::Default,
        sonicterm_core::grid::Color::Default,
        CellFlags::WIDE,
    )
}

#[test]
fn cjk_diag_what_does_cosmic_text_return() {
    // WIDE cells at cols 0, 2, 4, 6 (each CJK char occupies 2 grid cols).
    let cells: Vec<(u16, Cell)> = vec![
        (0, wide_cell('中')),
        (2, wide_cell('文')),
        (4, wide_cell('测')),
        (6, wide_cell('试')),
    ];

    let mut fs = FontSystem::new();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", 28.0);
    let out = shape_run(
        &mut r,
        "Rec Mono St.Helens",
        14.0,
        RunStyle { bold: false, italic: false },
        &cells,
    );

    eprintln!("=== shape_run output for 中文测试 ===");
    eprintln!("glyph count: {}", out.len());
    for (i, g) in out.iter().enumerate() {
        eprintln!(
            "  [{i}] lead_col={} ch={:?} (U+{:04X}) font_slot={} glyph_id={} cluster_cells={}",
            g.lead_col, g.ch, g.ch as u32, g.font_slot, g.glyph_id, g.cluster_cells
        );
    }

    // What we WANT: 4 glyphs, lead_cols [0,2,4,6], chs ['中','文','测','试'],
    // cluster_cells = 1 each (forces fallback path → correct char lookup).
    assert_eq!(out.len(), 4, "expected 4 glyphs for 4 CJK chars");
    let want_chars = ['中', '文', '测', '试'];
    let want_cols = [0u16, 2, 4, 6];
    for (i, g) in out.iter().enumerate() {
        assert_eq!(g.ch, want_chars[i], "glyph {i}: ch mismatch — got {:?}", g.ch);
        assert_eq!(g.lead_col, want_cols[i], "glyph {i}: lead_col mismatch");
        assert_eq!(
            g.cluster_cells, 1,
            "glyph {i}: cluster_cells must be 1 for CJK so the char-fallback path \
             zeros glyph_id and uses the correct codepoint for charmap lookup. \
             If this is >1 the renderer keeps the shaped glyph_id and CJK chars \
             render through the WRONG font slot/file → mangled output."
        );
    }
}
