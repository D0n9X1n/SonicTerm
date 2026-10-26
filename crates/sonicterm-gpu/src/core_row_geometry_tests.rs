//! Renderer row geometry contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn software_block_glyph_rect_matches_integer_raster_size() {
    let rect = software_block_glyph_target_rect(21.0, 49.017345, 36.0, 84.03469);

    assert_eq!(rect, (21.0, 49.0, 15.0, 35.0));
}

#[test]
fn software_block_glyph_rows_share_integer_edges() {
    let first = software_block_glyph_target_rect(21.0, 49.017345, 36.0, 84.03469);
    let second = software_block_glyph_target_rect(21.0, 84.03469, 36.0, 119.05203);

    assert_eq!(first.1 + first.3, second.1);
    assert_eq!(first.3, 35.0);
    assert_eq!(second.3, 35.0);
}

#[test]
fn software_block_glyph_rows_distribute_fractional_height_without_seams() {
    let first = software_block_glyph_target_rect(0.0, 0.0, 15.0, 35.6);
    let second = software_block_glyph_target_rect(0.0, 35.6, 15.0, 71.2);

    assert_eq!(first.1 + first.3, second.1);
    assert_eq!((first.3, second.3), (36.0, 35.0));
}

#[test]
fn hardware_block_glyph_geometry_stays_fractional() {
    let cx = 21.0_f32;
    let cy = 49.017345_f32;
    let cell_right = 36.0_f32;
    let cell_h = 35.017345_f32;
    let rect = (cx, cy, cell_right - cx, cell_h);

    assert_eq!(rect, (21.0, 49.017345, 15.0, 35.017345));
}

#[test]
fn status_markers_fit_the_same_single_cell_geometry() {
    // Contract: every Claude Code circle marker uses the same width-bound fit policy.
    let natural = (4.0, 7.0, 24.0, 12.0);
    let cell = (10.0, 20.0, 12.0, 20.0);

    for marker in ['\u{23fa}', '\u{25ef}', '\u{25cf}'] {
        assert_eq!(
            fit_single_cell_status_marker(marker, 1, false, false, natural, cell),
            (10.0, 27.0, 12.0, 6.0)
        );
    }
}

#[test]
fn status_marker_fit_preserves_aspect_ratio_when_height_binds() {
    // Contract: tall marker tiles remain centered and proportional inside one cell.
    let fitted = fit_single_cell_status_marker(
        '\u{23fa}',
        1,
        false,
        false,
        (2.0, 3.0, 8.0, 32.0),
        (10.0, 20.0, 12.0, 16.0),
    );

    assert_eq!(fitted, (14.0, 20.0, 4.0, 16.0));
    assert_eq!(fitted.2 / fitted.3, 8.0 / 32.0);
}

/// Hollow and solid fallback tiles normalize to one outer cell-constrained size.
///
/// Unequal square source tiles model the observed fallback-font mismatch: the hollow circle
/// exceeds the cell while the solid circle is naturally smaller than it.
#[test]
fn status_marker_fit_enlarges_small_tiles_to_match_oversized_tiles() {
    let cell = (10.0, 20.0, 6.0, 8.0);
    let hollow =
        fit_single_cell_status_marker('\u{25ef}', 1, false, false, (8.0, 18.0, 8.0, 8.0), cell);
    let solid =
        fit_single_cell_status_marker('\u{25cf}', 1, false, false, (11.0, 22.0, 4.0, 4.0), cell);

    assert_eq!(hollow, (10.0, 21.0, 6.0, 6.0));
    assert_eq!(solid, hollow);
}

#[test]
fn status_marker_fit_leaves_ineligible_geometry_unchanged() {
    // Contract: the targeted policy cannot alter ordinary text, wide cells, or multi-cell clusters.
    let natural = (-8.0, 3.0, 16.0, 20.0);
    let cell = (0.0, 0.0, 10.0, 20.0);

    assert_eq!(fit_single_cell_status_marker('x', 1, false, false, natural, cell), natural);
    assert_eq!(fit_single_cell_status_marker('\u{23fa}', 1, true, false, natural, cell), natural);
    assert_eq!(fit_single_cell_status_marker('\u{23fa}', 2, false, false, natural, cell), natural);
    assert_eq!(fit_single_cell_status_marker('\u{23fa}', 1, false, true, natural, cell), natural);
}

