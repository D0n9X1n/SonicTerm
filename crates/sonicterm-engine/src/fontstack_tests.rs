use super::*;
use std::path::Path;

/// The family of the test-only Rec Mono St.Helens fixture in `sonicterm-font/test-fonts/rec-mono`.
const REC_MONO_FAMILY: &str = "Rec Mono St.Helens";

// The Windows color face must reach the atlas as artwork, unchanged by monochrome weight settings.
#[cfg(windows)]
#[test]
fn native_color_face_preserves_artwork_across_weights() {
    let make_stack = |scale| {
        FontStack::try_new_full_with_weight_and_font_dirs(
            "Segoe UI Emoji",
            14.5,
            72,
            scale,
            &[PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../sonicterm-font/test-fonts/rec-mono")],
        )
        .unwrap()
    };
    let mut identity = make_stack(1.0);
    for character in ['\u{1f600}', '\u{1f680}'] {
        let key = GlyphKey::new(character, false, false);
        let baseline = identity.rasterize(key).expect("native emoji glyph");
        assert!(baseline.is_color, "Windows emoji face must produce color artwork");
        for scale in [0.5, 1.0, 2.0, 5.0] {
            let candidate = make_stack(scale).rasterize(key).unwrap();
            assert!(candidate.is_color);
            assert_eq!(
                (
                    candidate.width,
                    candidate.height,
                    candidate.offset_x,
                    candidate.offset_y,
                    candidate.advance
                ),
                (
                    baseline.width,
                    baseline.height,
                    baseline.offset_x,
                    baseline.offset_y,
                    baseline.advance
                )
            );
            assert_eq!(candidate.coverage, baseline.coverage);
        }
    }
}

// Nonempty transparent rasters stay valid through production conversion and atlas insertion, not missing/tofu.
#[test]
fn blank_color_raster_survives_fontstack_and_atlas() {
    struct Prepared(Option<RasterTile>);
    impl Rasterizer for Prepared {
        fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
            self.0.take()
        }
    }
    let stack = FontStack::try_new_with_font_dirs_for_test(
        &[(REC_MONO_FAMILY, false)],
        vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono")
        ],
        14.0,
        72,
        1.0,
    )
    .unwrap();
    let key = GlyphKey::new('A', false, false);
    let glyph = sonicterm_font::RasterizedGlyph {
        data: vec![0; 3 * 2 * 4],
        width: 3,
        height: 2,
        bearing_x: sonicterm_font::units::PixelLength::new(-3.0),
        bearing_y: sonicterm_font::units::PixelLength::new(9.0),
        has_color: true,
        is_scaled: true,
    };
    let tile = stack.rasterized_glyph_to_tile(glyph).expect("blank is valid");
    assert_eq!((tile.width, tile.height), (3, 2));
    assert_eq!((tile.offset_x, tile.offset_y), (-3, -9));
    assert!(tile.is_color && !tile.is_empty());
    assert!(tile.coverage.iter().all(|byte| *byte == 0));
    let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(16, 16);
    let info = atlas.get_or_insert(key, &mut Prepared(Some(tile))).unwrap();
    assert_eq!(info.px_size, [3, 2]);
    assert_eq!(info.px_offset, [-3, -9]);
    assert!(info.is_color && info.uv[2] > info.uv[0] && info.uv[3] > info.uv[1]);
    assert!(atlas.pixels().iter().all(|byte| *byte == 0));
    assert_eq!(atlas.get(key), Some(info));
}

#[test]
fn weight_scale_does_not_change_cell_metrics() {
    let regular = match FontStack::try_new_full_with_weight(DEFAULT_FONT_FAMILY, 14.0, 72, 1.0) {
        Ok(stack) => stack,
        Err(_) => return,
    };
    let heavier = match FontStack::try_new_full_with_weight(DEFAULT_FONT_FAMILY, 14.0, 72, 1.1) {
        Ok(stack) => stack,
        Err(_) => return,
    };
    let regular_metrics = match regular.cell_metrics_raster_px() {
        Ok(metrics) => metrics,
        Err(_) => return,
    };
    let heavier_metrics = match heavier.cell_metrics_raster_px() {
        Ok(metrics) => metrics,
        Err(_) => return,
    };
    assert_eq!(regular_metrics, heavier_metrics);
}

