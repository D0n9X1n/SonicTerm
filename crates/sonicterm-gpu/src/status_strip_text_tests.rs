//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::status_strip_tests::*;
use super::*;

/// Contract: at the default `font_scale` strip text is exactly 80% of the live terminal font
/// size, scaled by DPI, never under the 1 pt FreeType draws at; the strip's stack is built at the
/// same size.
#[test]
fn strip_text_is_its_scale_of_the_terminal_font_size() {
    assert!((geometry_for_font(13.0, 1.0).text_px() - 10.4).abs() < 1e-4, "13 pt gives 10.4");
    assert!((geometry_for_font(13.5, 2.0).text_px() - 21.6).abs() < 1e-4, "13.5 pt at 2×");
    // A Cmd+= zoom changes the live size, and the strip follows it.
    assert!((geometry_for_font(16.0, 1.0).text_px() - 12.8).abs() < 1e-4, "a zoomed 16 pt");
    assert!((geometry_for_font(1.0, 1.0).text_px() - 1.0).abs() < 1e-4, "the 1 pt floor at 1×");
    assert!((status_line_font_size(13.0, default_font_scale()) - 10.4).abs() < 1e-4);
    assert!((status_line_font_size(13.5, default_font_scale()) - 10.8).abs() < 1e-4);
}

/// Capitals sit centered in the band area outside the hairline, within one raster pixel, at
/// either position and at 1× and 2×, on the bundled chrome font.
#[test]
fn capitals_are_centered_in_the_band_outside_the_hairline() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for position in [StatusPosition::Top, StatusPosition::Bottom] {
        for scale in [1.0, 2.0] {
            let error = capital_centering_error(position, scale);
            assert!(error.abs() <= 1.0, "{position:?} at {scale}×: off by {error} px");
        }
    }
}

/// The cap height centers the drawn capital's ink box: twice its center's height above the
/// baseline. Ink that sits on the baseline gives its top; ink a pixel below it (as Windows draws)
/// gives one less, so the center never shifts a whole pixel. Without a drawn capital the font's
/// own measurement is used, and without either, 0.7 × the text size.
#[test]
fn the_cap_height_centers_the_drawn_capital_and_falls_back_in_order() {
    assert_eq!(resolve_cap_height(Some((-8.0, 8.0)), Some(5.0), 10.0), 8.0, "ink on the baseline");
    assert_eq!(resolve_cap_height(Some((-8.0, 9.0)), Some(5.0), 10.0), 7.0, "ink below it");
    assert_eq!(resolve_cap_height(None, Some(5.0), 10.0), 5.0, "the font's own measurement");
    assert_eq!(resolve_cap_height(None, None, 10.0), 7.0, "no capital and no metric");
}

/// On the bundled font the strip centers on its drawn `H`: the kept cap height is twice the
/// drawn ink center's height above the baseline.
#[test]
fn the_strip_measures_its_drawn_capital() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stack = chrome_stack();
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(256, 256);
    let text_px = 13.0;
    let cap = strip_cap_height(&stack, &mut raster, &mut atlas, text_px);
    let (ink_h, _) =
        drawn_h(&stack, &mut GlyphAtlas::new(256, 256), GlyphRasterVariant::StatusLine, text_px);
    assert!(cap > 0.0 && cap <= ink_h, "cap {cap} vs drawn H {ink_h}");
}

/// The renderer keeps one cap height per text size: the same size reuses it without measuring,
/// and a new size measures again, so a zoom or DPI change never centers on a stale value.
#[test]
fn the_kept_cap_height_follows_the_text_size() {
    let measured = std::cell::Cell::new(0);
    let measure = |value: f32| {
        measured.set(measured.get() + 1);
        value
    };
    let kept = kept_cap_height(None, 13.0, || measure(9.0));
    assert_eq!((kept.1, measured.get()), (9.0, 1));
    let again = kept_cap_height(Some(kept), 13.0, || measure(99.0));
    assert_eq!((again.1, measured.get()), (9.0, 1), "the same size is not measured again");
    let zoomed = kept_cap_height(Some(again), 16.0, || measure(11.0));
    assert_eq!((zoomed.1, measured.get()), (11.0, 2), "a new size measures anew");
    let back = kept_cap_height(Some(zoomed), 13.0, || measure(9.0));
    assert_eq!((back.1, measured.get()), (9.0, 3), "never the other size's stale value");
}

/// Strip text is `font_scale` × the live terminal font size, 0.8 by default, with the same 1 pt
/// floor; the stack the renderer builds for a scale asks for the same size the geometry reports.
#[test]
fn strip_text_follows_its_font_scale() {
    assert!((default_font_scale() - 0.8).abs() < 1e-6);
    let mut geometry = geometry_for_font(13.0, 1.0);
    assert!((geometry.text_px() - 10.4).abs() < 1e-4, "0.8 × 13 pt by default");
    geometry.font_scale = 0.65;
    assert!((geometry.text_px() - 8.45).abs() < 1e-4);
    assert!((status_line_font_size(13.0, 0.65) - 8.45).abs() < 1e-4);
    assert!((status_line_font_size(1.0, 0.3) - 1.0).abs() < 1e-4, "never under 1 pt");
}