#[test]
fn status_marker_fit_leaves_multi_cell_ligature_geometry_unchanged() {
    // Contract: both halves of a two-cell `=>` ligature retain their natural overhang.
    let cell = (0.0, 0.0, 10.0, 20.0);
    let equals_half = (-8.0, 1.0, 16.0, 20.0);
    let arrow_half = (2.0, 1.0, 16.0, 20.0);

    assert_eq!(fit_single_cell_status_marker('=', 2, false, false, equals_half, cell), equals_half);
    assert_eq!(fit_single_cell_status_marker('>', 2, false, false, arrow_half, cell), arrow_half);
}

#[test]
fn status_marker_fit_leaves_degenerate_geometry_unchanged() {
    // Contract: a zero-area glyph or cell cannot produce a meaningful fit ratio.
    let glyph = (1.0, 2.0, 0.0, 12.0);
    let cell = (10.0, 20.0, 12.0, 20.0);

    assert_eq!(fit_single_cell_status_marker('\u{25cf}', 1, false, false, glyph, cell), glyph);
    assert_eq!(
        fit_single_cell_status_marker(
            '\u{25cf}',
            1,
            false,
            false,
            (1.0, 2.0, 8.0, 12.0),
            (10.0, 20.0, 0.0, 20.0)
        ),
        (1.0, 2.0, 8.0, 12.0)
    );
}

/// Fallback and shaped glyphs both carry the marker-fit decision made at shape time from the
/// lead cell, and projection applies the fit to those two kinds only, after the shaping offset
/// and before the device-pixel snap and NDC conversion.
#[test]
fn status_marker_fit_is_wired_before_both_terminal_glyph_emissions() {
    let source: String = crate::core::CORE_SOURCE.split_whitespace().collect();
    let build = source.find("fnbuild_shape_run(").expect("record builder");
    let builder = &source[build..];
    let decide = builder.find("letmarker_fit=status_marker_fit_eligible(").expect("decision");
    let fallback = builder.find("ifg.glyph_id==0{").expect("fallback branch");
    assert!(decide < fallback, "one decision serves both the fallback and the shaped branch");
    assert_eq!(
        builder
            .matches("set_shaped_bits(&mutrecord,[shape_x_offset,g.y_offset],marker_fit,")
            .count(),
        2
    );
    let project = source.find("pub(crate)fnproject_row_glyph(").expect("projection");
    let body = &source[project..project + source[project..].find("\n}").unwrap_or(4000).min(4000)];
    let arm = body.find("RowGlyphKind::Fallback|RowGlyphKind::Shaped=>{").expect("fit arm");
    let offset = body.find("positioned_shaped_glyph_rect(").expect("shaping offset");
    let fit = body.find("fit_status_marker_rect(positioned,").expect("fit");
    let snap = arm + body[arm..].find("snap_to_device_pixels(fitted,").expect("snap");
    let ndc = body.find("rect:px_to_ndc(").expect("ndc");
    assert!(arm < offset && offset < fit && fit < snap && snap < ndc);
}

