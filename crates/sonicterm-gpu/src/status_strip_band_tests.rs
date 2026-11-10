//! The integrated titlebar band: button height, merging with the strip, the centre line, and
//! the small side text a band with its buttons on the right draws.

use super::status_strip_tests::*;
use super::*;

/// The band's button height is whole and even at every scale, so its midline is a whole pixel.
#[test]
fn the_band_button_height_is_an_even_number_of_raster_pixels() {
    for (logical, scale, raster) in
        [(28.0, 1.0, 28.0), (28.0, 1.5, 42.0), (28.0, 2.0, 56.0), (27.0, 1.5, 40.0)]
    {
        assert_eq!(band_button_px(logical, scale), raster, "{logical} × {scale}");
    }
}

/// Merged at the top, the strip is the band, at least the buttons' height, and reserves it through
/// its own placement; otherwise a separate name band reserves its height above the panes and the
/// strip keeps its own. A native window has no band.
#[test]
fn the_band_merges_with_a_top_strip_and_stands_alone_otherwise() {
    let merged = band_layout(true, 56.0, true, StatusPosition::Top, 46.0);
    assert_eq!(merged, BandLayout { merged: true, separate_px: 0.0, strip_px: 56.0 });
    let tall = band_layout(true, 56.0, true, StatusPosition::Top, 70.0);
    assert_eq!(tall.strip_px, 70.0, "a taller strip sets the merged band's height");
    let bottom = band_layout(true, 56.0, true, StatusPosition::Bottom, 46.0);
    assert_eq!(bottom, BandLayout { merged: false, separate_px: 56.0, strip_px: 46.0 });
    let off = band_layout(true, 56.0, false, StatusPosition::Top, 46.0);
    assert_eq!(off, BandLayout { merged: false, separate_px: 56.0, strip_px: 0.0 });
    let large_font = band_layout(true, 56.0, false, StatusPosition::Top, 70.0);
    assert_eq!(large_font.separate_px, 70.0, "a name-only band grows with its font");
    let full_screen = band_layout(true, 0.0, false, StatusPosition::Top, 46.0);
    assert_eq!(full_screen.separate_px, 0.0, "full screen with the strip off has no band");
    let full_screen_bottom = band_layout(true, 0.0, true, StatusPosition::Bottom, 46.0);
    assert_eq!(full_screen_bottom.separate_px, 0.0, "nor with the strip at the bottom");
    let full_screen_top = band_layout(true, 0.0, true, StatusPosition::Top, 46.0);
    assert_eq!(full_screen_top.strip_px, 46.0, "a top strip is just the strip");
    let native = band_layout(false, 56.0, true, StatusPosition::Top, 46.0);
    assert_eq!(native, BandLayout { merged: false, separate_px: 0.0, strip_px: 46.0 });
}

/// In an integrated band the capitals centre on the whole band, which is the window buttons'
/// centre line: the cap centre is exactly half the band at every scale and band height, merged or
/// name-only, with no hairline offset.
#[test]
fn text_in_the_band_centres_on_the_button_centre_line() {
    for (scale, band_px) in [(1.0, 28.0), (1.5, 42.0), (2.0, 56.0), (2.0, 80.0)] {
        let geometry = StripGeometry {
            top: 0.0,
            height: band_px,
            position: StatusPosition::Top,
            band: StripBand { in_band: true, left_inset: 0.0, right_inset: 0.0 },
            ..geometry_for_font(13.0, scale)
        };
        let baseline = strip_baseline(&geometry);
        let cap_centre = baseline - geometry.cap_height / 2.0;
        assert_eq!(cap_centre, band_px / 2.0, "scale {scale}, band {band_px}");
    }
    // An ordinary top strip still centres in the area above its hairline.
    let ordinary = StripGeometry { position: StatusPosition::Top, ..geometry_for_font(13.0, 2.0) };
    let centre = strip_baseline(&ordinary) - ordinary.cap_height / 2.0;
    assert!(centre < ordinary.height / 2.0);
}

