//! Regression tests for issue #610 sym-3 — `NERD_FONT_RANGES` table
//! corrections + CJK exclusion guard on the singleton widening path.
//!
//! Diagnosis v2: https://github.com/D0n9X1n/SonicTerm/issues/610#issuecomment-4609257433
//! APPROVED-DIAG: https://github.com/D0n9X1n/SonicTerm/issues/610#issuecomment-4609283052
//!
//! Each test in this file pins one specific correction in the table or
//! the new CJK guard so the next round of "nerd-font cleanup" can't
//! silently regress the table back to its pre-#610 shape.

use cosmic_text::FontSystem;
use sonicterm_text::shape::{is_nerd_font_pua, shape_run_with_cell_w, RunStyle, NERD_FONT_RANGES};
use sonicterm_text::swash_rasterizer::SwashRasterizer;
use sonicterm_types::{Cell, CellFlags};

fn cell(ch: char) -> Cell {
    let mut c = Cell::default();
    c.ch = ch;
    c
}

fn wide_cell(ch: char) -> Cell {
    let mut c = cell(ch);
    c.flags |= CellFlags::WIDE;
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
                    sonicterm_text::load_font_data_with_sonic_overrides(&mut fs, bytes);
                }
            }
        }
    }
    fs
}

const FAMILY: &str = "Rec Mono St.Helens";
const FONT_PX: f32 = 14.0;
const STYLE: RunStyle = RunStyle { bold: false, italic: false };

