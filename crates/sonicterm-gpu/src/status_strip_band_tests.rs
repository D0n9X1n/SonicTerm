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
    // Each size rasterizes under its own variant: footer tiles for the small text, tab-title tiles
    // for the band's full size, so neither can stand in for the other in the atlas.
    let variants: std::collections::HashSet<_> =
        atlas.resident_tile_keys().into_iter().map(|key| key.raster_variant).collect();
    assert!(variants.contains(&GlyphRasterVariant::PaletteFooter), "{variants:?}");
    assert!(variants.contains(&GlyphRasterVariant::TabTitle), "{variants:?}");
    let ink = |index: usize| crate::cursor::ndc_rect_px(glyphs[index].rect, 400.0, 200.0);
    let ((_, small_top, _, small_height), (_, full_top, _, full_height)) = (ink(0), ink(1));
    assert!(small_height < full_height, "small {small_height} vs full {full_height}");
    let (small_bottom, full_bottom) = (small_top + small_height, full_top + full_height);
    assert!((small_bottom - full_bottom).abs() <= 1.0, "baselines {small_bottom} {full_bottom}");
}

/// A caption band draws its quiet text in the band name color, which reads at 4.5:1; an unfocused
/// window's band draws values, the centred window name among them, in it too, as Windows dims an
/// inactive title. A band without caption buttons (macOS traffic lights, full screen) and an
/// ordinary strip keep their colors.
#[test]
fn an_unfocused_caption_band_dims_its_values_to_the_name_color() {
    use sonicterm_render_model::boundary::ui::window_band::BandButtons;
    let theme = Theme::default();
    let palette = sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(&theme);
    let (active, inactive) = (ChromeColor::WHITE, hex_to_chrome_color("#808080"));
    let colors = StatusColors::resolve(&theme, active, inactive, &palette);
    let dimmed = colors.for_band_focus(BandButtons::Caption, true, false);
    assert_eq!(
        dimmed.value, colors.band_name,
        "unfocused caption band: values take the name color"
    );
    assert_eq!(dimmed.name, colors.band_name);
    assert_eq!(dimmed.warn, colors.warn, "cautions keep their own color");
    let focused = colors.for_band_focus(BandButtons::Caption, true, true);
    assert_eq!((focused.value, focused.name), (colors.value, colors.band_name), "focused");
    assert_eq!(colors.for_band_focus(BandButtons::TrafficLights, true, false), colors, "macOS");
    assert_eq!(colors.for_band_focus(BandButtons::None, true, false), colors, "full screen");
    assert_eq!(colors.for_band_focus(BandButtons::Caption, false, false), colors, "strip below");
}

/// Every bundled theme's caption band name color reads at 4.5:1 on the band, the WCAG AA text
/// floor, whether the theme's inactive tab color does or falls back to quieter foreground text.
#[test]
fn every_bundled_theme_band_name_reads_at_four_and_a_half_to_one() {
    use sonicterm_render_model::boundary::ui::ui_tokens::UiPalette;
    let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    let mut checked = 0;
    for entry in std::fs::read_dir(&themes).expect("bundled themes") {
        let path = entry.expect("theme entry").path();
        let theme = Theme::load_strict(&path).expect("bundled theme parses");
        let palette = UiPalette::from_theme(&theme);
        let active = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        let inactive = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        let colors = StatusColors::resolve(&theme, active, inactive, &palette);
        let ratio = contrast_ratio(colors.band_name, colors.background);
        assert!(ratio >= BAND_TEXT_CONTRAST, "{}: {ratio}", path.display());
        checked += 1;
    }
    assert_eq!(checked, 9, "the nine bundled themes");
}

/// The band's quiet text never takes an unchecked color: a custom solarized-dark whose
/// `bright.white` is its own bar color still gets a band name that reads at 4.5:1 (its active
/// tab color), and a theme where every palette text fails gets black or white.
#[test]
fn band_name_fallbacks_are_checked() {
    use sonicterm_render_model::boundary::cfg::theme::Hex;
    use sonicterm_render_model::boundary::ui::ui_tokens::UiPalette;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/themes/solarized-dark.toml");
    let mut theme = Theme::load_strict(&path).expect("bundled solarized-dark parses");
    theme.colors.bright.white = theme.colors.tab.bar_bg.clone();
    let palette = UiPalette::from_theme(&theme);
    let active = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
    let inactive = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
    let colors = StatusColors::resolve(&theme, active, inactive, &palette);
    let ratio = contrast_ratio(colors.band_name, colors.background);
    assert!(ratio >= BAND_TEXT_CONTRAST, "custom solarized band name: {ratio}");
    let grey = Hex("#777777".into());
    let mut flat = Theme::default();
    flat.colors.background = grey.clone();
    flat.colors.foreground = grey.clone();
    flat.colors.bright.white = grey.clone();
    flat.colors.tab.active_fg = grey.clone();
    flat.colors.tab.inactive_fg = grey.clone();
    flat.colors.tab.bar_bg = grey.clone();
    flat.colors.ansi.black = grey;
    let palette = UiPalette::from_theme(&flat);
    let text = hex_to_chrome_color("#777777");
    let colors = StatusColors::resolve(&flat, text, text, &palette);
    let ratio = contrast_ratio(colors.band_name, colors.background);
    assert!(ratio >= BAND_TEXT_CONTRAST, "flat theme band name: {ratio}");
}

/// Text in the integrated titlebar band, a name band or a top strip merged into it, is the tab
/// titles' size on their raster variant whatever `[status_line] font_scale` is, so the titlebar
/// reads like the tab bar; an ordinary strip keeps `font_scale` on its own variant.
#[test]
fn band_text_matches_the_tab_titles_whatever_the_font_scale() {
    use sonicterm_render_model::boundary::ui::tab_spans::tab_title_font_size;
    for (scale, font_scale) in [(1.0, 0.85), (2.0, 0.85), (2.0, 1.0), (1.5, 1.2)] {
        let ordinary = StripGeometry { font_scale, ..geometry_for_font(13.0, scale) };
        let band = StripGeometry {
            band: StripBand { in_band: true, left_inset: 0.0, right_inset: 0.0 },
            ..ordinary
        };
        assert_eq!(band.text_px(), tab_title_font_size(13.0) * scale, "{scale} × {font_scale}");
        assert_eq!(band.raster_variant(), GlyphRasterVariant::TabTitle);
        assert_eq!(band.chrome_stack(), crate::chrome_cache::ChromeStack::TabTitle);
        assert_eq!(
            ordinary.text_px(),
            status_line_font_size(13.0, font_scale) * scale,
            "an ordinary strip scales"
        );
        assert_eq!(ordinary.raster_variant(), GlyphRasterVariant::StatusLine);
        assert_eq!(ordinary.chrome_stack(), crate::chrome_cache::ChromeStack::StatusLine);
    }
}
