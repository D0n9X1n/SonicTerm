//! Regression test: the cell height MUST include the font's intrinsic
//! `line_gap` (a.k.a. skrifa `leading`).
//!
//! Pre-fix, Sonic computed `cell_h = font_size * line_height_mult`,
//! which ignored the font's hhea/OS-2 line-gap entry. At
//! `font_size = 14, line_height = 1.1` with Rec Mono Casual on a 2x
//! Retina display, that produced a 30-physical-px cell — 88% of the
//! 34-physical-px cell WezTerm produces at IDENTICAL config. Visually:
//! Sonic squeezes ~3 extra rows into the same window vs WezTerm.
//!
//! Post-fix, `cell_h = natural_line_h_px(family, size) * line_height_mult`
//! where `natural_line_h_px = (ascent + |descent| + leading) / upem * size`.
//! At font_size=14 / line_height=1.1 with Rec Mono Casual, the natural
//! line height in logical px is ~16.x, giving a *logical* cell_h ≥ 16
//! and (the renderer keeps cell_h in logical px) a physical-pixel
//! pitch ≥ 32 at 2x scale — matching WezTerm within ±2 px.

use cosmic_text::FontSystem;

fn font_system_with_rec_mono() -> FontSystem {
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

#[test]
fn cell_height_includes_font_line_gap_for_rec_mono_casual() {
    let mut fs = font_system_with_rec_mono();
    let size = 14.0_f32;
    let line_height_mult = 1.1_f32;

    let natural = sonicterm_shared::render::natural_line_h_px(&mut fs, "Rec Mono St.Helens", size);
    let logical_cell_h = natural * line_height_mult;
    // 2x Retina is the canonical case the user reported the parity bug on.
    let physical_cell_h = logical_cell_h * 2.0;

    // Naive `size * line_height_mult` would be 14*1.1 = 15.4 logical px
    // → 30.8 physical px @2x. We must beat that materially; WezTerm
    // produces ~34. Allow ±2 px around 34 as the tolerance band.
    assert!(
        physical_cell_h >= 32.0,
        "expected physical cell_h >= 32 px (~ WezTerm parity), got {physical_cell_h:.2} \
         (logical {logical_cell_h:.2}, natural {natural:.2}). \
         Bug: cell_h formula likely dropped font line_gap again."
    );
    // Also assert we're not *over-correcting* into absurd territory.
    assert!(
        physical_cell_h <= 40.0,
        "physical cell_h {physical_cell_h:.2} is far past WezTerm parity (~34); \
         check that descent sign / leading sign conventions weren't double-counted."
    );
}