#[test]
fn bold_style_resolves_separately_from_regular_style() {
    let stack = match FontStack::try_new(72) {
        Ok(stack) => stack,
        Err(_) => return,
    };
    let regular = match stack.font_for_style(false, false) {
        Ok(font) => font,
        Err(_) => return,
    };
    let bold = match stack.font_for_style(true, false) {
        Ok(font) => font,
        Err(_) => return,
    };
    assert_ne!(regular.style(), bold.style());
    assert!(
        bold.style().font[0].weight.to_opentype_weight()
            > regular.style().font[0].weight.to_opentype_weight()
    );
}

#[test]
fn explicit_config_records_requested_font_size() {
    let cfg =
        build_config_with_font_dirs("Rec Mono St.Helens", 17.0, &["Symbols Nerd Font Mono"], &[]);
    assert_eq!(cfg.font_size, 17.0);
    assert_eq!(cfg.font.font[0].family, "Rec Mono St.Helens");
    assert_eq!(cfg.font.font[1].family, "Symbols Nerd Font Mono");
}

#[test]
fn font_dirs_resolve_all_rec_mono_fixture_styles() {
    // Configured font directories resolve every style directly while native fallback discovery stays on.
    let fonts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono");
    let config = build_config_with_font_dirs(
        REC_MONO_FAMILY,
        14.0,
        &fallback_families(REC_MONO_FAMILY),
        std::slice::from_ref(&fonts),
    );
    assert_eq!(config.font_locator, config::FontLocatorSelection::default());
    assert_eq!(config.font_dirs, vec![fonts.clone()]);

    let stack =
        FontStack::try_new_full_with_weight_and_font_dirs(REC_MONO_FAMILY, 14.0, 72, 1.0, &[fonts])
            .expect("tracked Rec Mono fixture fonts must build the production stack");

    for (bold, italic, expected_file) in [
        (false, false, "RecMonoSt.Helens-Regular.ttf"),
        (true, false, "RecMonoSt.Helens-Bold.ttf"),
        (false, true, "RecMonoSt.Helens-Italic.ttf"),
        (true, true, "RecMonoSt.Helens-BoldItalic.ttf"),
    ] {
        let font = stack.font_for_style(bold, italic).expect("fixture style must resolve");
        let primary = font.clone_handles().into_iter().next().expect("configured face");
        assert_eq!(primary.handle.origin, sonicterm_font::locator::FontOrigin::FontDirs);
        assert_eq!(primary.names().family, REC_MONO_FAMILY);
        assert_eq!(
            primary.handle.path_str().as_deref().map(Path::new).and_then(Path::file_name),
            Some(expected_file.as_ref())
        );
        assert!(!primary.synthesize_bold, "fixture bold face must be selected directly");
        assert!(!primary.synthesize_italic, "fixture italic face must be selected directly");
    }
}

