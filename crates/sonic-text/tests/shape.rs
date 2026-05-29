//! Tests for `shape_run` (cosmic-text-backed run shaping).
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/shape.rs`.

use cosmic_text::{fontdb, FontSystem};
use sonic_text::shape::{shape_run, RunStyle};
use sonic_text::swash_rasterizer::{lookup_id_in_db, SwashRasterizer};
use sonic_types::Cell;

fn cell(ch: char) -> Cell {
    let mut c = Cell::default();
    c.ch = ch;
    c
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
                    sonic_text::load_font_data_with_sonic_overrides(&mut fs, bytes);
                }
            }
        }
    }
    fs
}

#[test]
fn bundled_st_helens_normal_query_resolves_upright_face() {
    let fs = font_system_with_assets();
    let id = lookup_id_in_db(fs.db(), "Rec Mono St.Helens", false, false)
        .expect("bundled Rec Mono St.Helens family must resolve");
    let face = fs.db().face(id).expect("resolved fontdb face must exist");
    assert_eq!(
        face.style,
        fontdb::Style::Normal,
        "bundled Rec Mono St.Helens Regular must be registered as upright Normal, not Italic"
    );
}

#[test]
fn normal_lookup_rejects_italic_face_when_regular_is_missing() {
    let mut fs = FontSystem::new();
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/RecMonoSt.Helens-Italic.ttf");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    fs.db_mut().load_font_data(bytes);

    assert!(
        lookup_id_in_db(fs.db(), "Rec Mono St.Helens", false, false).is_none(),
        "upright lookup must not accept fontdb's fuzzy Italic fallback when Regular is absent"
    );
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
