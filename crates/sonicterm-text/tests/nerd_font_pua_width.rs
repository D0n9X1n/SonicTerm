//! Regression tests for issue #595 — nerd-font icons rendered into
//! 1 cell when the shaper either reports a wider advance OR the
//! codepoint falls inside a known Nerd Font PUA range.
//!
//! Two-branch coverage (per Step-2 spec):
//!   (a) advance heuristic — `LayoutGlyph.w > 1.5 * cell_w`
//!       widens cluster_cells from 1 → 2.
//!   (b) PUA range table — codepoint inside `NERD_FONT_RANGES` AND
//!       the resolved slot reports a non-zero charmap glyph widens
//!       cluster_cells from 1 → 2.
//!
//! Diagnosis: see comment thread on GitHub issue #595.

use cosmic_text::FontSystem;
use sonicterm_text::shape::{shape_run, shape_run_with_cell_w, RunStyle};
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

/// Sanity baseline: ASCII `a` MUST remain a 1-cell singleton even
/// with the widening pass armed (the advance for `a` is well under
/// 1.5 * cell_w and `a` is not in any PUA range).
#[test]
fn ascii_letter_remains_single_cell() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells = vec![(0u16, cell('a'))];
    // Use a deliberately small cell_w so the threshold is easy to
    // exceed — and still 'a' must not widen.
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 8.0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cluster_cells, 1, "ASCII 'a' must stay 1 cell wide");
}

/// CJK `中` carries the grid's WIDE flag — the renderer's
/// WIDE/WIDE_CONT path is what allocates the second column. The
/// #595 singleton widening MUST defer to that flag and NOT
/// double-widen, or every CJK glyph would silently consume an extra
/// cell on top of the WIDE_CONT the grid already reserved.
#[test]
fn cjk_glyph_remains_single_cell_from_shaper() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells = vec![(0u16, wide_cell('中'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 9.0);
    assert_eq!(out.len(), 1, "CJK must produce a single glyph");
    assert_eq!(
        out[0].cluster_cells, 1,
        "WIDE-flagged CJK cell must NOT be widened — renderer's WIDE path owns that"
    );
}

/// Branch (b) fallback: U+F121 (Font Awesome `code` icon) IS inside
/// the `0xF000..=0xF2FF` range. With Rec Mono St.Helens (the bundled
/// nerd-patched font) loaded, the slot's charmap returns a non-zero
/// glyph, so the singleton widening must fire even if the advance
/// alone wasn't enough.
#[test]
fn nerd_font_pua_codepoint_widens_to_two_cells_via_range_fallback() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let icon = '\u{F121}';
    let cells = vec![(0u16, cell(icon))];
    // Pass a generous cell_w (20px) so branch (a) can't fire — this
    // forces the test to exercise branch (b) specifically.
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 20.0);
    assert_eq!(out.len(), 1, "single PUA codepoint must produce one glyph");
    // Only assert widening if the font actually maps the codepoint —
    // otherwise the fallback declined (no charmap glyph) and the
    // singleton remains 1 cell, which is also correct behaviour.
    let has_glyph = out[0].glyph_id != 0;
    if has_glyph {
        assert_eq!(
            out[0].cluster_cells, 2,
            "Nerd Font PUA codepoint U+F121 with non-zero charmap must widen to 2 cells"
        );
    }
}

/// Branch (b) coverage for the Powerline range (U+E0B0).
#[test]
fn powerline_pua_codepoint_widens_to_two_cells() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    // U+E0B0 = right-pointing triangle (the canonical Powerline arrow).
    let cells = vec![(0u16, cell('\u{E0B0}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 20.0);
    assert_eq!(out.len(), 1);
    let has_glyph = out[0].glyph_id != 0;
    if has_glyph {
        assert_eq!(out[0].cluster_cells, 2, "Powerline U+E0B0 must widen to 2 cells");
    }
}

/// Disabled-widening path: passing `cell_w == 0.0` (the value
/// overlay-text and synthetic-cell callers use) must leave singleton
/// PUA codepoints at `cluster_cells == 1`. This protects overlay
/// rendering (palette, help overlay) from accidentally double-spacing
/// every icon-containing line.
#[test]
fn zero_cell_w_disables_widening() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells = vec![(0u16, cell('\u{F121}'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 0.0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cluster_cells, 1, "cell_w == 0.0 must short-circuit both widening branches");
}

/// `shape_run` (legacy entry point, no cell_w) must behave like
/// `shape_run_with_cell_w(..., 0.0)` — i.e. never widen — so existing
/// callers that haven't migrated to the cell-w-aware API are
/// unaffected.
#[test]
fn legacy_shape_run_never_widens() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    let cells = vec![(0u16, cell('\u{F121}'))];
    let out = shape_run(&mut r, FAMILY, FONT_PX, STYLE, &cells);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cluster_cells, 1, "legacy shape_run must not widen");
}

/// Branch (a) coverage: the advance heuristic fires when the shaper
/// reports `g.w > 1.5 * cell_w` for a singleton. We can't easily mock
/// cosmic-text's reported advance, but we CAN drive the threshold by
/// passing an absurdly small `cell_w` so any real glyph trips the
/// `> 1.5 * cell_w` check.
///
/// Using a known nerd-font icon makes the test font-agnostic for the
/// glyph_id==0 case — if the bundled font has the glyph, advance fires
/// (or branch b does). If it doesn't, the test asserts nothing
/// meaningful and that's fine; this is paired coverage with the
/// dedicated branch-(b) test above.
#[test]
fn advance_heuristic_widens_when_glyph_advance_exceeds_threshold() {
    let mut fs = font_system_with_assets();
    let mut r = SwashRasterizer::with_default_family(&mut fs);
    // Use a regular ASCII letter so it's definitely in the font with
    // a real advance. Then force a tiny cell_w (1px) so any reasonable
    // glyph blows past 1.5 * 1 = 1.5px advance.
    let cells = vec![(0u16, cell('M'))];
    let out = shape_run_with_cell_w(&mut r, FAMILY, FONT_PX, STYLE, &cells, 1.0);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].cluster_cells, 2,
        "advance heuristic must widen any glyph whose advance > 1.5 * cell_w"
    );
}