/// Regression: a window moving between displays of different scale
/// factors must re-rasterize at the new DPI. `change_scaling` is the
/// runtime path the gpu renderer's `rebuild_for_sf` relies on; doubling
/// the DPI (the 72 -> 144 step that a 1.0 -> 2.0 scale-factor move
/// produces) must roughly double the raster-px cell metrics. If a stale
/// DPI leaked through, the metrics would not change and fonts would
/// render at the wrong size.
#[test]
fn change_scaling_rescales_cell_metrics_with_dpi() {
    let stack = match FontStack::try_new(72) {
        Ok(stack) => stack,
        // No usable font in this sandbox; the bundled-font CI gate covers
        // the real assertion. Nothing to verify here.
        Err(_) => return,
    };
    let base = match stack.cell_metrics_raster_px() {
        Ok(metrics) => metrics,
        Err(_) => return,
    };
    assert!(base.cell_h > 0.0 && base.cell_w > 0.0, "baseline metrics must be positive");

    // Preserve logical font scale, double the DPI (1.0 -> 2.0 scale factor).
    stack.change_scaling(stack.get_font_scale(), 144);
    let scaled = stack.cell_metrics_raster_px().expect("metrics must resolve after change_scaling");

    let ratio = scaled.cell_h / base.cell_h;
    assert!(
        (1.6..=2.4).contains(&ratio),
        "doubling DPI should ~double cell height; got ratio {ratio} (base {} -> scaled {})",
        base.cell_h,
        scaled.cell_h
    );
}

/// The Windows fallback stack emits real OpenType mark positioning for the renderer to preserve.
#[cfg(target_os = "windows")]
#[test]
fn rec_mono_shaper_emits_nonzero_mark_offsets() {
    let fonts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono");
    let stack =
        FontStack::try_new_full_with_weight_and_font_dirs(REC_MONO_FAMILY, 15.0, 96, 1.0, &[fonts])
            .expect("Rec Mono fixture stack");
    let glyphs = stack.shape_text("مُحَمَّد").expect("Arabic marks shape through fallback");

    assert!(
        glyphs.iter().any(|glyph| glyph.x_offset.get().abs() >= 0.5),
        "fixture must exercise horizontal mark positioning"
    );
    assert!(
        glyphs.iter().any(|glyph| glyph.y_offset.get() <= -3.0),
        "fixture must exercise vertical mark positioning"
    );
}

#[test]
fn shaped_text_width_covers_mixed_ascii_cjk_and_status_text() {
    let stack = match FontStack::try_new(72) {
        Ok(stack) => stack,
        Err(_) => return,
    };

    let ascii = match stack.measure_text_width("/ search · 0/0") {
        Ok(width) => width,
        Err(_) => return,
    };
    let mixed = stack
        .measure_text_width("/ search法土大夫 · 0/0")
        .expect("mixed fallback-font text should shape");

    assert!(ascii.is_finite() && ascii > 0.0);
    assert!(mixed.is_finite() && mixed > ascii, "CJK glyphs must contribute to badge width");
}

// Pixel meaning alone excludes color artwork; every monochrome face shares this conversion stage.
#[test]
fn weight_conversion_preserves_geometry_and_color_artwork() {
    let make_stack = |scale| {
        FontStack::try_new_with_font_dirs_for_test(
            &[(REC_MONO_FAMILY, false)],
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../sonicterm-font/test-fonts/rec-mono")],
            14.5,
            72,
            scale,
        )
        .unwrap()
    };
    for color in [false, true] {
        let make_glyph = || sonicterm_font::RasterizedGlyph {
            data: [32, 64, 96, 96].repeat(20),
            width: 4,
            height: 5,
            bearing_x: sonicterm_font::units::PixelLength::new(-2.0),
            bearing_y: sonicterm_font::units::PixelLength::new(8.0),
            has_color: color,
            is_scaled: true,
        };
        let baseline = make_stack(1.0).rasterized_glyph_to_tile(make_glyph()).unwrap();
        for scale in [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0] {
            let candidate = make_stack(scale).rasterized_glyph_to_tile(make_glyph()).unwrap();
            assert_eq!(
                (
                    candidate.width,
                    candidate.height,
                    candidate.offset_x,
                    candidate.offset_y,
                    candidate.advance
                ),
                (
                    baseline.width,
                    baseline.height,
                    baseline.offset_x,
                    baseline.offset_y,
                    baseline.advance
                )
            );
            assert_eq!(candidate.is_color, color);
            if color || scale == 1.0 {
                assert_eq!(candidate.coverage, baseline.coverage);
            } else {
                assert_ne!(candidate.coverage, baseline.coverage);
                assert!(candidate
                    .coverage
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|px| px[3] == px[0].max(px[1]).max(px[2])));
            }
        }
    }
}

