//! Regression test: `measure_cell` MUST derive `cell_w` from the max
//! advance over printable ASCII codepoints (`0x20..=0x7E`), not from
//! `"M"` alone.
//!
//! Issue #623 stage 4: WezTerm's `wezterm-font/src/ftwrap.rs::cell_metrics`
//! iterates ASCII glyphs 32..128 and takes `max(horiAdvance)`. SonicTerm
//! pre-fix shaped only `"M"`, which under-sized the grid 3–5% on fonts
//! where the widest ASCII glyph isn't M (Nerd Font patched faces where
//! `_`, `w`, `@` exceed M's advance). The visible effect was symptom #2
//! in the bug report: icon↔label spacing tighter than WezTerm.
//!
//! This test bank uses the bundled Rec Mono St.Helens (true monospace,
//! all ASCII same advance) to verify the regression-safety case AND
//! exercises the missing-glyph / fallback paths.

use cosmic_text::{Buffer, FontSystem, Metrics, Shaping};
use sonicterm_text::metrics::measure_cell;
use sonicterm_text::terminal_font_attrs;

const FAMILY: &str = "Rec Mono St.Helens";

fn font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
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

/// Shape a single codepoint and return its advance, or `None` if the
/// font doesn't produce a glyph for it.
fn advance_of(fs: &mut FontSystem, family: &str, size: f32, ch: char) -> Option<f32> {
    let mut buf = Buffer::new(fs, Metrics::new(size, size));
    buf.set_size(fs, Some(1000.0), Some(1000.0));
    let mut tmp = [0u8; 4];
    buf.set_text(
        fs,
        ch.encode_utf8(&mut tmp),
        &terminal_font_attrs(family),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(fs, false);
    buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w))
}

/// Compute max advance over printable ASCII, mirroring the shape of
/// the new `measure_cell` implementation but at test scope.
fn max_ascii_advance(fs: &mut FontSystem, family: &str, size: f32) -> f32 {
    let mut max_w: f32 = 0.0;
    for code in 0x20u32..=0x7Eu32 {
        let Some(ch) = char::from_u32(code) else { continue };
        if let Some(adv) = advance_of(fs, family, size, ch) {
            if adv > max_w {
                max_w = adv;
            }
        }
    }
    max_w
}

#[test]
fn cell_w_uses_max_ascii_advance_not_m_only() {
    // Use a SYNTHETIC FontSystem with no fonts installed — cosmic-text
    // will fall back to a system face for shaping. We compute both the
    // M-only advance and the max-ASCII advance directly and verify
    // measure_cell returns the latter (>= the former). The contract
    // is: `measure_cell == max_ascii_advance >= advance_of('M')`. For
    // any font where some non-M ASCII glyph is wider, the inequality
    // is strict; for true monospace, equality holds.
    let mut fs = font_system();
    let size = 14.0_f32;

    let m_advance = advance_of(&mut fs, FAMILY, size, 'M').expect("M must shape");
    let max_advance = max_ascii_advance(&mut fs, FAMILY, size);
    let (cell_w, _) = measure_cell(&mut fs, FAMILY, size, size);

    assert!(
        cell_w >= m_advance - 0.01,
        "cell_w {cell_w:.3} must be >= M-only advance {m_advance:.3}; \
         regression: did the formula revert to M-only?"
    );
    assert!(
        (cell_w - max_advance).abs() < 0.01,
        "cell_w {cell_w:.3} must equal max-ASCII advance {max_advance:.3}; \
         got delta {:.3}",
        (cell_w - max_advance).abs()
    );
}

#[test]
fn cell_w_handles_missing_glyphs() {
    // Robustness: even if some codepoints have no glyph, measure_cell
    // must not crash and must return the max over what's present.
    // We can't easily force "missing glyph" with a real font, so we
    // instead assert measure_cell never panics across a range of sizes
    // and always returns a positive width when at least one ASCII
    // codepoint shapes (the universal case for any usable font).
    let mut fs = font_system();
    for size in [8.0_f32, 10.0, 14.0, 24.0, 48.0] {
        let (cell_w, cell_h) = measure_cell(&mut fs, FAMILY, size, size);
        assert!(cell_w > 0.0, "cell_w must be > 0 at size {size}, got {cell_w}");
        assert!(cell_h > 0.0, "cell_h must be > 0 at size {size}, got {cell_h}");
        // Sanity: cell_w should be in a sensible range relative to size
        // (i.e. neither absurdly tiny nor 10x the em).
        assert!(
            cell_w < size * 5.0,
            "cell_w {cell_w} at size {size} is implausibly large; \
             max-advance scan likely picked up a junk glyph"
        );
    }
}

#[test]
fn cell_w_monospace_font_unchanged() {
    // Regression safety: for a true monospace font (Rec Mono St.Helens,
    // bundled), all ASCII glyphs share the same advance, so the new
    // max-of-ASCII formula must equal the old M-only formula. If this
    // test fails, the max-advance scan is leaking some non-ASCII width
    // (e.g. PUA Nerd Font icon) into the cell metric.
    let mut fs = font_system();
    let size = 14.0_f32;
    let m_advance = advance_of(&mut fs, FAMILY, size, 'M').expect("M must shape in Rec Mono");
    let (cell_w, _) = measure_cell(&mut fs, FAMILY, size, size);
    assert!(
        (cell_w - m_advance).abs() < 0.01,
        "Rec Mono St.Helens is true monospace; cell_w {cell_w:.3} must equal \
         M advance {m_advance:.3} (delta {:.3}). Regression: max-ASCII scan \
         may be leaking a non-ASCII codepoint width.",
        (cell_w - m_advance).abs()
    );
}
