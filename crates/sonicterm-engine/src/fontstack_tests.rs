use super::*;

#[test]
fn regular_weight_scale_preserves_identity_and_extremes() {
    let original = vec![0, 1, 64, 128, 254, 255];
    let mut coverage = original.clone();
    apply_regular_weight_scale(&mut coverage, 1.0, false);
    assert_eq!(coverage, original);

    let mut stronger = original.clone();
    apply_regular_weight_scale(&mut stronger, 1.1, false);
    assert_eq!(stronger[0], 0);
    assert_eq!(*stronger.last().unwrap(), 255);
    assert!(stronger[2] > original[2]);
    assert!(stronger[3] > original[3]);

    let mut lighter = original.clone();
    apply_regular_weight_scale(&mut lighter, 0.9, false);
    assert!(lighter[2] < original[2]);
    assert!(lighter[3] < original[3]);
}

#[test]
fn subpixel_weight_scale_recomputes_alpha_from_rgb_coverage() {
    let mut coverage = vec![32, 64, 96, 96, 0, 0, 0, 0];
    apply_regular_weight_scale(&mut coverage, 1.1, true);
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
    let cfg = build_config("Rec Mono St.Helens", 17.0, &["Symbols Nerd Font Mono"]);
    assert_eq!(cfg.font_size, 17.0);
    assert_eq!(cfg.font.font[0].family, "Rec Mono St.Helens");
    assert_eq!(cfg.font.font[1].family, "Symbols Nerd Font Mono");
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
        Ok(s) => s,
        // No usable font in this sandbox; the bundled-font CI gate covers
        // the real assertion. Nothing to verify here.
        Err(_) => return,
    };
    let base = match stack.cell_metrics_raster_px() {
        Ok(m) => m,
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

#[test]
fn shaped_text_width_covers_mixed_ascii_cjk_and_status_text() {
    let stack = match FontStack::try_new(72) {
        Ok(s) => s,
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
    apply_regular_weight_scale(&mut remapped, 5.0, false);
    assert_eq!(remapped, coverage, "gamma remap cannot create ink");

    // Dilation spreads into those same pixels, inside the tile it was given.
    // The dimensions must not move: a weight control that resizes glyphs makes
    // every character change size when the user asks for more ink, and glyphs
    // from different fonts change by different amounts.
    let (grown, w, h, pad) =
        embolden_coverage(&coverage, 3, 3, 1.0, false).expect("radius 1.0 must dilate");
    assert_eq!(
        (w, h, pad),
        (3, 3, 0),
        "the tile keeps its dimensions and its origin, so the glyph gains weight without \
         gaining size"
    );
    let center = (h / 2) * w + (w / 2);
    assert_eq!(grown[center], 255);
    assert!(grown[center - 1] > 0, "ink must spread horizontally");
    assert!(grown[center + 1] > 0, "ink must spread horizontally");
    assert!(grown[center - w] > 0, "ink must spread vertically");
    assert!(grown[center + w] > 0, "ink must spread vertically");
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

    let (grown, w, h, pad) =
        embolden_coverage(&coverage, 5, 5, 1.0, false).expect("edge-touching stem must grow");
    assert_eq!((w, h, pad), (5, 5, 0), "weight must not resize or reposition the tile");
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
    let (at_ceiling, w, h, pad) = embolden_coverage(&inset, 3, 3, EXPECTED_CEILING_PX, false)
        .expect("the one-pixel ceiling permits growth");
    assert_eq!((w, h, pad), (3, 3, 0));
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

    let (grown, w, h, pad) = embolden_coverage(&coverage, width, 1, 1.0, false)
        .expect("bounded scratch padding must not reject a legal cropped result");

    assert_eq!((w, h, pad), (width, 1, 0));
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
    let (grown, w, h, _) =
        embolden_coverage(&coverage, 4, 3, 1.0, true).expect("subpixel dilation");
    assert_eq!(grown.len(), w * h * 4);
    for px in grown.chunks_exact(4) {
        assert_eq!(px[3], px[0].max(px[1]).max(px[2]), "alpha must envelope RGB");
    }
}

/// Fractional radii blend the outer ring proportionally, so growth ramps
/// smoothly instead of snapping a whole pixel at a time as weight increases.
#[test]
fn embolden_fractional_radius_blends_rather_than_snapping() {
    let coverage = vec![0, 0, 0, 0, 255, 0, 0, 0, 0];
    let (half, w, _, _) = embolden_coverage(&coverage, 3, 3, 0.5, false).expect("half radius");
    let (full, fw, _, _) = embolden_coverage(&coverage, 3, 3, 1.0, false).expect("full radius");
    let half_neighbor = half[(half.len() / w / 2) * w + w / 2 + 1];
    let full_neighbor = full[(full.len() / fw / 2) * fw + fw / 2 + 1];
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
    for y in 1..4 {
        for x in 1..4 {
            coverage[y * 5 + x] = 255;
        }
    }

    // The remap leaves every 255 exactly where it was, at any scale in range.
    let mut remapped = coverage.clone();
    apply_regular_weight_scale(&mut remapped, 0.5, false);
    assert_eq!(remapped, coverage, "gamma remap cannot erode a solid core");

    // Erosion eats the rim of that core.
    let eroded = erode_coverage(&coverage, 5, 5, 1.0, false).expect("radius 1.0 must erode");
    let before: u32 = coverage.iter().map(|&v| u32::from(v)).sum();
    let after: u32 = eroded.iter().map(|&v| u32::from(v)).sum();
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
        eroded.iter().filter(|&&v| v == 255).count() > 20,
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
    for px in eroded.chunks_exact(4) {
        assert_eq!(px[3], px[0].max(px[1]).max(px[2]), "alpha must envelope RGB");
    }
}

/// `weight_scale` must reach the configured family's glyphs.
///
/// The setting exists to change that font's weight, so a build that gated it
/// away entirely would be no fix at all — it would trade a wrong-glyph bug for
/// a dead feature.
#[test]
fn weight_scale_acts_on_the_configured_family() {
    assert!(
        weight_scale_applies(false, false, true),
        "a regular glyph from the configured font is exactly what the setting names"
    );
}

/// And must not reach glyphs from any other font.
///
/// This is the defect the gate closes. A fallback glyph is drawn at the weight
/// its own designer chose, in a family the user never configured; scaling it
/// applies the user's intent for one font to a different one. The visible
/// result is a fallback glyph growing or thinning while its neighbour from the
/// configured family stays put.
#[test]
fn weight_scale_leaves_fallback_fonts_alone() {
    assert!(
        !weight_scale_applies(false, false, false),
        "a fallback font is one the user never configured, so the weight setting for \
         their own family must not touch it"
    );
}

/// Provenance is asked of the font, not inferred from the handle index.
///
/// Resolution pushes a handle only when a family actually matches, so a
/// configured family that fails to load is absent entirely and the first
/// fallback inherits index 0. A gate written `font_idx == 0` would then report
/// "configured" for a font the user never named — reweighting it while
/// claiming to protect it, in the one case where the distinction matters most.
///
/// This pins that the predicate takes the answer rather than deriving it: a
/// fallback at index 0 must still be excluded.
#[test]
fn a_fallback_that_inherited_index_zero_is_still_excluded() {
    // What `is_configured_family(0)` returns when the configured family
    // failed to load and a fallback took the first slot.
    let fallback_at_index_zero = false;
    assert!(
        !weight_scale_applies(false, false, fallback_at_index_zero),
        "when the configured family fails to load, the fallback that inherits index 0 is \
         still not the user's font, and an index-based gate would get this wrong"
    );
}

/// The two exclusions that predate this gate must survive it.
///
/// Colour glyphs carry artwork rather than a weight, and an SGR-bold glyph has
/// already had a bold face resolved for it — scaling on top would compound two
/// weight changes. Both were correct before and are unrelated to the fallback
/// question, so a fix that dropped either would be a regression smuggled in
/// beside a fix.
#[test]
fn colour_and_bold_glyphs_stay_excluded_even_from_the_configured_family() {
    assert!(
        !weight_scale_applies(true, false, true),
        "a colour glyph carries its own artwork; remapping coverage alters the picture"
    );
    assert!(
        !weight_scale_applies(false, true, true),
        "SGR bold already resolved a bold face; scaling it again compounds two changes"
    );
    assert!(!weight_scale_applies(true, true, true), "both exclusions together must still exclude");
}

/// The gate governs fixed-tile outline growth, not only the coverage remap.
///
/// This is the assertion a helper-level test misses. `rasterize` runs two
/// mechanisms behind this single gate: the coverage remap and
/// `embolden_coverage`, which max-filters the outline inside padded scratch
/// space before cropping back to the original tile. The second adds real ink
/// while width, height, origin, and advance remain fixed.
///
/// A fix that gated only the remap would leave a fallback glyph still being
/// dilated. Pinning that the growth radius is non-zero at a raised weight makes
/// this test fail against that half-fix rather than pass it.
#[test]
fn the_gate_governs_outline_growth_and_not_just_the_coverage_remap() {
    // A weight the user reaches in four keypresses at 0.25 per step.
    let scale = 2.0_f32;
    let cell_h = 28.0_f64;

    // Precondition: at this weight the growth is real, so gating it matters.
    let radius = embolden_radius_px(scale, cell_h);
    assert!(
        radius > 0.0,
        "test setup: weight {scale} must produce real outline growth, or this test cannot \
         distinguish a fix that gates the growth from one that does not"
    );

    // Both mechanisms sit behind one predicate, so one answer decides both.
    assert!(
        weight_scale_applies(false, false, true),
        "the configured family gets both the remap and the growth"
    );
    assert!(
        !weight_scale_applies(false, false, false),
        "a fallback glyph gets neither — including fixed-tile outline growth"
    );
}