/// A small segment is measured in the small text when one is given, so it is narrower than the
/// same text at the strip's size; without small text both keep the strip's size.
#[test]
fn small_segments_use_the_small_text() {
    use sonicterm_render_model::boundary::cfg::status_line::{StatusSegmentId, StatusSide};
    use sonicterm_render_model::boundary::ui::status_line::{
        SpokenSegment, StatusPart, StatusSegmentView,
    };
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stack = super::status_strip_tests::chrome_stack();
    let mut raster = stack.clone();
    let small_stack = stack.with_font_size(10.0);
    let mut small_raster = small_stack.clone();
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let segment = |id, small| StatusSegmentView {
        id,
        side: StatusSide::Left,
        priority: 500,
        protected: false,
        parts: vec![StatusPart::new("Mem: 261 MB", StatusTone::Name)],
        compact: None,
        compact_last: false,
        small,
        click: None,
        spoken: SpokenSegment::default(),
    };
    let view = StatusLineView::new(vec![
        segment(StatusSegmentId::Mem, true),
        segment(StatusSegmentId::Clip, false),
    ]);
    let geometry = StripGeometry {
        top: 0.0,
        height: 40.0,
        position: StatusPosition::Top,
        screen: (1200.0, 200.0),
        scale_factor: 1.0,
        font_raster_px: 26.0,
        font_scale: default_font_scale(),
        cap_height: 7.0,
        band: StripBand::default(),
    };
    let width_of = |fitted: &FittedStrip, index| {
        fitted.segments.iter().find(|placed| placed.index == index).map(|placed| placed.width)
    };
    let small =
        SmallText { font_stack: &small_stack, rasterizer: &mut small_raster, text_px: 10.0 };
    let fitted = fit_strip(&view, &geometry, &stack, &mut raster, &mut atlas, Some(small));
    let (small_width, full_width) = (width_of(&fitted, 0).unwrap(), width_of(&fitted, 1).unwrap());
    assert!(small_width < full_width, "small {small_width} vs full {full_width}");
    assert!(fitted.segments.iter().any(|placed| placed.small), "the flag reaches the placement");
    let plain = fit_strip(&view, &geometry, &stack, &mut raster, &mut atlas, None);
    assert_eq!(width_of(&plain, 0), width_of(&plain, 1), "no small text: both at the strip size");
}

/// A small segment is drawn in the small text, on the strip's one baseline: its capital is shorter
/// than a full-size segment's and both sit on the same line, so the side text lines up with the
/// name.
#[test]
fn small_segments_draw_smaller_on_the_names_baseline() {
    use sonicterm_render_model::boundary::ui::ui_tokens::UiPalette;
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stack = chrome_stack();
    let mut raster = stack.clone();
    let small_stack = stack.with_font_size(10.0);
    let mut small_raster = small_stack.clone();
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let geometry = StripGeometry {
        top: 0.0,
        height: 40.0,
        position: StatusPosition::Top,
        screen: (400.0, 200.0),
        scale_factor: 1.0,
        font_raster_px: 26.0,
        font_scale: default_font_scale(),
        cap_height: 7.0,
        band: StripBand { in_band: true, left_inset: 0.0, right_inset: 0.0 },
    };
    let segment = |left: f32, small: bool| PlacedSegment {
        index: 0,
        left,
        width: 40.0,
        parts: vec![PlacedPart { text: "H".into(), tone: StatusTone::Name, left, width: 40.0 }],
        small,
    };
    let placed = vec![segment(10.0, true), segment(200.0, false)];
    let theme = sonicterm_render_model::boundary::cfg::theme::Theme::default();
    let palette = UiPalette::from_theme(&theme);
    let colors = StatusColors::resolve(&theme, ChromeColor::WHITE, ChromeColor::WHITE, &palette);
    let (mut quads, mut glyphs) = (Vec::new(), Vec::new());
    emit_strip(
        &placed,
        &[],
        &geometry,
        &colors,
        Some(&stack),
        Some(&mut raster),
        Some(SmallText { font_stack: &small_stack, rasterizer: &mut small_raster, text_px: 10.0 }),
        &mut atlas,
        &mut crate::chrome_cache::ChromeRunCache::default(),
        true,
        &mut quads,
        &mut glyphs,
    );
    assert_eq!(glyphs.len(), 2, "one capital per segment");
    // Each size rasterizes under its own variant: footer tiles for the small text, strip tiles for
    // the full size, so neither can stand in for the other in the atlas.
    let variants: std::collections::HashSet<_> =
        atlas.resident_tile_keys().into_iter().map(|key| key.raster_variant).collect();
    assert!(variants.contains(&GlyphRasterVariant::PaletteFooter), "{variants:?}");
    assert!(variants.contains(&GlyphRasterVariant::StatusLine), "{variants:?}");
    let ink = |index: usize| crate::cursor::ndc_rect_px(glyphs[index].rect, 400.0, 200.0);
    let ((_, small_top, _, small_height), (_, full_top, _, full_height)) = (ink(0), ink(1));
    assert!(small_height < full_height, "small {small_height} vs full {full_height}");
    let (small_bottom, full_bottom) = (small_top + small_height, full_top + full_height);
    assert!((small_bottom - full_bottom).abs() <= 1.0, "baselines {small_bottom} {full_bottom}");
}
