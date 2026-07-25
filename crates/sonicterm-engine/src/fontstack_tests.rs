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
    for scale in [f32::NAN, f32::INFINITY, 0.0, 0.49, 2.01] {
        assert_eq!(sanitize_weight_scale(scale), 1.0);
    }
    assert_eq!(sanitize_weight_scale(1.1), 1.1);
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