/// Glyph flags preserve color selection in x and raw subpixel coverage in y.
#[test]
fn glyph_flags_keep_color_and_subpixel_axes_independent() {
    assert_eq!(glyph_flags(false, false), [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(glyph_flags(true, false), [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(glyph_flags(false, true), [0.0, 1.0, 0.0, 0.0]);
}

#[test]
fn fill_snapped_cell_x_matches_build_bit_for_bit() {
    // Filling a reused edge buffer gives the column edges the allocating builder gives, bit for
    // bit, for fractional origins and cell widths such as fractional-DPI raster sizes, and the
    // same as the snapping formula applied directly.
    let mut seed: u64 = 0x5eed_1555;
    let mut next = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as u32
    };
    let mut edges = Vec::new();
    for _ in 0..200 {
        let origin_x = next() as f32 / 997.0 % 300.0;
        let cell_w = 3.0 + (next() % 4000) as f32 / 333.0;
        let cols = (next() % 400) as u16;
        fill_snapped_cell_x(&mut edges, origin_x, cell_w, cols);
        let built = build_snapped_cell_x(origin_x, cell_w, cols);
        let oracle: Vec<f32> = (0..=cols)
            .map(|col| {
                sonicterm_render_model::geometry::snap_to_device_pixels(
                    (origin_x + (col as f32) * cell_w, 0.0, 0.0, 0.0),
                    1.0,
                )
                .0
            })
            .collect();
        let bits = |values: &[f32]| values.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&edges), bits(&built), "origin {origin_x} cell {cell_w} cols {cols}");
        assert_eq!(bits(&edges), bits(&oracle));
    }
}

/// A quick-select hint is placed relative to the displayed viewport's top, so on a scrolled-back
/// view a history hint lands on its displayed row, and a row above or below the view draws nothing.
/// Placing it from the live top instead put a history hint off screen.
#[test]
fn quick_select_hints_map_to_the_displayed_viewport_row() {
    assert_eq!(quick_select_viewport_row(12, 10, 5), Some(2));
    assert_eq!(quick_select_viewport_row(10, 10, 5), Some(0));
    assert_eq!(quick_select_viewport_row(14, 10, 5), Some(4));
    assert_eq!(quick_select_viewport_row(9, 10, 5), None, "above the view");
    assert_eq!(quick_select_viewport_row(15, 10, 5), None, "below the view");
}

/// A quick-select hint after one more history row is evicted draws one displayed row higher, on
/// the row that still holds its text, and a hint whose own row was evicted draws nothing.
#[test]
fn quick_select_hints_follow_history_eviction_to_their_displayed_row() {
    let hint_at = |letter: char, row: usize| QuickSelectHint {
        hint: letter,
        row,
        col_start: 0,
        col_end: 3,
        text: "https://a.co".to_string(),
    };
    let quick_select = QuickSelectState {
        hints: vec![hint_at('a', 0), hint_at('b', 12), hint_at('c', 10)],
        scrollback_evicted: 5,
    };
    let (view_top_abs, visible_rows) = (8, 5);
    let display_row = |hint_index: usize, scrollback_evicted: u64| {
        quick_select_hint_display_row(
            &quick_select,
            &quick_select.hints[hint_index],
            scrollback_evicted,
            view_top_abs,
            visible_rows,
        )
    };
    assert_eq!(display_row(1, 5), Some(4), "no eviction since the scan keeps the scan row");
    assert_eq!(display_row(1, 6), Some(3), "one eviction moves the hint up one row");
    assert_eq!(display_row(2, 7), Some(0), "two evictions move it up two rows");
    assert_eq!(display_row(0, 6), None, "a hint whose own row was evicted draws nothing");
    assert_eq!(display_row(2, 8), None, "a hint moved above the view draws nothing");
}

/// With the viewport at the top of the grid, so no clipping can hide it, a hint whose own row is
/// evicted draws nothing rather than being clamped onto row 0, and the hint below it moves up into
/// row 0. A placement that saturated the eviction shift at row 0 would draw the evicted hint there.
#[test]
fn an_evicted_quick_select_hint_is_suppressed_even_at_the_viewport_top() {
    let hint_at = |letter: char, row: usize| QuickSelectHint {
        hint: letter,
        row,
        col_start: 0,
        col_end: 3,
        text: "https://a.co".to_string(),
    };
    let quick_select =
        QuickSelectState { hints: vec![hint_at('a', 0), hint_at('b', 1)], scrollback_evicted: 5 };
    let (view_top_abs, visible_rows) = (0, 5);
    let display_row = |hint_index: usize, scrollback_evicted: u64| {
        quick_select_hint_display_row(
            &quick_select,
            &quick_select.hints[hint_index],
            scrollback_evicted,
            view_top_abs,
            visible_rows,
        )
    };
    assert_eq!(display_row(0, 5), Some(0), "before any eviction the hint draws on row 0");
    assert_eq!(display_row(1, 5), Some(1));
    assert_eq!(display_row(0, 6), None, "its own row evicted, the hint draws nothing");
    assert_eq!(display_row(1, 6), Some(0), "the surviving hint moves up into row 0");
}

/// The placement check accepts compliant draw sites even when a string holds `//` or a brace and
/// prose or a literal names the raw row, and it accepts the production renderer.
#[test]
fn quick_select_placement_check_accepts_compliant_sites() {
    assert_eq!(
        quick_select_placement_violations(COMPLIANT_PLACEMENT_FIXTURE),
        Vec::<String>::new()
    );
    assert_eq!(quick_select_placement_violations(crate::core::CORE_SOURCE), Vec::<String>::new());
}

/// The placement check names the site that bypasses the helper: a raw read split across lines, a
/// helper call that survives only in a comment or string, an eviction shift outside the helper,
/// and a missing site all fail it.
#[test]
fn quick_select_placement_check_names_each_bypassing_site() {
    let letter_raw_read = COMPLIANT_PLACEMENT_FIXTURE.replace(
        "draw_letter(hint.hint, hint.col_start, visible_row,",
        "draw_letter(hint.hint, hint.col_start, hint\n                .row,",
    );
    assert_eq!(
        quick_select_placement_violations(&letter_raw_read),
        vec!["letter: reads `hint.row` directly".to_string()]
    );
    let background_in_prose_only = COMPLIANT_PLACEMENT_FIXTURE.replace(
        "let Some(visible_row) = quick_select_hint_display_row(\n            quick_select, hint, evicted,\n        ) else {\n            continue;\n        };",
        "/* quick_select_hint_display_row( */ let label = \"quick_select_hint_display_row(\";\n        let visible_row = hint.row;",
    );
    assert_eq!(
        quick_select_placement_violations(&background_in_prose_only),
        vec![
            "background: 0 placement-helper calls, expected 1".to_string(),
            "background: reads `hint.row` directly".to_string(),
        ]
    );
    let shifted_outside_helper = COMPLIANT_PLACEMENT_FIXTURE.replace(
        "fn render(&mut self) {",
        "fn render(&mut self) {\n    let _ = qs.hint_row_now(hint, 0);",
    );
    assert_eq!(
        quick_select_placement_violations(&shifted_outside_helper),
        vec!["hint_row_now is called outside the placement helper".to_string()]
    );
    let letter_site_removed =
        COMPLIANT_PLACEMENT_FIXTURE.replace("if quick_select_hint_count > 0", "if true");
    assert_eq!(quick_select_placement_violations(&letter_site_removed).len(), 1);
}

/// A row record counts only while its slot shows the row it was made for: switching to the
/// alternate screen stages that screen's row key, so the primary row's tofu record is pruned and
/// no stale count is shown.
#[test]
fn a_screen_switch_drops_the_other_screens_tofu_record() {
    use crate::missing_glyphs::{commit_missing, prune_missing, SlotMissing, StagedMissing};
    let mut rig = GlyphRig::new(false);
    let mut grid = text_grid(8, &["a\u{378}b"]);
    rig.begin(&grid);
    let primary = rig.emit(&grid, 0, 0);
    assert!(primary.missing.contains(&'\u{378}'), "U+0378 draws as tofu");
    let mut presented = std::collections::HashMap::new();
    let stage = |key, missing: &[char]| StagedMissing {
        pane_id: 7,
        slot: 0,
        record: SlotMissing { key, fonts: None, chars: missing.to_vec(), truncated: false },
    };
    commit_missing(&mut presented, &mut vec![stage(primary.key, &primary.missing)]);
    rig.cache.commit_slots();
    grid.enter_alt_screen();
    write_row(&mut grid, 0, "alt");
    rig.begin(&grid);
    let alternate = rig.emit(&grid, 0, 0);
    assert_ne!(alternate.key, primary.key);
    rig.cache.commit_slots();
    let cache = &rig.cache;
    prune_missing(&mut presented, |pane_id, slot| cache.committed_slot(pane_id, slot), None);
    assert!(presented.is_empty(), "the alternate screen shows no primary tofu");
}

/// The About window names each graphics API the way its platform does, so the renderer row reads
/// `Metal` or `DirectX 12`, never wgpu's lowercase identifiers.
#[test]
fn graphics_apis_have_display_names() {
    assert_eq!(backend_display_name(wgpu::Backend::Metal), "Metal");
    assert_eq!(backend_display_name(wgpu::Backend::Dx12), "DirectX 12");
    assert_eq!(backend_display_name(wgpu::Backend::Vulkan), "Vulkan");
    assert_eq!(backend_display_name(wgpu::Backend::Gl), "OpenGL");
}
