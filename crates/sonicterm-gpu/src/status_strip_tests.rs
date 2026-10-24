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

/// A strip geometry at `scale_factor` whose terminal font rasters at `font_pt` logical points.
fn geometry_for_font(font_pt: f32, scale_factor: f32) -> StripGeometry {
    StripGeometry {
        top: 0.0,
        height: status_line_height(13.0, scale_factor),
        position: StatusPosition::Bottom,
        screen: (800.0 * scale_factor, 600.0 * scale_factor),
        scale_factor,
        font_raster_px: font_pt * scale_factor,
    }
}

/// Contract: strip text is exactly 60% of the live terminal font size, scaled by DPI, never under
/// the 1 pt FreeType draws at; the strip's stack is built at the same size.
#[test]
fn strip_text_is_sixty_percent_of_the_terminal_font_size() {
    assert!((geometry_for_font(13.0, 1.0).text_px() - 7.8).abs() < 1e-4, "13 pt gives 7.8");
    assert!((geometry_for_font(13.5, 2.0).text_px() - 16.2).abs() < 1e-4, "13.5 pt at 2×");
    // A Cmd+= zoom changes the live size, and the strip follows it.
    assert!((geometry_for_font(16.0, 1.0).text_px() - 9.6).abs() < 1e-4, "a zoomed 16 pt");
    assert!((geometry_for_font(1.0, 1.0).text_px() - 1.0).abs() < 1e-4, "the 1 pt floor at 1×");
    assert!((status_line_font_size(13.0) - 7.8).abs() < 1e-4);
    assert!((status_line_font_size(13.5) - 8.1).abs() < 1e-4);
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
        position: StatusPosition::Bottom,
        screen: (400.0, 200.0),
        scale_factor: 1.0,
        font_raster_px: 40.0,
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

/// Ink height of `text` drawn alone through the strip with `stack`, at a 13 px terminal font.
fn strip_ink_height(stack: &sonicterm_engine::FontStack, text: &str) -> f32 {
    use sonicterm_render_model::boundary::ui::status_line::PlacedPart;
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let geometry = StripGeometry {
        top: 100.0,
        height: 40.0,
        position: StatusPosition::Bottom,
        screen: (400.0, 200.0),
        scale_factor: 1.0,
        font_raster_px: 13.0,
    };
    let part = PlacedPart { text: text.into(), tone: StatusTone::Value, left: 10.0, width: 80.0 };
    let placed = vec![PlacedSegment { index: 0, left: 10.0, width: 80.0, parts: vec![part] }];
    let theme = sonicterm_render_model::boundary::cfg::theme::Theme::default();
    let palette = sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(&theme);
    let colors = StatusColors::resolve(&theme, ChromeColor::WHITE, ChromeColor::WHITE, &palette);
    let (mut quads, mut glyphs) = (Vec::new(), Vec::new());
    emit_strip(
        &placed,
        &geometry,
        &colors,
        Some(stack),
        Some(&mut raster),
        &mut atlas,
        &mut quads,
        &mut glyphs,
    );
    glyphs
        .iter()
        .map(|glyph| crate::cursor::ndc_rect_px(glyph.rect, 400.0, 200.0).3)
        .fold(0.0, f32::max)
}

/// The strip draws at its own size: a native stack at `status_line_font_size`, sharing the body
/// configuration, so its capitals are smaller than the tab title's, not tab-title tiles placed
/// on a smaller baseline. The renderer builds, rescales and routes that stack.
#[test]
fn the_strip_draws_with_its_own_smaller_stack() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let body = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![assets],
        13.0,
        72,
        1.0,
    )
    .unwrap();
    let stacks = crate::core::renderer_font_views(Some(body), 13.0);
    let title = stacks.tab_title.expect("a tab-title stack");
    let strip = stacks.status_line.expect("a strip stack");
    assert!(strip.shares_configuration_with(&title), "one configuration, one font database");
    assert!((status_line_font_size(13.0) - 7.8).abs() < 1e-4, "60% of 13 pt");
    let (strip_h, title_h) = (strip_ink_height(&strip, "H"), strip_ink_height(&title, "H"));
    // 7.8 pt against 14 pt: the strip's capitals are drawn shorter, whole pixels apart.
    assert!(strip_h > 0.0 && strip_h < title_h, "strip H {strip_h} vs title H {title_h}");
    let core = include_str!("core.rs").replace("\r\n", "\n");
    assert!(core.contains("let mut strip_rasterizer = self.status_line_font_stack.clone();"));
    assert!(core.contains("self.status_line_font_stack = new_stacks.status_line;"));
    assert!(core.contains("if let Some(stack) = self.status_line_font_stack.as_ref() {"));
    assert!(core.contains("GlyphRasterVariant::StatusLine => self.status_line_font_stack.as_ref()"));
}