// Designed bold counters stay open under the same outline-growth policy as regular text.
#[test]
fn bold_counters_survive_weight_two_at_current_raster_size() {
    let mut stack = FontStack::try_new_with_font_dirs_for_test(
        &[(REC_MONO_FAMILY, false)],
        vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono")
        ],
        14.5,
        72,
        2.0,
    )
    .unwrap();
    for character in ['0', '8', 'B'] {
        let tile = stack.rasterize(GlyphKey::new(character, true, false)).unwrap();
        let width = tile.width as usize;
        let height = tile.height as usize;
        let channels = if tile.is_subpixel { 4 } else { 1 };
        let alpha = |column: usize, row: usize| {
            tile.coverage[(row * width + column) * channels + channels - 1]
        };
        assert!(
            (1..height - 1).any(|row| (1..width - 1).any(|column| {
                alpha(column, row) < 96
                    && (0..column).any(|left| alpha(left, row) > 192)
                    && (column + 1..width).any(|right| alpha(right, row) > 192)
            })),
            "bold {character} counter must remain open"
        );
    }
}

#[cfg(test)]
#[path = "fontstack_tests_frame_fixture_tests.rs"]
mod frame_fallback;

/// The diagnostic face resolution mirrors the rasterizer: a character key (glyph id 0) and the
/// shaped key for the same glyph resolve to one face, one glyph id and one strike, and the strike
/// is the loaded face's raster size in thousandths of a pixel (14 pt at 96 DPI is 18.667 px).
#[test]
fn a_character_key_and_its_shaped_key_resolve_to_one_face_and_strike() {
    let fonts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono");
    let stack = FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![fonts],
        14.0,
        96,
        1.0,
    )
    .unwrap();
    let shaped = stack.shape_text_with_style("a", false, false).unwrap();
    let glyph = shaped.first().expect("a shapes to one glyph");
    let by_character = stack.resolved_glyph_face(GlyphKey::new('a', false, false));
    let by_glyph = stack.resolved_glyph_face(GlyphKey::shaped(
        'a',
        u8::try_from(glyph.font_idx).unwrap(),
        glyph.glyph_pos,
        false,
        false,
    ));
    let resolved = by_character.expect("the character resolves");
    assert_eq!(Some(&resolved), by_glyph.as_ref(), "both keys name one tile source");
    assert_eq!(resolved.glyph_id, glyph.glyph_pos);
    assert_eq!(resolved.strike_px_milli, 18_667);
    assert!(resolved.face.source.ends_with("RecMonoSt.Helens-Regular.ttf"), "{resolved:?}");
    assert_eq!(resolved.face.face_index, 0);
}

/// A face's content identity is the file namespace's SHA-256 of the exact bytes on disk that the
/// key's face was loaded from, so it does not depend on the file's name or directory; a key that
/// resolves to no face names no content.
#[test]
fn a_resolved_face_content_is_the_digest_of_its_file_bytes() {
    use sonicterm_text::face_content::{face_content_id, FaceNamespace};
    let stack = rec_mono_stack();
    let key = GlyphKey::new('a', false, false);
    let resolved = stack.resolved_glyph_face(key).expect("the character resolves");
    let bytes = std::fs::read(&resolved.face.source).expect("the fixture face file reads");
    assert_eq!(
        stack.resolved_face_content(key),
        Some(face_content_id(FaceNamespace::File, &bytes)),
        "the content is the file's digest in the file namespace"
    );
    assert_eq!(stack.resolved_face_content(GlyphKey::shaped('a', 200, 1, false, false)), None);
}

