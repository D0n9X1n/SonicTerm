//! Tests for `shape_run` (cosmic-text-backed run shaping).
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/shape.rs`.

use cosmic_text::FontSystem;
use sonic_text::shape::{shape_run, RunStyle};
use sonic_text::swash_rasterizer::SwashRasterizer;
use sonic_types::Cell;

fn cell(ch: char) -> Cell {
    Cell { ch, ..Cell::default() }
}

fn font_system_with_assets() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                if let Ok(bytes) = std::fs::read(&p) {
                    fs.db_mut().load_font_data(bytes);
                }
            }
        }
    }
    fs
}

#[test]
fn plain_ascii_one_glyph_per_cell() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells: Vec<(u16, Cell)> =
        "abc".chars().enumerate().map(|(i, ch)| (i as u16, cell(ch))).collect();
    let out =
        shape_run(&mut r, "Rec Mono Casual", 14.0, RunStyle { bold: false, italic: false }, &cells);
    assert_eq!(out.len(), 3, "ASCII abc must produce one glyph per cell");
    for (i, g) in out.iter().enumerate() {
        assert_eq!(g.lead_col, i as u16);
        assert_eq!(g.cluster_cells, 1);
    }
}