/// Drawn `H` height over the text size the geometry reports, for a `body_pt` terminal font at
/// `scale`, on the bundled font's strip stack.
fn cap_over_text_px(body_pt: f32, scale: f32) -> f32 {
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let dpi = (72.0 * scale).round() as usize;
    let body = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![assets],
        f64::from(body_pt),
        dpi,
        1.0,
    )
    .unwrap();
    let stack = crate::core::renderer_font_views(Some(body), body_pt).status_line.unwrap();
    let text_px = geometry_for_font(body_pt, scale).text_px();
    let (ink_h, _) =
        drawn_h(&stack, &mut GlyphAtlas::new(512, 512), GlyphRasterVariant::StatusLine, text_px);
    ink_h / text_px
}

/// FreeType never rasterizes under 1 pt, so at a 1 pt terminal font the strip's stack and its
/// geometry both use 1 pt: the drawn capital keeps the proportion it has at a normal size,
/// instead of standing taller than the text size the geometry centers and fits by.
#[test]
fn a_tiny_strip_size_is_the_size_freetype_draws() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!((status_line_font_size(1.0) - 1.0).abs() < 1e-4, "the stack asks for 1 pt");
    assert!((geometry_for_font(1.0, 2.0).text_px() - 2.0).abs() < 1e-4, "1 pt at 2×");
    let (normal, tiny) = (cap_over_text_px(13.0, 8.0), cap_over_text_px(1.0, 8.0));
    assert!((tiny - normal).abs() < 0.15, "cap/text {tiny} at 1 pt vs {normal} at 13 pt");
}

/// Draw `H` with `stack` as raster role `variant` at `size_px` into `atlas`; returns its ink
/// height and atlas UV rect.
fn drawn_h(
    stack: &sonicterm_engine::FontStack,
    atlas: &mut GlyphAtlas,
    variant: GlyphRasterVariant,
    size_px: f32,
) -> (f32, [f32; 4]) {
    let mut raster = stack.clone();
    let run = chrome_text::layout_with_raster_variant(
        stack,
        &mut raster,
        atlas,
        "H",
        ChromeColor::WHITE,
        ChromeAttrs { bold: false, italic: false },
        size_px,
        size_px,
        (0.0, 100.0),
        (400.0, 200.0),
        None,
        variant,
    );
    let glyph = run.glyphs.first().expect("H draws");
    (crate::cursor::ndc_rect_px(glyph.rect, 400.0, 200.0).3, glyph.uv)
}

/// The strip's tiles share the renderer's one atlas with the tab title's, keyed apart by raster
/// role, so they never alias. A glyph key carries no size, so a warm atlas would keep serving
/// old-size strip tiles after a zoom or a DPI change; both changes reset the atlas and rebuild or
/// rescale the strip's stack, and a fresh atlas then rasterizes the strip at the new size.
#[test]
fn the_strip_rasterizes_anew_after_zoom_and_dpi_changes() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let views_at = |body_pt: f32| {
        let body = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
            &[("Rec Mono St.Helens", false)],
            vec![assets.clone()],
            f64::from(body_pt),
            72,
            1.0,
        )
        .unwrap();
        crate::core::renderer_font_views(Some(body), body_pt)
    };
    let strip_px = |body_pt: f32| status_line_font_size(body_pt);
    let views = views_at(13.0);
    let (strip, title) = (views.status_line.unwrap(), views.tab_title.unwrap());
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let (strip_h, strip_uv) =
        drawn_h(&strip, &mut atlas, GlyphRasterVariant::StatusLine, strip_px(13.0));
    let (title_h, title_uv) = drawn_h(&title, &mut atlas, GlyphRasterVariant::TabTitle, 14.0);
    let roles: std::collections::HashSet<GlyphRasterVariant> = atlas
        .resident_tile_keys()
        .into_iter()
        .filter(|key| key.ch == 'H' || key.glyph_id != 0)
        .map(|key| key.raster_variant)
        .collect();
    assert!(
        roles.contains(&GlyphRasterVariant::StatusLine)
            && roles.contains(&GlyphRasterVariant::TabTitle)
    );
    assert_ne!(strip_uv, title_uv, "one atlas, two tiles: the strip never reads a title tile");
    assert!(strip_h < title_h, "strip H {strip_h} vs title H {title_h}");
    // Zoom to 20 pt: the warm atlas would still serve the 7.8 pt tile, which is why it is reset.
    let zoomed = views_at(20.0).status_line.unwrap();
    let (stale_h, _) = drawn_h(&zoomed, &mut atlas, GlyphRasterVariant::StatusLine, strip_px(20.0));
    assert_eq!(stale_h, strip_h, "a glyph key has no size, so only a reset drops the old tile");
    let mut reset = GlyphAtlas::new(1024, 1024);
    let (zoomed_h, _) =
        drawn_h(&zoomed, &mut reset, GlyphRasterVariant::StatusLine, strip_px(20.0));
    assert!(
        zoomed_h > strip_h,
        "after the reset the strip draws at 12 pt: {zoomed_h} vs {strip_h}"
    );
    // A DPI change to 2× rescales the strip's stack; a fresh atlas draws it twice as tall.
    strip.change_scaling(strip.get_font_scale(), 144);
    let mut rescaled = GlyphAtlas::new(1024, 1024);
    let (double_h, _) =
        drawn_h(&strip, &mut rescaled, GlyphRasterVariant::StatusLine, strip_px(13.0) * 2.0);
    assert!((double_h - 2.0 * strip_h).abs() <= 2.0, "2× draws {double_h} vs {strip_h} at 1×");
    // The renderer resets the atlas on both changes and rebuilds or rescales the strip's stack.
    let core = include_str!("core.rs").replace("\r\n", "\n");
    // The body of a renderer method, up to its closing brace.
    let method = |signature: &str| {
        let start = core.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
        core[start..start + core[start..].find("\n    }\n").unwrap()].to_owned()
    };
    // `set_font` builds the new stacks and adopts them, which resets the atlas.
    let set_font = method("    pub fn set_font(");
    assert!(set_font.contains("self.adopt_font_stacks("));
    let adopt = method("    fn adopt_font_stacks(");
    assert!(adopt.contains("self.reset_glyph_atlas_in_place(\"font_change\");"));
    assert!(adopt.contains("self.status_line_font_stack = new_stacks.status_line;"));
    let rescale = method("    fn rebuild_for_sf(");
    assert!(rescale.contains("self.reset_glyph_atlas_in_place(\"dpi_change\");"));
    assert!(
        rescale.contains(
            "if let Some(stack) = self.status_line_font_stack.as_ref() {\n            stack.change_scaling("
        ),
        "the DPI change rescales the strip's stack"
    );
}

