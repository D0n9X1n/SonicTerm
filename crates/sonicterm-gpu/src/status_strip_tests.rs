use super::*;

/// Linear-sRGB opaque black and white backgrounds.
const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const WHITE_BG: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

#[test]
fn height_is_sixty_percent_of_the_tab_bar_with_a_floor_and_scales() {
    // 0.6 × the tab bar (font × 2 + 12, 36 px floor), at least 20 logical px, scaled by DPI.
    assert_eq!(status_line_height(13.0, 1.0), 23.0);
    assert_eq!(status_line_height(13.0, 2.0), 46.0);
    assert_eq!(status_line_height(20.0, 1.0), 31.0, "follows font zoom");
    for font_size in [1.0, 6.0, 9.0, 13.0, 24.0] {
        assert!(status_line_height(font_size, 1.0) >= 20.0, "{font_size}");
        assert!(status_line_height(font_size, 1.0) < tab_bar_height(font_size), "{font_size}");
    }
}

#[test]
fn contrast_ratio_matches_wcag_extremes() {
    // Black on white is 21:1 and a color against itself is 1:1.
    assert!((contrast_ratio(ChromeColor::rgb(0, 0, 0), WHITE_BG) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(ChromeColor::WHITE, WHITE_BG) - 1.0).abs() < 0.01);
}

#[test]
fn caution_colors_fall_back_below_three_to_one() {
    // A yellow that fails 3:1 on a light strip falls back to the foreground; one that passes is kept.
    let yellow = ChromeColor::rgb(0xF0, 0xE0, 0x40);
    let foreground = ChromeColor::rgb(0x20, 0x20, 0x20);
    assert_eq!(readable_or(yellow, foreground, WHITE_BG), foreground);
    assert_eq!(readable_or(yellow, foreground, BLACK), yellow);
}

#[test]
fn segment_hit_test_uses_half_open_bounds() {
    // A press inside a segment's bounds finds it; the gap between segments finds nothing.
    use sonicterm_render_model::boundary::cfg::status_line::StatusSegmentId;
    let presented = [
        PresentedStatusSegment { id: StatusSegmentId::Mode, x: 10.0, width: 20.0, click: None },
        PresentedStatusSegment { id: StatusSegmentId::Mem, x: 50.0, width: 30.0, click: None },
    ];
    assert_eq!(segment_at(&presented, 10.0).map(|segment| segment.id), Some(StatusSegmentId::Mode));
    assert_eq!(segment_at(&presented, 30.0), None);
    assert_eq!(segment_at(&presented, 79.9).map(|segment| segment.id), Some(StatusSegmentId::Mem));
    assert_eq!(segment_at(&presented, 5.0), None);
}

/// A packaged chrome font stack for drawing tests.
fn chrome_stack() -> sonicterm_engine::FontStack {
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![assets],
        14.0,
        96,
        1.0,
    )
    .unwrap()
}

/// Tall text (CJK, emoji, a combining stack and a missing glyph) drawn at a size larger than the
/// band leaves no glyph or box ink outside the band, so the strip needs no damage ink padding.
#[test]
fn strip_ink_never_leaves_its_band() {
    use sonicterm_render_model::boundary::ui::status_line::PlacedPart;
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().unwrap();
    let stack = chrome_stack();
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let geometry = StripGeometry {
        top: 100.0,
        height: 12.0,
        screen: (400.0, 200.0),
        scale_factor: 1.0,
        title_raster_px: 30.0,
    };
    let placed = vec![PlacedSegment {
        index: 0,
        left: 10.0,
        width: 300.0,
        parts: vec![PlacedPart {
            text: "Mem: 漢字 👨‍👩‍👧 e\u{301}\u{301}\u{301} \u{10FFFD} ⚠".into(),
            tone: StatusTone::Chip,
            left: 10.0,
            width: 300.0,
        }],
    }];
    let theme = sonicterm_render_model::boundary::cfg::theme::Theme::default();
    let palette = sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(&theme);
    let colors = StatusColors::resolve(&theme, ChromeColor::WHITE, ChromeColor::WHITE, &palette);
    let (mut quads, mut glyphs) = (Vec::new(), Vec::new());
    emit_strip(
        &placed,
        &geometry,
        &colors,
        Some(&stack),
        Some(&mut raster),
        &mut atlas,
        &mut quads,
        &mut glyphs,
    );
    assert!(!glyphs.is_empty(), "the clip keeps partly visible glyphs");
    let band = (geometry.top, geometry.top + geometry.height);
    for rect in glyphs.iter().map(|glyph| glyph.rect).chain(quads.iter().map(|quad| quad.rect)) {
        let (_, top, _, height) = crate::cursor::ndc_rect_px(rect, 400.0, 200.0);
        assert!(
            top >= band.0 - 0.5 && top + height <= band.1 + 0.5,
            "ink {top}..{} outside {band:?}",
            top + height
        );
    }
}

