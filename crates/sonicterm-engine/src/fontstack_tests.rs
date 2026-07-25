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

    // Dilation grows the glyph outward into those same pixels.
    let (grown, w, h, pad) =
        embolden_coverage(&coverage, 3, 3, 1.0, false).expect("radius 1.0 must dilate");
    assert_eq!((w, h, pad), (5, 5, 1));
    let center = (h / 2) * w + (w / 2);
    assert_eq!(grown[center], 255);
    assert!(grown[center - 1] > 0, "ink must spread horizontally");
    assert!(grown[center + 1] > 0, "ink must spread horizontally");
    assert!(grown[center - w] > 0, "ink must spread vertically");
    assert!(grown[center + w] > 0, "ink must spread vertically");
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
    // A tile that would exceed the atlas dimension limit is refused rather
    // than producing a buffer the atlas must reject later.
    let big = MAX_RASTERIZED_GLYPH_DIMENSION;
    assert!(embolden_coverage(&vec![0u8; 4], big, 1, 2.0, false).is_none());
}

#[test]
fn embolden_recomputes_subpixel_alpha_from_dilated_rgb() {
    // 2x1 BGRA: one lit pixel, one empty.
    let coverage = vec![200, 100, 50, 200, 0, 0, 0, 0];
    let (grown, w, h, _) =
        embolden_coverage(&coverage, 2, 1, 1.0, true).expect("subpixel dilation");
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