/// The body of renderer method `signature` in `core`, up to its closing brace.
fn renderer_method<'source>(core: &'source str, signature: &str) -> &'source str {
    let start = core.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    &core[start..start + core[start..].find("\n    }\n").unwrap()]
}

/// Moving a shown strip keeps the presented layout, so routing and accessibility keep the band
/// still on screen until a frame at the new edge presents; only hiding the strip drops it. Both
/// insets read the placement, so the pane area follows the position.
#[test]
fn moving_the_strip_keeps_the_presented_layout_until_a_frame_presents() {
    let core = include_str!("core.rs").replace("\r\n", "\n");
    let moved = renderer_method(&core, "pub fn set_status_line_position(");
    assert!(!moved.contains("status_line_layouts.clear()"), "a move drops the presented strip");
    assert!(moved.contains("last_frame_key = None"), "a move forces the next frame");
    let hidden = renderer_method(&core, "pub fn set_status_line_visible(");
    assert!(hidden.contains("status_line_layouts.clear()"), "hiding drops the strip");
    for inset in ["pub fn top_inset(", "pub fn bottom_inset("] {
        assert!(renderer_method(&core, inset).contains("status_line_placement()"), "{inset}");
    }
}

/// At either position the band sits on its edge, reserves exactly its height there, and the pane
/// area left between the insets never overlaps it.
#[test]
fn the_band_reserves_its_own_edge_and_never_overlaps_the_panes() {
    let (surface_h, titlebar_px, tab_bar_px, strip_h) = (600.0, 28.0, 40.0, 23.0);
    let top = strip_placement(StatusPosition::Top, surface_h, titlebar_px, tab_bar_px, strip_h);
    assert_eq!(top, StripPlacement { top: 28.0, reserved_top: 23.0, reserved_bottom: 0.0 });
    let bottom =
        strip_placement(StatusPosition::Bottom, surface_h, titlebar_px, tab_bar_px, strip_h);
    assert_eq!(bottom, StripPlacement { top: 537.0, reserved_top: 0.0, reserved_bottom: 23.0 });
    for placement in [top, bottom] {
        let panes_top = titlebar_px + placement.reserved_top;
        let panes_bottom = surface_h - tab_bar_px - placement.reserved_bottom;
        let band = placement.top..placement.top + strip_h;
        assert!(band.end <= panes_top || band.start >= panes_bottom, "{placement:?}");
        assert!(panes_bottom - panes_top == surface_h - titlebar_px - tab_bar_px - strip_h);
    }
    let hidden = strip_placement(StatusPosition::Top, surface_h, titlebar_px, tab_bar_px, 0.0);
    assert_eq!((hidden.reserved_top, hidden.reserved_bottom), (0.0, 0.0), "a hidden strip");
    let no_tab_bar = strip_placement(StatusPosition::Bottom, surface_h, 0.0, 0.0, strip_h);
    assert_eq!(no_tab_bar.top, 577.0, "with the tab bar hidden the band sits on the bottom edge");
}

/// The hairline sits on the band's terminal side: its bottom edge at the top, its top edge at
/// the bottom.
#[test]
fn the_hairline_sits_on_the_terminal_side() {
    assert_eq!(hairline_top(StatusPosition::Top, 28.0, 23.0, 2.0), 49.0);
    assert_eq!(hairline_top(StatusPosition::Bottom, 537.0, 23.0, 2.0), 537.0);
}