/// A deterministic stack over the Rec Mono fixture faces, independent of host fonts.
fn rec_mono_stack() -> FontStack {
    let fonts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono");
    FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![fonts],
        14.0,
        96,
        1.0,
    )
    .unwrap()
}

/// The face today's path resolves for `(bold, italic)` on `stack`: the configured style, made
/// bold or italic, at the stack's own point size.
fn todays_face(
    stack: &FontStack,
    bold: bool,
    italic: bool,
) -> std::rc::Rc<sonicterm_font::LoadedFont> {
    let mut style: TextStyle = stack.font_config.config().font.clone();
    if bold {
        style = style.make_bold();
    }
    if italic {
        style = style.make_italic();
    }
    stack.font_config.resolve_font_at_size(&style, stack.font_size_pt).unwrap()
}

#[test]
fn style_faces_match_todays_resolution() {
    // The memo changes how often a face is resolved, never which face: every style returns the
    // configuration's own `Rc`, a face replacement returns a new one, and a native-size view
    // resolves at its own size.
    let stack = rec_mono_stack();
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        let memoized = stack.font_for_style(bold, italic).unwrap();
        assert!(std::rc::Rc::ptr_eq(&memoized, &todays_face(&stack, bold, italic)));
        let again = stack.font_for_style(bold, italic).unwrap();
        assert!(std::rc::Rc::ptr_eq(&memoized, &again), "a repeat returns the same face");
    }
    let before_scaling = stack.font_for_style(false, false).unwrap();
    stack.change_scaling(stack.get_font_scale(), 192);
    let after_scaling = stack.font_for_style(false, false).unwrap();
    assert!(!std::rc::Rc::ptr_eq(&before_scaling, &after_scaling), "scaling replaces the face");
    stack.font_config.config_changed(&stack.font_config.config()).unwrap();
    let after_reload = stack.font_for_style(false, false).unwrap();
    assert!(!std::rc::Rc::ptr_eq(&after_scaling, &after_reload), "a reload replaces the face");
    let view = stack.with_font_size(20.0);
    let view_face = view.font_for_style(false, false).unwrap();
    assert!(std::rc::Rc::ptr_eq(&view_face, &todays_face(&view, false, false)));
    assert!(!std::rc::Rc::ptr_eq(&view_face, &after_reload), "the view has its own size");
}

#[test]
fn memo_skips_resolution_and_every_warmed_sibling_misses_after_a_shared_change() {
    // A warm stack shapes without resolving its face again, and a face replacement made through
    // any sibling (a clone or a native-size view sharing the configuration) makes every warmed
    // sibling resolve exactly once at its next lookup and return the new face.
    let stack = rec_mono_stack();
    let resolves = || stack.font_config.resolve_count_for_test();
    stack.shape_text_for_frame("abc", false, false).unwrap();
    let warm = resolves();
    stack.shape_text_for_frame("abc", false, false).unwrap();
    assert_eq!(resolves() - warm, 0, "a warm style resolves nothing");

    let clone = stack.clone();
    let view = stack.with_font_size(20.0);
    let siblings = [&stack, &clone, &view];
    type Replace<'replace> = Box<dyn Fn() + 'replace>;
    let replacements: [(&str, Replace<'_>); 2] = [
        (
            "change_scaling",
            Box::new(|| {
                clone.change_scaling(clone.get_font_scale(), 144);
            }),
        ),
        (
            "config_changed",
            Box::new(|| {
                clone.font_config.config_changed(&clone.font_config.config()).unwrap();
            }),
        ),
    ];
    for (name, replace) in replacements {
        let old_faces: Vec<_> =
            siblings.iter().map(|sibling| sibling.font_for_style(false, false).unwrap()).collect();
        replace();
        for (sibling, old_face) in siblings.iter().zip(&old_faces) {
            let before = resolves();
            let face = sibling.font_for_style(false, false).unwrap();
            assert_eq!(resolves() - before, 1, "{name}: one resolve at the next lookup");
            assert!(!std::rc::Rc::ptr_eq(&face, old_face), "{name}: the new face is returned");
            let before_repeat = resolves();
            sibling.font_for_style(false, false).unwrap();
            assert_eq!(resolves() - before_repeat, 0, "{name}: then the memo is warm again");
        }
    }
}

/// The Rec Mono fixture reports a cap height of its own: below the cell height, and scaling with
/// the stack's size, so the strip centers on the font's measurement, not the `H` fallback.
#[test]
fn rec_mono_fixture_reports_a_cap_height_that_follows_its_size() {
    let at = |size: f64| {
        FontStack::try_new_with_font_dirs_for_test(
            &[(REC_MONO_FAMILY, false)],
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../sonicterm-font/test-fonts/rec-mono")],
            size,
            72,
            1.0,
        )
        .unwrap()
    };
    let (small, large) = (at(9.6), at(19.2));
    let small_cap = small.cap_height_raster_px().expect("the bundled font has a cap height");
    let large_cap = large.cap_height_raster_px().expect("at every size");
    let cell_h = small.cell_metrics_raster_px().unwrap().cell_h;
    assert!(small_cap > 0.0 && small_cap < cell_h, "cap {small_cap} vs cell {cell_h}");
    assert!(large_cap > small_cap * 1.6, "{large_cap} follows the size from {small_cap}");
    println!("cap height: {small_cap} px at 9.6 pt, {large_cap} px at 19.2 pt");
}

/// The app's font directory: the bundled symbols font production passes to every stack.
fn bundled_font_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")
}

