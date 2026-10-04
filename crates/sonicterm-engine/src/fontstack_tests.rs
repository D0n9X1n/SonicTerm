use super::*;
use std::path::Path;

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
            &[PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")],
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
        &[(DEFAULT_FONT_FAMILY, false)],
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")],
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

// Identity and endpoint coverage remain exact while intermediate coverage follows the weight control.
#[test]
fn weight_scale_preserves_identity_and_extremes() {
    let original = vec![0, 1, 64, 128, 254, 255];
    let mut coverage = original.clone();
    apply_weight_scale(&mut coverage, 1.0, false);
    assert_eq!(coverage, original);

    let mut stronger = original.clone();
    apply_weight_scale(&mut stronger, 1.1, false);
    assert_eq!(stronger[0], 0);
    assert_eq!(*stronger.last().unwrap(), 255);
    assert!(stronger[2] > original[2]);
    assert!(stronger[3] > original[3]);

    let mut lighter = original.clone();
    apply_weight_scale(&mut lighter, 0.9, false);
    assert!(lighter[2] < original[2]);
    assert!(lighter[3] < original[3]);
}

// RGB remapping rebuilds the alpha envelope without adding ink to empty pixels.
#[test]
fn subpixel_weight_scale_recomputes_alpha_from_rgb_coverage() {
    let mut coverage = vec![32, 64, 96, 96, 0, 0, 0, 0];
    apply_weight_scale(&mut coverage, 1.1, true);
    assert_eq!(coverage[3], coverage[0].max(coverage[1]).max(coverage[2]));
    assert_eq!(&coverage[4..], &[0, 0, 0, 0]);
}

#[test]
fn invalid_weight_scales_fall_back_to_identity() {
    for scale in [f32::NAN, f32::INFINITY, 0.0, 0.49, 5.01] {
        assert_eq!(sanitize_weight_scale(scale), 1.0);
    }
    assert_eq!(sanitize_weight_scale(1.1), 1.1);
    assert_eq!(sanitize_weight_scale(5.0), 5.0);
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
fn production_font_dirs_resolve_all_packaged_rec_mono_styles() {
    // Protect packaged startup and live reload without disabling native fallback discovery.
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let config = build_config_with_font_dirs(
        DEFAULT_FONT_FAMILY,
        14.0,
        FALLBACK_FAMILIES,
        std::slice::from_ref(&fonts),
    );
    assert_eq!(config.font_locator, config::FontLocatorSelection::default());
    assert_eq!(config.font_dirs, vec![fonts.clone()]);

    let stack = FontStack::try_new_full_with_weight_and_font_dirs(
        DEFAULT_FONT_FAMILY,
        14.0,
        72,
        1.0,
        &[fonts],
    )
    .expect("tracked packaged fonts must build the production stack");

    for (bold, italic, expected_file) in [
        (false, false, "RecMonoSt.Helens-Regular.ttf"),
        (true, false, "RecMonoSt.Helens-Bold.ttf"),
        (false, true, "RecMonoSt.Helens-Italic.ttf"),
        (true, true, "RecMonoSt.Helens-BoldItalic.ttf"),
    ] {
        let font = stack.font_for_style(bold, italic).expect("packaged style must resolve");
        let primary = font.clone_handles().into_iter().next().expect("configured face");
        assert_eq!(primary.handle.origin, sonicterm_font::locator::FontOrigin::FontDirs);
        assert_eq!(primary.names().family, DEFAULT_FONT_FAMILY);
        assert_eq!(
            primary.handle.path_str().as_deref().map(Path::new).and_then(Path::file_name),
            Some(expected_file.as_ref())
        );
        assert!(!primary.synthesize_bold, "packaged bold face must be selected directly");
        assert!(!primary.synthesize_italic, "packaged italic face must be selected directly");
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
fn packaged_shaper_emits_nonzero_mark_offsets() {
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let stack = FontStack::try_new_full_with_weight_and_font_dirs(
        DEFAULT_FONT_FAMILY,
        15.0,
        96,
        1.0,
        &[fonts],
    )
    .expect("packaged font stack");
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

/// The coverage remap saturates: a stem pixel already at 255 cannot get
/// darker, which is why `weight_scale` was close to invisible at HiDPI where
/// stem cores are solid. Outline growth is the part that adds real ink, so it
/// must reach pixels the remap can never touch.
#[test]
fn embolden_puts_ink_where_the_coverage_remap_cannot() {
    // Single solid pixel surrounded by empty space.
    let coverage = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];

    // The remap leaves every zero pixel at zero, at any scale in range.
    let mut remapped = coverage.clone();
    apply_weight_scale(&mut remapped, 5.0, false);
    assert_eq!(remapped, coverage, "gamma remap cannot create ink");

    // Dilation spreads into those same pixels, inside the tile it was given.
    // The dimensions must not move: a weight control that resizes glyphs makes
    // every character change size when the user asks for more ink, and glyphs
    // from different fonts change by different amounts.
    let (grown, width, height, pad) =
        embolden_coverage(&coverage, 3, 3, 1.0, false).expect("radius 1.0 must dilate");
    assert_eq!(
        (width, height, pad),
        (3, 3, 0),
        "the tile keeps its dimensions and its origin, so the glyph gains weight without \
         gaining size"
    );
    let center = (height / 2) * width + (width / 2);
    assert_eq!(grown[center], 255);
    assert!(grown[center - 1] > 0, "ink must spread horizontally");
    assert!(grown[center + 1] > 0, "ink must spread horizontally");
    assert!(grown[center - width] > 0, "ink must spread vertically");
    assert!(grown[center + width] > 0, "ink must spread vertically");
}

/// Growth is independent of how much spare bitmap margin a glyph carries.
///
/// FreeType returns a tight control box. Flat-sided glyphs commonly touch one
/// edge while curved glyphs retain antialiasing margin, so using the nearest
/// spare margin as a bound makes the same weight scale act differently by glyph
/// shape. Crop-back is allowed at the touched side; in-bounds neighbours still
/// receive ink and tile geometry stays fixed.
#[test]
fn embolden_grows_an_asymmetric_glyph_that_touches_one_edge() {
    // 5x5 with a vertical stem on the left edge and room to its right.
    let mut coverage = vec![0u8; 25];
    for row in 1..4 {
        coverage[row * 5] = 255;
    }

    let (grown, width, height, pad) =
        embolden_coverage(&coverage, 5, 5, 1.0, false).expect("edge-touching stem must grow");
    assert_eq!((width, height, pad), (5, 5, 0), "weight must not resize or reposition the tile");
    assert!(
        grown[2 * 5 + 1] > 0,
        "the left-edge stem must spread into its in-bounds neighbour even though outward ink is cropped"
    );
}

#[test]
fn embolden_uses_a_literal_one_pixel_shape_independent_ceiling() {
    const EXPECTED_CEILING_PX: f64 = 1.0;
    assert_eq!(
        MAX_EMBOLDEN_RADIUS_PX, EXPECTED_CEILING_PX,
        "the documented crop-back ceiling is one raster pixel"
    );
    let inset = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];
    let (at_ceiling, width, height, pad) =
        embolden_coverage(&inset, 3, 3, EXPECTED_CEILING_PX, false)
            .expect("the one-pixel ceiling permits growth");
    assert_eq!((width, height, pad), (3, 3, 0));
    let (above_ceiling, ..) =
        embolden_coverage(&inset, 3, 3, 5.0, false).expect("large radius is capped");
    assert_eq!(
        at_ceiling, above_ceiling,
        "radii above one pixel must produce the same bounded crop-back result"
    );
}

#[test]
fn embolden_radius_scales_with_weight_and_is_zero_at_or_below_identity() {
    for scale in [0.5, 0.9, 1.0] {
        assert_eq!(embolden_radius_px(scale, 30.0), 0.0, "scale {scale} must not grow outlines");
    }
    let at_two = embolden_radius_px(2.0, 30.0);
    let at_five = embolden_radius_px(5.0, 30.0);
    assert!(at_two > 0.0);
    assert!(at_five > at_two, "higher weight must grow more");
    // Radius tracks cell height so the effect holds its proportion across DPI.
    assert!(embolden_radius_px(2.0, 60.0) > at_two);
    // Degenerate metrics disable growth instead of guessing.
    assert_eq!(embolden_radius_px(2.0, 0.0), 0.0);
    assert_eq!(embolden_radius_px(2.0, f64::NAN), 0.0);
}

#[test]
fn embolden_declines_work_it_cannot_do_safely() {
    let coverage = vec![255; 9];
    assert!(embolden_coverage(&coverage, 3, 3, 0.0, false).is_none(), "no radius, no work");
    assert!(embolden_coverage(&[], 0, 0, 1.0, false).is_none(), "empty glyph");
    assert!(embolden_coverage(&[0u8; 8], 3, 3, 1.0, false).is_none(), "short buffer");
    assert!(embolden_coverage(&[0u8; 10], 3, 3, 1.0, false).is_none(), "long buffer");
    assert!(embolden_coverage(&[0u8; 2], usize::MAX, 2, 1.0, false).is_none());

    let over = MAX_RASTERIZED_GLYPH_DIMENSION + 1;
    assert!(embolden_coverage(&vec![0u8; over], over, 1, 1.0, false).is_none());
}

#[test]
fn embolden_accepts_a_legal_final_tile_at_the_dimension_limit() {
    let width = MAX_RASTERIZED_GLYPH_DIMENSION;
    let mut coverage = vec![0u8; width];
    coverage[width / 2] = 255;

    let (grown, grown_width, grown_height, pad) =
        embolden_coverage(&coverage, width, 1, 1.0, false)
            .expect("bounded scratch padding must not reject a legal cropped result");

    assert_eq!((grown_width, grown_height, pad), (width, 1, 0));
    assert_eq!(grown.len(), coverage.len());
    assert!(grown[width / 2 - 1] > 0);
}

#[test]
fn embolden_recomputes_subpixel_alpha_from_dilated_rgb() {
    // 4x3 BGRA with a single lit pixel near the centre. Subpixel channels are
    // dilated independently and alpha is rebuilt from their envelope.
    let mut coverage = vec![0u8; 4 * 3 * 4];
    let (row, col, tile_w, bytes_per_px) = (1usize, 1usize, 4usize, 4usize);
    let centre = (row * tile_w + col) * bytes_per_px;
    coverage[centre..centre + 4].copy_from_slice(&[200, 100, 50, 200]);
    let (grown, width, height, _) =
        embolden_coverage(&coverage, 4, 3, 1.0, true).expect("subpixel dilation");
    assert_eq!(grown.len(), width * height * 4);
    for px in grown.as_chunks::<4>().0 {
        assert_eq!(px[3], px[0].max(px[1]).max(px[2]), "alpha must envelope RGB");
    }
}

/// Fractional radii blend the outer ring proportionally, so growth ramps
/// smoothly instead of snapping a whole pixel at a time as weight increases.
#[test]
fn embolden_fractional_radius_blends_rather_than_snapping() {
    let coverage = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];
    let (half, half_width, _, _) =
        embolden_coverage(&coverage, 3, 3, 0.5, false).expect("half radius");
    let (full, full_width, _, _) =
        embolden_coverage(&coverage, 3, 3, 1.0, false).expect("full radius");
    let half_neighbor = half[(half.len() / half_width / 2) * half_width + half_width / 2 + 1];
    let full_neighbor = full[(full.len() / full_width / 2) * full_width + full_width / 2 + 1];
    assert!(half_neighbor > 0, "fractional radius still spreads ink");
    assert!(half_neighbor < full_neighbor, "half radius must spread less than full");
}

/// The saturation ceiling cuts both ways: gamma cannot lighten a pixel that is
/// already fully opaque, so below 1.0 the outline has to shrink for thinning to
/// reach a solid stem core.
#[test]
fn erosion_removes_ink_the_coverage_remap_cannot() {
    // 5x5 with a solid 3x3 core — a stem thick enough to have an interior.
    let mut coverage = vec![0u8; 25];
    for row in 1..4 {
        for column in 1..4 {
            coverage[row * 5 + column] = 255;
        }
    }

    // The remap leaves every 255 exactly where it was, at any scale in range.
    let mut remapped = coverage.clone();
    apply_weight_scale(&mut remapped, 0.5, false);
    assert_eq!(remapped, coverage, "gamma remap cannot erode a solid core");

    // Erosion eats the rim of that core.
    let eroded = erode_coverage(&coverage, 5, 5, 1.0, false).expect("radius 1.0 must erode");
    let before: u32 = coverage.iter().map(|&byte| u32::from(byte)).sum();
    let after: u32 = eroded.iter().map(|&byte| u32::from(byte)).sum();
    assert!(after < before, "erosion must remove ink: {before} -> {after}");
    // The centre of a 3x3 core survives a radius-1 erosion; its rim does not.
    assert_eq!(eroded[2 * 5 + 2], 255, "core centre must survive");
    assert_eq!(eroded[5 + 1], 0, "core corner must erode");
}

/// A stem wide enough to have an interior must keep opaque pixels under a
/// light thin. Guards against an over-eager erosion that washes glyphs out
/// instead of slimming them.
#[test]
fn light_erosion_preserves_the_interior_of_a_thick_stem() {
    // 9x9 fully solid: every interior pixel is far from an edge.
    let coverage = vec![255u8; 81];
    let eroded = erode_coverage(&coverage, 9, 9, 0.05, false).expect("sub-pixel erosion");
    let centre = eroded[4 * 9 + 4];
    assert_eq!(centre, 255, "a sub-pixel thin must not touch a deep interior pixel");
    assert!(
        eroded.iter().filter(|&&byte| byte == 255).count() > 20,
        "most of a solid block must stay opaque under a light thin"
    );
}

#[test]
fn thin_radius_scales_with_weight_and_is_zero_at_or_above_identity() {
    for scale in [1.0, 1.5, 5.0] {
        assert_eq!(thin_radius_px(scale, 30.0), 0.0, "scale {scale} must not erode");
    }
    let at_09 = thin_radius_px(0.9, 30.0);
    let at_05 = thin_radius_px(0.5, 30.0);
    assert!(at_09 > 0.0);
    assert!(at_05 > at_09, "lower weight must erode more");
    assert_eq!(thin_radius_px(0.9, 0.0), 0.0);
    assert_eq!(thin_radius_px(0.9, f64::NAN), 0.0);
}

#[test]
fn erosion_declines_work_it_cannot_do_safely() {
    let coverage = vec![255u8; 9];
    assert!(erode_coverage(&coverage, 3, 3, 0.0, false).is_none(), "no radius, no work");
    assert!(erode_coverage(&[], 0, 0, 1.0, false).is_none(), "empty glyph");
    // Length that disagrees with the declared geometry is refused rather than
    // indexed past the end, in either direction.
    assert!(erode_coverage(&[0u8; 5], 3, 3, 1.0, false).is_none(), "short buffer");
    assert!(erode_coverage(&[0u8; 10], 3, 3, 1.0, false).is_none(), "long buffer");
    assert!(erode_coverage(&[0u8; 2], usize::MAX, 2, 1.0, false).is_none());
}

#[test]
fn erosion_keeps_tile_geometry_and_subpixel_alpha_consistent() {
    // Erosion only removes ink, so the buffer length must be preserved
    // exactly — the caller relies on dimensions and offsets staying valid.
    let coverage = vec![200u8; 4 * 4 * 4];
    let eroded = erode_coverage(&coverage, 4, 4, 0.5, true).expect("subpixel erosion");
    assert_eq!(eroded.len(), coverage.len(), "erosion must not resize the tile");
    for px in eroded.as_chunks::<4>().0 {
        assert_eq!(px[3], px[0].max(px[1]).max(px[2]), "alpha must envelope RGB");
    }
}

// Pixel meaning alone excludes color artwork; every monochrome face shares this conversion stage.
#[test]
fn weight_conversion_preserves_geometry_and_color_artwork() {
    let make_stack = |scale| {
        FontStack::try_new_with_font_dirs_for_test(
            &[(DEFAULT_FONT_FAMILY, false)],
            vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")],
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
        &[(DEFAULT_FONT_FAMILY, false)],
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")],
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

/// Fixtures for frame shaping that never waits on fallback discovery.
mod frame_fallback {
    use super::*;
    use sonicterm_font::locator::{FontDataHandle, FontDataSource, FontLocator, FontOrigin};
    use sonicterm_font::parser::ParsedFont;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// How long the locator waits for its gate, and how long a test waits for the worker.
    const WORKER_WAIT: Duration = Duration::from_secs(10);

    type Gate = Arc<(Mutex<bool>, Condvar)>;

    fn open(gate: &Gate) {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }

    /// Answers every fallback request with Rec Mono once `gate` opens; an unopened gate fails
    /// the request instead of hanging the worker.
    struct GatedRecMono {
        gate: Gate,
    }

    impl FontLocator for GatedRecMono {
        fn load_fonts(
            &self,
            _: &[config::FontAttributes],
            _: &mut std::collections::HashSet<config::FontAttributes>,
            _: u16,
        ) -> anyhow::Result<Vec<ParsedFont>> {
            Ok(Vec::new())
        }

        fn locate_fallback_for_codepoints(&self, _: &[char]) -> anyhow::Result<Vec<ParsedFont>> {
            let (opened, changed) = &*self.gate;
            let opened = changed
                .wait_timeout_while(opened.lock().unwrap(), WORKER_WAIT, |opened| !*opened)
                .unwrap()
                .0;
            anyhow::ensure!(*opened, "the test never opened the fallback gate");
            let handle = FontDataHandle {
                source: FontDataSource::OnDisk(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
                ),
                index: 0,
                variation: 0,
                origin: FontOrigin::BuiltIn,
                coverage: None,
            };
            Ok(vec![ParsedFont::from_locator(&handle)?])
        }
    }

    /// A stack whose only primary face is the ASCII-only sample font (family Roboto), so é reaches
    /// the gated locator; the temporary font directory is removed on drop.
    pub(super) struct GatedStack {
        pub(super) stack: FontStack,
        pub(super) gate: Gate,
        directory: PathBuf,
    }

    impl Drop for GatedStack {
        // Lifecycle: dropping `GatedStack` removes its temporary font `directory`.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    pub(super) fn gated_stack(name: &str) -> GatedStack {
        let directory =
            std::env::temp_dir().join(format!("sonicterm-engine-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c/test.ttf"),
            directory.join("primary.ttf"),
        )
        .unwrap();
        let gate: Gate = Arc::default();
        let stack = FontStack::try_new_with_locator_for_test(
            "Roboto",
            vec![directory.clone()],
            Arc::new(GatedRecMono { gate: Arc::clone(&gate) }),
            14.0,
            96,
        )
        .unwrap();
        GatedStack { stack, gate, directory }
    }

    pub(super) fn wait_for_generation(stack: &FontStack, generation: u64) {
        let started = Instant::now();
        while stack.fallback_notice().generation() < generation {
            assert!(started.elapsed() < WORKER_WAIT, "generation {generation} was never published");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait, bounded, until `frames` has received `count` wakes. The notice bumps its generation
    /// before it posts the wake, so a test that saw the generation must still wait for delivery.
    fn wait_for_wakes(frames: &Frames<'_>, count: usize) {
        let started = Instant::now();
        while frames.wakes() < count {
            assert!(started.elapsed() < WORKER_WAIT, "{count} wake(s) were never delivered");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait(gate: &Gate) {
        let (opened, changed) = &**gate;
        let opened = changed
            .wait_timeout_while(opened.lock().unwrap(), WORKER_WAIT, |opened| !*opened)
            .unwrap()
            .0;
        assert!(*opened, "a test gate was never opened");
    }

    /// A pause point for the worker: it opens `entered`, then waits until `release` opens.
    fn pause_hook(entered: &Gate, release: &Gate) -> Arc<dyn Fn() + Send + Sync> {
        let (entered, release) = (Arc::clone(entered), Arc::clone(release));
        Arc::new(move || {
            open(&entered);
            wait(&release);
        })
    }

    /// The renderer's frame-font contract in miniature: `begin` applies a newer notice generation
    /// once per frame and drops the stored title width; `title_width` measures only when nothing
    /// is stored; `wake_due` is the handler acknowledging the notice. The renderer's own seam
    /// (`prepare_frame_fonts`, `fallback_frame_due`) is tested in the GPU crate; this drives the
    /// same rules against the production frame entry points and a real worker.
    struct Frames<'stack> {
        stack: &'stack FontStack,
        applied: Option<(u64, u64)>,
        applies: usize,
        title_width: Option<f32>,
        wakes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl<'stack> Frames<'stack> {
        fn new(stack: &'stack FontStack) -> Self {
            let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = Arc::clone(&wakes);
            stack.fallback_notice().attach_waker(Arc::new(move |_notice| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
            Self { stack, applied: None, applies: 0, title_width: None, wakes }
        }

        fn begin(&mut self) -> bool {
            let notice = self.stack.fallback_notice();
            let current = (notice.id(), notice.generation());
            if self.applied == Some(current) {
                // When: `applied` already holds this generation, nothing can be stale.
                return false;
            }
            self.applied = Some(current);
            self.applies += 1;
            self.title_width = None;
            true
        }

        fn title_width(&mut self) -> f32 {
            let stack = self.stack;
            *self
                .title_width
                .get_or_insert_with(|| stack.measure_text_width_for_frame("é").unwrap())
        }

        fn wake_due(&self) -> bool {
            let notice = self.stack.fallback_notice();
            let generation = notice.acknowledge();
            self.applied != Some((notice.id(), generation))
        }

        fn wakes(&self) -> usize {
            self.wakes.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// The glyph id and advance a frame shapes for é.
    fn shaped_e(stack: &FontStack) -> (u32, f64) {
        let shaped = stack.shape_text_for_frame("é", false, false).unwrap();
        (shaped[0].glyph_pos, shaped[0].x_advance.get())
    }

    #[test]
    fn a_mid_frame_merge_is_corrected_by_the_frame_that_applies_its_generation() {
        // Frame N measures é's title with notdef's advance, then shapes é again after the worker
        // published it: the real glyph draws while the stored width lags. The completion's one
        // wake finds the generation unapplied, and frame N+1 applies it and remeasures.
        let fixture = gated_stack("mid-frame");
        let mut frames = Frames::new(&fixture.stack);
        assert!(frames.begin(), "frame N applies generation 0");
        let notdef_width = frames.title_width();
        assert_eq!(shaped_e(&fixture.stack).0, 0, "é is notdef before publication");
        open(&fixture.gate);
        wait_for_generation(&fixture.stack, 1);
        let (glyph, advance) = shaped_e(&fixture.stack);
        assert_ne!(glyph, 0, "frame N merges the published handles and shapes the real glyph");
        assert_eq!(frames.title_width(), notdef_width, "the stored width lags within frame N");
        wait_for_wakes(&frames, 1);
        assert_eq!(frames.wakes(), 1, "the completion posts exactly one wake");
        assert!(frames.wake_due(), "the handler sees generation 1 unapplied");
        assert!(frames.begin(), "frame N+1 applies generation 1");
        assert_eq!(frames.applies, 2);
        let real_width = frames.title_width();
        assert_ne!(real_width, notdef_width, "frame N+1 remeasures with the real face");
        assert!((f64::from(real_width) - advance).abs() < 0.5, "widths and glyphs agree");
        assert_eq!(shaped_e(&fixture.stack).0, glyph);
        assert!(!frames.wake_due(), "an applied generation needs no further frame");
    }

    #[test]
    fn a_frame_shaping_while_the_worker_holds_the_lock_keeps_notdef_until_the_next_frame() {
        // The worker pauses while holding `pending_fallback` mid-append, so frame N's shape skips
        // the merge and returns notdef at once; after release the next frame applies and resolves.
        let fixture = gated_stack("lock-busy");
        let (entered, release): (Gate, Gate) = (Arc::default(), Arc::default());
        fixture.stack.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
            during_append: Some(pause_hook(&entered, &release)),
            before_completion: None,
        });
        let mut frames = Frames::new(&fixture.stack);
        assert!(frames.begin());
        let notdef_width = frames.title_width();
        open(&fixture.gate);
        wait(&entered);
        let started = Instant::now();
        assert_eq!(shaped_e(&fixture.stack).0, 0, "a busy lock skips the merge");
        assert!(started.elapsed() < Duration::from_secs(5), "frame shaping waited on the worker");
        assert_eq!(frames.wakes(), 0, "nothing completes while the worker holds the lock");
        open(&release);
        wait_for_generation(&fixture.stack, 1);
        assert!(frames.wake_due());
        assert!(frames.begin(), "the next frame applies generation 1");
        assert_ne!(frames.title_width(), notdef_width);
        assert_ne!(shaped_e(&fixture.stack).0, 0);
    }

    #[test]
    fn frames_between_the_unlock_and_the_completion_lag_until_the_published_generation() {
        // The worker unlocks, then pauses before completing: frames prepared in that window draw
        // the real glyph with the old width and post no wake. Releasing the completion posts one
        // wake, and the frame that applies its generation remeasures.
        let fixture = gated_stack("paused-completion");
        let (entered, release): (Gate, Gate) = (Arc::default(), Arc::default());
        fixture.stack.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
            during_append: None,
            before_completion: Some(pause_hook(&entered, &release)),
        });
        let mut frames = Frames::new(&fixture.stack);
        assert!(frames.begin(), "frame N applies generation 0");
        let notdef_width = frames.title_width();
        open(&fixture.gate);
        wait(&entered);
        let real_glyph = shaped_e(&fixture.stack).0;
        assert_ne!(real_glyph, 0, "frame N merges handles published before the completion");
        for _ in 0..2 {
            assert!(!frames.begin(), "generation 0 is still current, so nothing is applied");
            assert_eq!(frames.title_width(), notdef_width, "the width lags");
            assert_eq!(shaped_e(&fixture.stack).0, real_glyph, "the real glyph draws");
        }
        assert_eq!(frames.wakes(), 0, "no wake before the completion");
        open(&release);
        wait_for_generation(&fixture.stack, 1);
        wait_for_wakes(&frames, 1);
        assert_eq!(frames.wakes(), 1, "the completion posts exactly one wake");
        assert!(frames.wake_due());
        assert!(frames.begin(), "the next frame applies generation 1");
        assert_ne!(frames.title_width(), notdef_width, "and remeasures");
    }

    #[test]
    fn frame_shaping_and_measuring_return_at_once_while_fallback_is_blocked() {
        // The renderer's frame entry points never wait for discovery: with the locator blocked, é
        // shapes as notdef and measures at once; after publication a later frame shapes the real glyph.
        let fixture = gated_stack("frame");
        let started = Instant::now();
        let shaped = fixture.stack.shape_text_for_frame("é", false, false).unwrap();
        assert_eq!(shaped[0].glyph_pos, 0, "notdef while the locator is blocked");
        assert!(fixture.stack.measure_text_width_for_frame("é").unwrap() > 0.0);
        assert!(started.elapsed() < Duration::from_secs(5), "frame shaping waited");
        open(&fixture.gate);
        wait_for_generation(&fixture.stack, 1);
        let resolved = fixture.stack.shape_text_for_frame("é", false, false).unwrap();
        assert_ne!(resolved[0].glyph_pos, 0, "a later frame shapes the published face");
    }

    #[test]
    fn rasterizing_an_unresolved_glyph_returns_none_without_waiting() {
        // A glyph-0 atlas miss shapes for the frame: while é is unresolved it rasterizes to `None` at
        // once (the atlas caches that as missing), and after publication it rasterizes the real glyph.
        let mut fixture = gated_stack("raster");
        let started = Instant::now();
        assert!(fixture.stack.rasterize(GlyphKey::new('é', false, false)).is_none());
        assert!(started.elapsed() < Duration::from_secs(5), "rasterize waited for fallback");
        open(&fixture.gate);
        wait_for_generation(&fixture.stack, 1);
        let tile = fixture.stack.rasterize(GlyphKey::new('é', false, false)).expect("resolved");
        assert!(tile.width > 0 && tile.height > 0);
    }

    #[test]
    fn a_space_is_an_empty_tile_and_a_malformed_buffer_is_none() {
        // A space is a valid empty glyph, so it rasterizes to an empty tile with no coverage, never
        // `None`; a buffer whose length does not match its size stays `None`.
        let mut fixture = gated_stack("space");
        let space =
            fixture.stack.rasterize(GlyphKey::new(' ', false, false)).expect("a space is valid");
        assert_eq!(space.width * space.height, 0);
        assert!(space.coverage.is_empty());
        let raster = |data: Vec<u8>, width: usize, height: usize| sonicterm_font::RasterizedGlyph {
            data,
            width,
            height,
            bearing_x: sonicterm_font::units::PixelLength::new(1.0),
            bearing_y: sonicterm_font::units::PixelLength::new(2.0),
            has_color: false,
            is_scaled: true,
        };
        let empty = fixture.stack.rasterized_glyph_to_tile(raster(Vec::new(), 0, 0)).unwrap();
        assert_eq!((empty.offset_x, empty.offset_y, empty.advance), (1, -2, 0.0));
        assert!(fixture.stack.rasterized_glyph_to_tile(raster(vec![0; 5], 3, 2)).is_none());
        assert!(fixture.stack.rasterized_glyph_to_tile(raster(vec![0; 4], 0, 0)).is_none());
    }
}

/// The diagnostic face resolution mirrors the rasterizer: a character key (glyph id 0) and the
/// shaped key for the same glyph resolve to one face, one glyph id and one strike, and the strike
/// is the loaded face's raster size in thousandths of a pixel (14 pt at 96 DPI is 18.667 px).
#[test]
fn a_character_key_and_its_shaped_key_resolve_to_one_face_and_strike() {
    let fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
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