/// A light fixture theme (every bundled theme is dark) whose yellow fails 3:1 on the strip.
fn light_fixture() -> sonicterm_render_model::boundary::cfg::theme::Theme {
    use sonicterm_render_model::boundary::cfg::theme::{Appearance, Hex};
    let mut theme = sonicterm_render_model::boundary::cfg::theme::Theme {
        appearance: Appearance::Light,
        ..Default::default()
    };
    theme.colors.background = Hex("#fafafa".into());
    theme.colors.foreground = Hex("#202020".into());
    theme.colors.tab.active_fg = Hex("#1a1a1a".into());
    theme.colors.tab.inactive_fg = Hex("#555555".into());
    theme.colors.ansi.yellow = Hex("#f0e040".into());
    theme.colors.ansi.red = Hex("#b00020".into());
    theme
}

/// On the default theme and a light fixture, values, names, cautions and failures each reach
/// 3:1 against `bg_base`; the light fixture's failing yellow falls back to the foreground.
#[test]
fn strip_colors_reach_three_to_one_on_dark_and_light_themes() {
    use sonicterm_render_model::boundary::ui::ui_tokens::UiPalette;
    for (name, theme) in [
        ("default", sonicterm_render_model::boundary::cfg::theme::Theme::default()),
        ("light", light_fixture()),
    ] {
        let palette = UiPalette::from_theme(&theme);
        let active = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        let inactive = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        let colors = StatusColors::resolve(&theme, active, inactive, &palette);
        for tone in [StatusTone::Value, StatusTone::Name, StatusTone::Warn, StatusTone::Error] {
            let ratio = contrast_ratio(colors.text(tone), colors.background);
            assert!(ratio >= MIN_CONTRAST, "{name} {tone:?}: {ratio}");
        }
    }
    let light = light_fixture();
    let palette = UiPalette::from_theme(&light);
    let yellow = hex_to_chrome_color(light.colors.ansi.yellow.0.as_str());
    assert!(contrast_ratio(yellow, palette.bg_base) < MIN_CONTRAST, "fixture yellow must fail");
    let colors = StatusColors::resolve(&light, ChromeColor::WHITE, ChromeColor::WHITE, &palette);
    assert_eq!(colors.warn, hex_to_chrome_color("#202020"), "warn falls back to the foreground");
    assert_eq!(colors.error, hex_to_chrome_color("#b00020"), "a passing red is kept");
}

/// The focus ring is four edges inside the focused segment's box, filling the band's height; it is
/// at least one pixel thick, scales with the display, and doubles under `strong_focus`.
#[test]
fn focus_ring_frames_the_segment_and_strong_focus_doubles_it() {
    assert_eq!(focus_ring_thickness(1.0, false), 2.0);
    assert_eq!(focus_ring_thickness(1.0, true), 4.0);
    assert_eq!(focus_ring_thickness(2.0, false), 4.0);
    assert_eq!(focus_ring_thickness(2.0, true), 8.0);
    assert_eq!(focus_ring_thickness(0.25, false), 1.0, "never thinner than one pixel");
    let rects = focus_ring_rects(50.0, 30.0, 100.0, 12.0, 2.0);
    assert_eq!(
        rects,
        [
            [50.0, 100.0, 30.0, 2.0],
            [50.0, 110.0, 30.0, 2.0],
            [50.0, 100.0, 2.0, 12.0],
            [78.0, 100.0, 2.0, 12.0],
        ]
    );
    for [left, top, width, height] in rects {
        assert!(left >= 50.0 && left + width <= 80.0 && top >= 100.0 && top + height <= 112.0);
    }
    // A segment narrower than two edges still gets a ring that stays inside it.
    let narrow = focus_ring_rects(10.0, 3.0, 0.0, 12.0, 2.0);
    assert!(narrow.iter().all(|[left, _, width, _]| *left >= 10.0 && left + width <= 13.0));
}