/// A Nerd Font icon the platform's default monospace face lacks resolves from the bundled
/// symbols font in the app's font directory, ahead of any host-installed copy or system
/// fallback, for the body stack and the chrome views built from it.
#[test]
fn nerd_icon_resolves_from_the_bundled_symbols_font_under_the_default_family() {
    let bundled = bundled_font_dir()
        .join("SymbolsNerdFontMono-Regular.ttf")
        .canonicalize()
        .expect("the bundled symbols font is tracked");
    let stack = FontStack::try_new_full_with_weight_and_font_dirs(
        DEFAULT_FONT_FAMILY,
        14.0,
        72,
        1.0,
        &[bundled_font_dir()],
    )
    .expect("the default family builds a stack");
    for view in [stack.clone(), stack.with_font_size(11.0)] {
        // U+F07B (nf-fa-folder) and U+F0001 (nf-md) sit outside every platform default face.
        for icon in ['\u{F07B}', '\u{F0001}'] {
            let resolved = view
                .resolved_glyph_face(GlyphKey::new(icon, false, false))
                .unwrap_or_else(|| panic!("{icon:?} resolves to a face"));
            assert_eq!(Path::new(&resolved.face.source), bundled, "{icon:?}: {resolved:?}");
            assert_ne!(resolved.glyph_id, 0, "{icon:?} is a real glyph, not notdef");
        }
    }
}

/// A configured family the host does not have falls back to the platform's default monospace
/// face for text and cell metrics, never to the symbols or emoji fallback that follows it.
#[test]
fn a_missing_configured_family_falls_back_to_the_default_monospace_face() {
    let primary_family = |family: &str| {
        let stack = FontStack::try_new_full_with_weight_and_font_dirs(
            family,
            14.0,
            72,
            1.0,
            &[bundled_font_dir()],
        )
        .expect("a stack builds even when its family is missing");
        let font = stack.font_for_style(false, false).expect("the regular style resolves");
        let handles = font.clone_handles();
        handles.first().expect("some face loads").names().family.clone()
    };
    let default_family = primary_family(DEFAULT_FONT_FAMILY);
    assert_ne!(default_family, "Symbols Nerd Font Mono");
    assert_eq!(primary_family("SonicTermNoSuchFamily-5D1C"), default_family);
}