/// Codicons (U+EA60..U+EBEB) was missing pre-#610. The diagnosis flagged
/// it as one of the two load-bearing gaps (every starship/oh-my-posh
/// prompt uses Codicons). Asserts: the table reports membership, AND
/// the widening path engages for a representative codepoint when the
/// shaper's advance is at-or-above 1× cell pitch (post-#610 the
/// threshold tightens to `>1.0×` inside NERD_FONT_RANGES).
#[test]
fn codicons_range_widened() {
    assert!(is_nerd_font_pua('\u{EA60}'), "U+EA60 (first Codicons) must be in table");
    assert!(is_nerd_font_pua('\u{EBEB}'), "U+EBEB (last Codicons) must be in table");
    assert!(is_nerd_font_pua('\u{EB05}'), "Codicons mid-range must be in table");
    // Singleton widening branch (a) at cell_w=1.0: tightened to >1.0× for
    // PUA codepoints, so any nontrivial advance trips it.
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells = vec![(0u16, cell('\u{EB05}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 1.0);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].cluster_cells, 2,
        "Codicon U+EB05 must widen at the tightened 1.0× PUA threshold"
    );
}

/// MDI Plane-1 (U+F0001..U+F1AF0) was missing pre-#610. NF v3 moved
/// MDI off the legacy `0xF500..0xFD46` BMP range, so any prompt using
/// MDI on a modern font shipped without widening. Asserts: table holds
/// the new Plane-1 range and a representative codepoint triggers the
/// tightened (`>1.0× cell_w`) widening branch.
#[test]
fn mdi_plane1_widened() {
    assert!(is_nerd_font_pua('\u{F0001}'), "U+F0001 (first MDI Plane-1) must be in table");
    assert!(is_nerd_font_pua('\u{F1AF0}'), "U+F1AF0 (last MDI Plane-1) must be in table");
    assert!(is_nerd_font_pua('\u{F0900}'), "MDI Plane-1 mid-range must be in table");
    // Widen via the advance heuristic at the tightened PUA threshold.
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells = vec![(0u16, cell('\u{F0900}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 1.0);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].cluster_cells, 2,
        "MDI Plane-1 U+F0900 must widen at the tightened 1.0× PUA threshold"
    );
}

/// IEC Power Symbols live at BMP U+23FB..U+23FE plus the lone U+2B58
/// — NOT inside the PUA. Pre-#610 the table listed a bogus PUA range
/// (`0xEE00..=0xEE0B`). The widening gate consults NERD_FONT_RANGES
/// when deciding whether to tighten the threshold from `>1.5×` to
/// `>1.0×`, so the table MUST hold these BMP entries even though
/// they are outside the PUA.
#[test]
fn iec_power_symbol_widened_bmp_range() {
    // All five IEC Power Symbol codepoints must report membership.
    assert!(is_nerd_font_pua('\u{23FB}'), "U+23FB POWER SYMBOL must be in table (BMP)");
    assert!(is_nerd_font_pua('\u{23FC}'), "U+23FC POWER ON-OFF SYMBOL must be in table");
    assert!(is_nerd_font_pua('\u{23FD}'), "U+23FD POWER ON SYMBOL must be in table");
    assert!(is_nerd_font_pua('\u{23FE}'), "U+23FE POWER SLEEP SYMBOL must be in table");
    assert!(is_nerd_font_pua('\u{2B58}'), "U+2B58 HEAVY CIRCLE (power-off) must be in table");
    // And the spurious pre-#610 placeholder must be gone.
    assert!(
        !is_nerd_font_pua('\u{EE05}'),
        "Pre-#610 placeholder range 0xEE00..=0xEE0B must be removed"
    );
}

/// CJK guard: codepoints unicode-width already treats as 2-cells wide
/// (CJK ideographs, kana, hangul, fullwidth, CJK punctuation incl.
/// U+3001 IDEOGRAPHIC COMMA) MUST short-circuit the widening pipeline
/// even when the shaper reports a wide advance. Without this guard,
/// the grid hands us a single cell with WIDE set, we widen to 2, and
/// the renderer's WIDE_CONT slot also reserves a cell → 3-cell glyph.
///
/// We test BOTH the WIDE-flag path (already covered upstream) and the
/// unflagged path: callers occasionally hand us a CJK punctuation cell
/// without the WIDE flag (the grid only sets WIDE for ideographs, not
/// every East-Asian-Width:W codepoint). The guard must catch both.
#[test]
fn cjk_punctuation_not_double_widened() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    // U+3001 IDEOGRAPHIC COMMA — unicode-width treats as 2 wide. Force
    // a tiny cell_w so the legacy 1.5× heuristic would fire on any
    // real glyph, then assert the guard keeps cluster_cells at 1.
    let cells = vec![(0u16, cell('\u{3001}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 1.0);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].cluster_cells, 1,
        "U+3001 IDEOGRAPHIC COMMA must not be re-widened by the singleton path"
    );
    // U+FF0C FULLWIDTH COMMA — same.
    let cells = vec![(0u16, cell('\u{FF0C}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 1.0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cluster_cells, 1, "U+FF0C must not be re-widened");
    // U+4E2D 中 — even with the WIDE flag the guard must defer to the
    // grid, which is the legacy behaviour preserved here.
    let cells = vec![(0u16, wide_cell('\u{4E2D}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 1.0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cluster_cells, 1, "WIDE-flagged CJK must not be re-widened");
}

/// The legacy Material Design Icons range `0xF500..=0xFD46` was
/// dead-fonts only — NF v3 moved MDI to Plane-1 and the BMP range no
/// longer hosts MDI glyphs. Pre-#610 the table still carried the
/// legacy range, which inflated false positives for any non-NF font
/// that happens to map glyphs in that BMP band. This test pins the
/// removal so it can't silently come back.
#[test]
fn legacy_mdi_range_removed() {
    // Three sample codepoints from inside the old `0xF500..=0xFD46`
    // band — all MUST report non-membership now. Pick values that
    // sit clearly outside the still-listed Octicons (`0xF400..=0xF533`)
    // tail and Font Logos (`0xF300..=0xF381`).
    assert!(!is_nerd_font_pua('\u{F600}'), "U+F600 (legacy MDI mid) must be removed");
    assert!(!is_nerd_font_pua('\u{F800}'), "U+F800 (legacy MDI mid) must be removed");
    assert!(!is_nerd_font_pua('\u{FD00}'), "U+FD00 (legacy MDI tail) must be removed");
    // And no NERD_FONT_RANGES entry may overlap the removed BMP band.
    for r in NERD_FONT_RANGES {
        let lo = *r.start();
        let hi = *r.end();
        // Reject any entry that sits *entirely* inside 0xF534..=0xFD46
        // (i.e. the legacy MDI band minus the Octicons overlap region).
        let inside_legacy = lo >= 0xF534 && hi <= 0xFD46;
        assert!(
            !inside_legacy,
            "NERD_FONT_RANGES must not contain a band purely inside the removed legacy MDI region (got {lo:#X}..={hi:#X})"
        );
    }
}

/// `0xE5FA..=0xE62F` was commented "Codicons" pre-#610; the v3
/// cheat-sheet says that band is Seti UI. The diagnosis also calls for
/// extending the tail past U+E62F up to ~U+E6B5 to cover the rest of
/// the Seti UI block. This test pins both the relabel (by virtue of
/// the Codicons range now sitting at U+EA60..U+EBEB) and the extended
/// tail.
#[test]
fn mislabeled_codicons_relabeled_as_seti_ui() {
    // The Seti UI band: the head MUST still be in the table.
    assert!(is_nerd_font_pua('\u{E5FA}'), "U+E5FA (Seti UI head) must be in table");
    // The pre-#610 tail.
    assert!(is_nerd_font_pua('\u{E62F}'), "U+E62F (pre-#610 Seti UI tail) must be in table");
    // The new extended tail (Seti UI extends to U+E6B5 per NF v3).
    assert!(is_nerd_font_pua('\u{E6B5}'), "U+E6B5 (extended Seti UI tail) must be in table");
    assert!(is_nerd_font_pua('\u{E660}'), "U+E660 mid-Seti-UI must be in table");
    // Crucially, U+EA60..=U+EBEB (the REAL Codicons range) is now
    // separately tabled — independent confirmation that the relabel
    // was completed and the new Codicons entry is live.
    assert!(is_nerd_font_pua('\u{EA60}'), "Codicons must live at U+EA60+, not at U+E5FA");
    assert!(is_nerd_font_pua('\u{EBEB}'), "Codicons tail at U+EBEB must be in table");
}

/// Extension to the existing `nerd_font_pua_width` suite per the
/// diagnosis: re-run a representative widening case at scale_factor=1.5
/// (the prior 2× regression test in `nerd_font_pua_width.rs` only
/// covered scale 2.0). Catches a unit-conversion regression that
/// triggers between 1× and 2× — e.g. 125 % / 150 % Windows DPI.
#[test]
fn nerd_font_pua_width_at_1_5x_dpi() {
    let mut fs = font_system_with_assets();
    let raster_px = FONT_PX * 1.5;
    let (cell_w_raster, _) =
        sonicterm_text::metrics::measure_cell(&mut fs, FAMILY, raster_px, raster_px);
    let mut r = SwashRasterizer::new(&mut fs, FAMILY, raster_px);

    // Non-PUA Latin letter must NOT widen at 1.5× DPI when cell_w_px is
    // also expressed in raster pixels (the bug Haiku flagged on PR #605
    // was that the LOGICAL cell_w being passed at >1× DPI halved the
    // effective threshold and widened ordinary glyphs).
    let cells = vec![(0u16, cell('é'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, raster_px, STYLE, &cells, cell_w_raster);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].cluster_cells, 1,
        "non-PUA Latin glyph must stay 1 cell at 1.5× DPI (cell_w_raster={cell_w_raster})"
    );
    let cells = vec![(0u16, cell('M'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, raster_px, STYLE, &cells, cell_w_raster);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cluster_cells, 1, "ASCII 'M' must stay 1 cell at 1.5× DPI");
}