#[test]
fn a_strip_layout_is_committed_only_when_its_frame_presents() {
    // Assembly stages the whole strip it placed (band top, height and segment rects); pointer
    // routing and accessibility read only what a presented frame showed. A frame that fails or
    // presents nothing, after a resize, DPI or font change, keeps the previous strip whole.
    use sonicterm_render_model::boundary::cfg::status_line::StatusSegmentId;
    let segment = |id, left_px| PresentedStatusSegment { id, x: left_px, width: 40.0, click: None };
    let strip = |top, height, segments| StripHit { top, height, segments };
    let mut layouts = StripLayouts::default();
    layouts.stage(strip(500.0, 20.0, vec![segment(StatusSegmentId::Mem, 0.0)]));
    assert_eq!(layouts.presented(), None, "staged is not on screen");
    layouts.settle(true);
    let shown = strip(500.0, 20.0, vec![segment(StatusSegmentId::Mem, 0.0)]);
    assert_eq!(layouts.presented(), Some(&shown));
    // A resize or DPI change moves the band and scales the segments; the frame fails.
    layouts.stage(strip(980.0, 40.0, vec![segment(StatusSegmentId::Mem, 0.0)]));
    layouts.settle(false);
    assert_eq!(layouts.presented(), Some(&shown), "band and segments stay the presented ones");
    assert!(
        layouts.presented().unwrap().contains(510.0)
            && !layouts.presented().unwrap().contains(990.0)
    );
    layouts.settle(true);
    assert_eq!(layouts.presented(), Some(&shown), "a discarded stage never commits later");
    layouts.stage(strip(500.0, 20.0, vec![segment(StatusSegmentId::Cpu, 50.0)]));
    layouts.clear();
    layouts.settle(true);
    assert_eq!(layouts.presented(), None, "hiding the strip clears both");
    // The renderer reads band and segments only from the committed unit, never live geometry.
    let core = include_str!("core.rs");
    assert!(!core.contains("self.status_line_presented = placed"));
    assert!(core.contains("self.status_line_layouts.stage("));
    assert!(core.contains("self.status_line_layouts.settle("));
    committed_strip_checkout(core);
}

/// The committed-strip check on `core` as checked out, with LF or, on Windows, CRLF line endings;
/// the fixtures build both forms from its LF text.
fn committed_strip_checkout(core: &str) {
    committed_strip_fixtures(core);
}

#[test]
fn the_committed_strip_check_holds_on_a_crlf_checkout() {
    // A Windows checkout gives `include_str!` CRLF text; the check on it must pass as on LF.
    let crlf = include_str!("core.rs").replace("\r\n", "\n").replace('\n', "\r\n");
    committed_strip_checkout(&crlf);
}

/// Run the committed-strip check on an LF and a CRLF fixture of `source`, both built from its
/// LF form, so a source already checked out with CRLF never gains a doubled `\r`.
fn committed_strip_fixtures(source: &str) {
    let normalized = source.replace("\r\n", "\n");
    committed_strip_readers(&normalized);
    committed_strip_readers(&normalized.replace('\n', "\r\n"));
}

/// `status_line_hit` and `status_line_contains` in `core` read the committed strip, never live
/// geometry.
fn committed_strip_readers(core: &str) {
    let core = core.replace("\r\n", "\n");
    for reader in ["pub fn status_line_hit(", "pub fn status_line_contains("] {
        let start = core.find(reader).unwrap();
        let body = &core[start..start + core[start..].find("\n    }\n").unwrap()];
        assert!(!body.contains("status_line_top()"), "{reader} reads live geometry");
        assert!(body.contains("status_line_layouts.presented()"), "{reader}");
    }
}
