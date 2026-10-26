//! Renderer frame identity contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn dirty_rows_damage_rect_unions_and_clips_rows() {
    let damage = dirty_rows_damage_rect(
        [1usize, 3usize],
        sonicterm_render_model::geometry::PixelRect { x: 8, y: 10, w: 100, h: 50 },
        8.0,
        10.0,
        10,
        6.0,
        12.0,
        80,
        80,
    );

    // Rows 1 and 3 union vertically into [22, 58); horizontally the strip
    // spans the pane's clipped bounds [8, 80) rather than the 60px the cell
    // grid occupies. The extra 20px is padding band, which carries glyph ink
    // from negative left side bearings and so is repainted with the row.
    assert_eq!(
        damage,
        Some(sonicterm_render_model::geometry::PixelRect { x: 8, y: 22, w: 72, h: 36 })
    );
}

#[test]
fn dirty_rows_damage_rect_returns_none_for_no_dirty_rows() {
    let damage = dirty_rows_damage_rect(
        [],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 100, h: 50 },
        0.0,
        0.0,
        10,
        6.0,
        12.0,
        100,
        50,
    );

    assert_eq!(damage, None);
}

#[test]
fn dirty_rows_damage_rect_closes_fractional_cell_seam() {
    // at fractional DPI the cell height is non-integer. A single dirty
    // row's damage strip must extend down to the CEILED top of the next row,
    // otherwise the boundary pixel a full-cell inverse block (zsh's
    // reverse-video PROMPT_EOL_MARK `%`) painted into is never repainted when
    // the cell is later cleared — leaving a 1px underline-like remnant.
    //
    // cell_h = 20.4, origin_y = 0:
    //   row 2 top  = floor(2*20.4)=floor(40.8)=40
    //   row 3 top  = ceil(3*20.4)=ceil(61.2)=62
    //   strip      = [40, 62) → height 22
    // The pre-fix height was ceil(20.4)=21 → [40,61), missing pixel 61, which
    // is the start of where row 3 (= the next cell row) begins on screen.
    let damage = dirty_rows_damage_rect(
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .expect("one dirty row yields damage");
    assert_eq!(damage.y, 40, "row top floored");
    assert_eq!(
        damage.y + damage.h as i32,
        62,
        "strip must reach the ceiled top of the next row (no rounding seam)"
    );

    // Two adjacent dirty rows must tile seamlessly: row N's bottom edge equals
    // row N+1's top edge with no gap and no missing boundary pixel.
    let r2 = dirty_rows_damage_rect(
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .unwrap();
    let r3 = dirty_rows_damage_rect(
        [3usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .unwrap();
    assert!(
        r2.y + r2.h as i32 >= r3.y,
        "row 2 strip must reach row 3 top: {} vs {}",
        r2.y + r2.h as i32,
        r3.y
    );
}

/// Retained damage reserves one native line above and below every changed row.
#[test]
fn terminal_ink_pad_uses_native_font_height() {
    let metrics = sonicterm_engine::CellMetricsPx {
        cell_w: 12.0,
        cell_h: 24.0,
        underline_h: 1.0,
        descender: -5.0,
    };

    assert_eq!(terminal_vertical_ink_pad(31.2, Some(metrics)), 24.0);
    assert_eq!(terminal_vertical_ink_pad(24.0, Some(metrics)), 24.0);
    assert_eq!(terminal_vertical_ink_pad(12.0, Some(metrics)), 24.0);
}

/// Missing font metrics conservatively use the configured row height.
#[test]
fn terminal_ink_pad_falls_back_to_row_height() {
    assert_eq!(terminal_vertical_ink_pad(12.2, None), 13.0);
}

/// Font ink outside a compressed row expands retained GPU damage in both directions.
#[test]
fn dirty_rows_damage_rect_covers_vertical_glyph_overhang() {
    let damage = dirty_rows_damage_rect_with_ink_pad(
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        12.0,
        12.0,
        600,
        800,
    )
    .expect("one dirty row yields padded damage");

    assert_eq!(damage.y, 12, "12 px of preceding-row overhang is repainted");
    assert_eq!(damage.y + damage.h as i32, 48, "following-row overhang is repainted too");
}

/// Ink padding remains clipped to the pane rather than repainting peer panes.
#[test]
fn dirty_rows_damage_rect_clips_vertical_overhang_to_pane() {
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 30, w: 600, h: 24 };
    let damage = dirty_rows_damage_rect_with_ink_pad(
        [0usize],
        pane,
        0.0,
        30.0,
        80,
        10.0,
        12.0,
        20.0,
        600,
        800,
    )
    .expect("padded row intersects its pane");

    assert_eq!(damage, pane);
}

/// Alternate screens already repaint the pane and ignore row-level ink padding.
#[test]
fn pane_damage_rect_alt_ignores_redundant_ink_padding() {
    let pane = sonicterm_render_model::geometry::PixelRect { x: 10, y: 20, w: 200, h: 300 };
    let damage = pane_damage_rect_with_ink_pad(
        true,
        [3usize],
        pane,
        10.0,
        20.0,
        80,
        10.0,
        12.0,
        40.0,
        800,
        600,
    );

    assert_eq!(damage, Some(pane));
}

#[test]
fn dirty_rows_damage_rect_closes_fractional_origin_seam_horizontally() {
    // The horizontal twin of the vertical seam above. A pane whose left edge
    // is not on a whole pixel — a split boundary, or padding at fractional
    // DPI — has its damage rect floored leftward. If the width is derived
    // from the cell count alone it does not get that fraction back, so the
    // strip ends short of the true right edge and the last column is never
    // repainted. A glyph that painted there survives after its cell is
    // cleared, appearing as a stray mark on an otherwise empty row.
    //
    // origin_x = 24.6, cell_w = 10.4, cols = 80:
    //   left        = floor(24.6)               = 24
    //   true right  = 24.6 + 80*10.4 = 856.6
    //   ceiled      = 857
    //   width       = 857 - 24                  = 833
    // Deriving width as ceil(80*10.4) = 832 gives right = 856, one pixel
    // short of the 856.6 the pane actually covers.
    //
    // The pane rect here starts AT the content origin and ends at its right
    // edge, so the strip is not also being widened to a padding band — this
    // test is about the rounding alone.
    let damage = dirty_rows_damage_rect(
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 24, y: 0, w: 833, h: 800 },
        24.6,
        0.0,
        80,
        10.4,
        20.0,
        1200,
        800,
    )
    .expect("one dirty row yields damage");

    assert_eq!(damage.x, 24, "left edge floored");
    assert_eq!(
        damage.x + damage.w as i32,
        857,
        "strip must reach the ceiled true right edge, not floor(origin) + ceil(cols * cell_w)"
    );

    // A whole-pixel origin must be unaffected, or the fix has moved the
    // common case to buy the fractional one.
    let aligned = dirty_rows_damage_rect(
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 24, y: 0, w: 832, h: 800 },
        24.0,
        0.0,
        80,
        10.4,
        20.0,
        1200,
        800,
    )
    .expect("one dirty row yields damage");
    assert_eq!(aligned.x, 24, "aligned left edge unchanged");
    assert_eq!(
        aligned.x + aligned.w as i32,
        856,
        "aligned origin still covers exactly ceil(24 + 80 * 10.4)"
    );
}

// --- pane_damage_rect: alt-screen whole-pane vs normal narrow damage ------

#[test]
fn pane_damage_rect_alt_clean_pane_has_no_damage() {
    // An alt-screen pane with zero dirty rows contributes no damage
    // (nothing changed -> no repaint), the same as a clean normal pane.
    let d = pane_damage_rect(
        true,
        [],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 200, h: 480 },
        0.0,
        0.0,
        80,
        10.0,
        12.0,
        800,
        600,
    );
    assert_eq!(d, None);
}

#[test]
fn pane_damage_rect_alt_dirty_pane_repaints_whole_clipped_rect() {
    // A single dirty row on the alt screen must expand to the ENTIRE pane
    // rectangle (not a narrow row strip): the app may have scrolled content
    // out from under rows it did not re-emit this frame.
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 200, h: 480 };
    let d = pane_damage_rect(true, [5usize], pane, 0.0, 0.0, 80, 10.0, 12.0, 800, 600);
    assert_eq!(d, Some(pane), "one dirty alt row must repaint the full pane");
}

#[test]
fn pane_damage_rect_covers_the_padding_band_around_the_content() {
    // The cell grid starts at the CONTENT origin, inset from the pane by the
    // configured padding, but glyph ink is not confined to it: a negative
    // left side bearing at column 0 paints left of its cell, into the
    // padding band. A damage rect that stops at the content edge never
    // repaints those columns, so such a pixel survives every later frame —
    // including a full alt-screen pane repaint — until something forces a
    // whole-surface redraw.
    //
    // Both paths must therefore span the pane's own bounds, with the cell
    // geometry still measured from the content origin.
    //
    // pane at x=0 w=848, content origin x=24 (padding_left 12 logical at 2x),
    // 80 cols of 10.0 => content spans [24, 824), pane spans [0, 848).
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 848, h: 640 };

    let normal = pane_damage_rect(false, [0usize], pane, 24.0, 16.0, 80, 10.0, 20.0, 1200, 800)
        .expect("a dirty normal pane damages its row");
    assert_eq!(normal.x, 0, "the row strip must reach the pane's left edge, not the content edge");
    assert_eq!(
        normal.x + normal.w as i32,
        848,
        "and its right edge, so ink in either padding band is repainted"
    );

    let alt = pane_damage_rect(true, [0usize], pane, 24.0, 16.0, 80, 10.0, 20.0, 1200, 800)
        .expect("a dirty alt pane damages its rect");
    assert_eq!(alt, pane, "an alt repaint covers the whole pane including padding");

    // Widening must not lose the row's vertical precision: a normal-screen
    // repaint still covers one row, not the whole pane. Otherwise this would
    // trade a stale-pixel bug for repainting everything on every keystroke.
    assert!(
        normal.h < pane.h,
        "the normal path must stay row-limited vertically: got h={} against a {}-tall pane",
        normal.h,
        pane.h
    );
}

#[test]
fn pane_damage_rect_alt_dirty_pane_is_clipped_to_surface() {
    // A dirty alt pane larger than the surface repaints only its on-screen
    // intersection — a complete pane repaint, never a full-window repaint
    // beyond the surface bounds.
    //   pane   = {100,100,800,800}, right/bottom = 900/900
    //   bounds = {0,0,400,300}
    //   clip   = {100,100, 400-100=300, 300-100=200}
    let d = pane_damage_rect(
        true,
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 100, y: 100, w: 800, h: 800 },
        100.0,
        100.0,
        80,
        10.0,
        12.0,
        400,
        300,
    );
    assert_eq!(
        d,
        Some(sonicterm_render_model::geometry::PixelRect { x: 100, y: 100, w: 300, h: 200 })
    );
}

#[test]
fn pane_damage_rect_alt_offscreen_pane_has_no_damage() {
    // A dirty alt pane wholly off the surface intersects nothing -> None.
    let d = pane_damage_rect(
        true,
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 500, y: 500, w: 100, h: 100 },
        500.0,
        500.0,
        80,
        10.0,
        12.0,
        400,
        400,
    );
    assert_eq!(d, None);
}

#[test]
fn pane_damage_rect_alt_sparse_rows_still_repaint_whole_pane() {
    // Scattered dirty rows [2, 37]: the alt pane repaints in full, while the
    // same input on a normal pane stays narrow (union of two thin strips).
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 200, h: 480 };
    let alt = pane_damage_rect(true, [2usize, 37usize], pane, 0.0, 0.0, 80, 10.0, 12.0, 800, 600);
    assert_eq!(alt, Some(pane), "alt sparse rows -> whole pane");

    let normal =
        pane_damage_rect(false, [2usize, 37usize], pane, 0.0, 0.0, 80, 10.0, 12.0, 800, 600)
            .expect("dirty normal pane has damage");
    assert!(
        normal.h < pane.h,
        "normal sparse damage must stay narrow, got {normal:?} vs pane {pane:?}"
    );
}

#[test]
fn pane_damage_rect_normal_matches_narrow_helper() {
    // A normal-screen pane delegates to the narrow dirty-row helper and
    // produces exactly its rect for the same inputs (behavior preserved).
    let pane = sonicterm_render_model::geometry::PixelRect { x: 8, y: 10, w: 100, h: 50 };
    let via_wrapper =
        pane_damage_rect(false, [1usize, 3usize], pane, 8.0, 10.0, 10, 6.0, 12.0, 80, 80);
    let direct = dirty_rows_damage_rect([1usize, 3usize], pane, 8.0, 10.0, 10, 6.0, 12.0, 80, 80);
    assert_eq!(via_wrapper, direct);
    // Width spans the pane's clipped bounds — [8, 80) after the 80px surface
    // clips the 100px pane — rather than the 60px the cell grid occupies.
    // The 20px difference is padding band, which holds glyph ink from
    // negative left side bearings and so has to be repainted with the row.
    assert_eq!(
        via_wrapper,
        Some(sonicterm_render_model::geometry::PixelRect { x: 8, y: 22, w: 72, h: 36 })
    );
}

#[test]
fn pane_damage_rect_normal_empty_input_is_none() {
    // No dirty rows on a normal pane -> no damage.
    let d = pane_damage_rect(
        false,
        [],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 100, h: 50 },
        0.0,
        0.0,
        10,
        6.0,
        12.0,
        100,
        50,
    );
    assert_eq!(d, None);
}

#[test]
fn pane_damage_rect_normal_closes_fractional_cell_seam() {
    // Fractional DPI: a normal pane still routes through the seam-closing
    // narrow logic. Row 2 at cell_h 20.4 spans [40, 62).
    let d = pane_damage_rect(
        false,
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .expect("one dirty row yields damage");
    assert_eq!(d.y, 40, "row top floored");
    assert_eq!(d.y + d.h as i32, 62, "strip reaches the ceiled next-row top");
}

#[test]
fn pane_damage_rect_normal_sparse_rows_clip_offscreen_row() {
    // Scattered rows where the far row is off the surface: only the
    // on-surface row contributes, clipped into the pane.
    //   row 0   -> [floor(0), ceil(20.4)) = [0, 21)
    //   row 100 -> top floor(2040) is past the 800px surface -> clipped away
    let d = pane_damage_rect(
        false,
        [0usize, 100usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .expect("row 0 is on-surface");
    assert_eq!(d, sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 21 });
}

/// Effective scrollbar opacity follows the same visibility floor as quad emission.
#[test]
fn effective_scrollbar_buckets_match_visible_output() {
    use sonicterm_render_model::boundary::cfg::config::ScrollbarMode;
    use sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR;

    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Never, 10, 24, 1.0), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 0, 24, 1.0), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 0, 1.0), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 24, ALPHA_EMIT_FLOOR), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 24, 0.5), 32_768);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 24, 2.0), u16::MAX);
}

/// Pane order cannot perturb the deterministic scrollbar frame identity.
#[test]
fn scrollbar_identity_sorts_panes_by_id() {
    use sonicterm_render_model::boundary::cfg::config::ScrollbarMode;

    let forward =
        pane_scrollbar_identity(ScrollbarMode::Auto, [(1, 4, 24, 0.25), (2, 8, 24, 0.75)]);
    let reversed =
        pane_scrollbar_identity(ScrollbarMode::Auto, [(2, 8, 24, 0.75), (1, 4, 24, 0.25)]);

    assert_eq!(forward, reversed);
}

/// A visible opacity change must make two otherwise identical frame keys unequal.
#[test]
fn scrollbar_bucket_change_invalidates_the_frame_key() {
    let baseline = revision_plan(7, 1).key;
    let mut visible = baseline.clone();
    visible.panes[0].scrollbar_bucket = u16::MAX;

    assert_ne!(baseline, visible);
}

/// Changing effective LCD policy invalidates the frame without requiring atlas identity changes.
#[test]
fn subpixel_aa_change_invalidates_the_frame_key() {
    let grayscale = revision_plan(7, 1).key;
    let mut lcd = grayscale.clone();
    lcd.window.subpixel_aa = SubpixelAaMode::Rgb;

    assert_ne!(grayscale, lcd);
}

/// Degraded rendering treats scrollbar changes as a full-frame signal.
#[test]
fn scrollbar_change_forces_degraded_full_render() {
    assert_eq!(
        decide_render_mode(true, RenderSignals { scrollbar_change: true, ..Default::default() },),
        RenderMode::Full
    );
}

#[test]
fn full_repaint_forced_on_invalidation() {
    let damage = Some(sonicterm_render_model::geometry::PixelRect { x: 1, y: 2, w: 3, h: 4 });
    let mut cases = [
        RenderSignals { first_frame: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { resize: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { dpi_or_scale_change: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { font_or_atlas_rebuild: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { theme_or_config_reload: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { surface_reconfigure: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { occlusion_restore: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { viewport_scroll: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { selection_change: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { tab_switch: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { pane_topology_change: true, dirty_damage: damage, ..Default::default() },
        RenderSignals {
            overlay_active_or_toggled: true,
            dirty_damage: damage,
            ..Default::default()
        },
    ];
    for signals in cases.iter_mut() {
        assert_eq!(decide_render_mode(true, *signals), RenderMode::Full);
    }
    assert_eq!(
        decide_render_mode(true, RenderSignals { dirty_damage: damage, ..Default::default() }),
        RenderMode::Full
    );
}

#[test]
fn non_degrade_always_full() {
    assert_eq!(decide_render_mode(false, RenderSignals::default()), RenderMode::Full);
}

#[test]
fn overlay_active_forces_full() {
    assert_eq!(
        decide_render_mode(
            true,
            RenderSignals {
                overlay_active_or_toggled: true,
                dirty_damage: Some(sonicterm_render_model::geometry::PixelRect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
                ..Default::default()
            },
        ),
        RenderMode::Full
    );
}

/// Focus feedback advances through visible buckets and expires exactly at its bound.
#[test]
fn pane_focus_flash_sample_advances_and_expires() {
    let first = pane_focus_flash_sample(Duration::ZERO).expect("flash starts visible");
    let next = pane_focus_flash_sample(Duration::from_millis(16)).expect("second bucket visible");
    let last =
        pane_focus_flash_sample(Duration::from_millis(359)).expect("last millisecond visible");

    assert_eq!(first.0, 1);
    assert_eq!(next.0, 2);
    assert!(first.1 > next.1 && next.1 > last.1, "flash alpha must fade monotonically");
    assert_eq!(pane_focus_flash_sample(Duration::from_millis(360)), None);
}

#[test]
fn damage_empty_is_noop() {
    assert_eq!(decide_render_mode(true, RenderSignals::default()), RenderMode::Noop);
}

/// A Full frame for an edit to live row 5 of a view scrolled back three rows drops the quad
/// cache's entry for absolute row 105 and damages slot 8, which draws it; it neither damages
/// slot 5 nor drops the entries for absolute row 102 (drawn at slot 5) or 108.
#[test]
fn scrolled_back_edit_invalidates_the_live_rows_absolute_entry_in_both_caches() {
    let plan = scrolled_back_cache_plan(vec![5]);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(
        plan.damage.intersect(cache_plan_slot_rect(&plan, 8)),
        Some(cache_plan_slot_rect(&plan, 8))
    );
    assert_eq!(plan.damage.intersect(cache_plan_slot_rect(&plan, 5)), None);
    assert_eq!(surviving_cached_rows(&plan), [102, 108, 122], "quad cache");
}

/// With one dirty live row drawn and one below the view, a Full frame drops both absolute
/// rows from the quad cache while its damage covers only the drawn row's slot.
#[test]
fn mixed_scrolled_back_edit_invalidates_both_rows_and_damages_one_slot() {
    let plan = scrolled_back_cache_plan(vec![5, 22]);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, cache_plan_slot_rect(&plan, 8));
    assert_eq!(surviving_cached_rows(&plan), [102, 108], "quad cache");
}

/// The cursor is drawn when the view is live and hidden when it is scrolled back, even by
/// three rows of a 24-row view that still displays the cursor's live row 5 at slot 8: the
/// cursor follows the live top, not whether its row is in the viewport.
#[test]
fn terminal_cursor_is_drawn_only_at_the_live_view_top() {
    let (scrollback_len, rows, cursor_live_row) = (100_u64, 24_u16, 5_usize);
    let scrolled_view_top = scrollback_len - 3;
    let displayed_slot = crate::frame_plan::live_row_to_slot(
        scrollback_len,
        scrolled_view_top,
        rows,
        cursor_live_row,
    );
    assert_eq!(displayed_slot, Some(8), "the scrolled-back view still shows the cursor row");
    assert!(terminal_cursor_drawn_at_view(scrollback_len, scrollback_len), "live view");
    assert!(!terminal_cursor_drawn_at_view(scrolled_view_top, scrollback_len), "scrolled back");
}

/// The cursor draw path gates on the active view top through the tested predicate, once, and
/// the frame identity's drawn cursor cell gates through the same predicate, so neither can
/// drift from the condition the renderer applies.
#[test]
fn cursor_draw_path_calls_the_live_view_predicate() {
    let core: String = crate::core::CORE_SOURCE.split_whitespace().collect();
    let gate =
        "letview_top=plan.active_view_top_abs;ifterminal_cursor_drawn_at_view(view_top,live_top){";
    assert_eq!(core.matches(gate).count(), 1, "cursor path must call the predicate");
    let identity = "&&terminal_cursor_drawn_at_view(view_top_abs,live_top);if!drawn{";
    assert_eq!(core.matches(identity).count(), 1, "the drawn cursor cell must call it");
    assert_eq!(
        core.matches("terminal_cursor_drawn_at_view(").count(),
        3,
        "definition, the draw call and the identity call"
    );
    assert_eq!(core.matches("ifview_top==live_top{").count(), 0, "no inline duplicate");
}

/// With clean grids (no row dirt), a renderer whose caches were warmed by the previous frame
/// draws each pointer operation exactly as a renderer with empty caches, at scales 1, 1.25, 1.5
/// and 2: glyph instance bytes (position, atlas region, colour, cursor recolour), underline and
/// tofu records, background, selection and focus chrome all match. Because the glyph key is the
/// row's content, every row whose cells are unchanged replays wherever it is drawn: a selection,
/// a focus change and a moved pane replay all four rows, a two-row viewport move replays the two
/// rows still shown, and only a resized pane, released with its old width, replays none.
#[test]
fn pointer_operations_render_the_same_from_warmed_caches_as_from_fresh_ones() {
    let stack = rec_mono_font_stack();
    let wide = parity_grid(6);
    let narrow = parity_grid(4);
    let live_top = wide.scrollback_len() as u64;
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let rest = ParityFrame {
            view_top_abs: live_top,
            origin_x: 0.0,
            selection: None,
            focused: false,
            scale,
        };
        let focused = ParityFrame { focused: true, ..rest };
        let cases = [
            // name, before frame and grid, after frame and grid, glyph and background replays.
            (
                "selection",
                (rest, &wide),
                (ParityFrame { selection: Some((live_top + 1, live_top + 1)), ..rest }, &wide),
                4,
                3,
            ),
            ("focus gained", (rest, &wide), (focused, &wide), 4, 4),
            ("focus lost", (focused, &wide), (rest, &wide), 4, 4),
            (
                "viewport",
                (rest, &wide),
                (ParityFrame { view_top_abs: live_top - 2, ..rest }, &wide),
                2,
                0,
            ),
            ("moved pane", (rest, &wide), (ParityFrame { origin_x: 120.0, ..rest }, &wide), 4, 0),
            ("resized pane", (rest, &wide), (rest, &narrow), 0, 0),
        ];
        for (name, (before, before_grid), (after, after_grid), glyph_hits, background_hits) in cases
        {
            let mut atlas = GlyphAtlas::new(1024, 1024);
            let mut warmed = ParityCaches::new();
            let (before_output, _, _) =
                render_parity_frame(&stack, &mut atlas, &mut warmed, before_grid, before);
            let (cached, glyph_replays, background_replays) =
                render_parity_frame(&stack, &mut atlas, &mut warmed, after_grid, after);
            // The forced-fresh render shares the atlas, so glyph regions keep their placement.
            let (fresh, fresh_replays, _) = render_parity_frame(
                &stack,
                &mut atlas,
                &mut ParityCaches::new(),
                after_grid,
                after,
            );
            assert!(!fresh.glyphs.is_empty(), "{name} at {scale}: the fixture draws real glyphs");
            assert_eq!(fresh_replays, 0, "{name} at {scale}: empty caches replay nothing");
            assert_eq!(cached, fresh, "{name} at {scale}: warmed caches draw what fresh ones do");
            assert_ne!(cached, before_output, "{name} at {scale}: the operation changes the frame");
            assert_eq!(
                (glyph_replays, background_replays),
                (glyph_hits, background_hits),
                "{name} at {scale}: rows whose content did not change still replay"
            );
        }
    }
}

/// The frame identity's cursor cell follows the draw's own rule: a cursor is drawn only when it
/// is visible, the window is focused, the pane is not read-only and the view is at the live top;
/// it sits at the cursor's viewport slot, and on either half of a wide character covers both.
#[test]
fn drawn_cursor_cell_follows_the_draw_condition_and_wide_span() {
    use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};
    let mut grid = Grid::new(8, 4);
    grid.linefeed();
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    let live_top = grid.scrollback_len() as u64;
    let narrow = drawn_cursor_cell(&grid, 7, live_top, true, true, false);
    assert_eq!(narrow, Some(CursorCell { pane_id: 7, slot: 1, col: 1, span: 1 }));
    for (visible, focused, read_only) in
        [(false, true, false), (true, false, false), (true, true, true)]
    {
        assert_eq!(drawn_cursor_cell(&grid, 7, live_top, visible, focused, read_only), None);
    }
    for _ in 0..6 {
        grid.linefeed();
    }
    let scrolled_live_top = grid.scrollback_len() as u64;
    assert!(scrolled_live_top > 0, "history exists to scroll back into");
    assert_eq!(drawn_cursor_cell(&grid, 7, scrolled_live_top - 1, true, true, false), None);

    let mut wide = Grid::new(8, 4);
    wide.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    assert!(wide.row(0)[0].flags.contains(CellFlags::WIDE), "the lead half is marked wide");
    wide.cursor.col = 1;
    let on_trail = drawn_cursor_cell(&wide, 7, 0, true, true, false);
    assert_eq!(on_trail, Some(CursorCell { pane_id: 7, slot: 0, col: 0, span: 2 }));
    wide.cursor.col = 0;
    let on_lead = drawn_cursor_cell(&wide, 7, 0, true, true, false);
    assert_eq!(on_lead, Some(CursorCell { pane_id: 7, slot: 0, col: 0, span: 2 }));
}

/// Cursor, blink and focus changes reach the planner as damage classes, so their setters keep
/// the retained frame key: the next frame is not a whole-surface first frame. Line endings are
/// normalized before scanning.
#[test]
fn cursor_and_focus_setters_keep_the_frame_key() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    for signature in [
        "    pub fn set_cursor_shape(",
        "    pub fn set_cursor_blink(",
        "    pub fn set_window_focused(",
    ] {
        let body = function_body(&source, signature);
        assert!(!body.contains("last_frame_key = None"), "{signature} clears the frame key");
    }
    // Blink still restarts its phase when the setting changes.
    assert!(function_body(&source, "    pub fn set_cursor_blink(").contains("self.blink_epoch ="));
}

/// Production wiring of the damage classes: the identity records the drawn cursor cell, both
/// cursor recolor sites accumulate one record, the plan's damage is widened by it before the
/// presenter reads damage, the record is kept only by a presented frame and fed back to the next
/// plan, row emission follows the coverage helper, and the scrollbar draws from the geometry the
/// planner damages.
#[test]
fn damage_classes_are_wired_through_the_renderer() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let compact: String = source.split_whitespace().collect();
    assert!(compact.contains("cursor_cell:drawn_cursor_cell("));
    assert!(compact.contains("previous_recolor:self.last_recolor,"));
    assert_eq!(compact.matches("frame_recolor=frame_recolor.merge(record);").count(), 2);
    // `assemble_frame` hands both recolor records to the shared pass-end helper, which widens
    // the damage by them before reading receipts, and the layers are built only after it.
    assert!(compact.contains("current_recolor:frame_recolor,"));
    let helper = method_body(&source, "    pub(super) fn finish_assembly_pass(");
    let widen = helper
        .find("plan.widen_for_recolor(pass.previous_recolor, pass.current_recolor);")
        .expect("the helper widens by the recolors");
    let receipts = helper.find("Ok(presented_receipts(plan, panes))").expect("receipts");
    assert!(widen < receipts, "damage is widened before the receipts are read");
    let call = source.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    let layers = source.find("Ok(Assembled::Layers(Box::new(AssembledLayers {").unwrap();
    assert!(call < layers, "damage is widened before the layers carry it");
    // The record is stored after the presenter reports `Presented` and before the frame finishes.
    let present = function_body(&source, "    pub(super) fn present_layers(");
    let guard = present.find("return Ok(FrameOutcome::without_receipts(outcome));").unwrap();
    let store = present.find("self.last_recolor = recolor;").expect("the record is stored");
    let finish = present.find("self.finish_successful_frame(").unwrap();
    assert!(guard < store && store < finish);
    assert_eq!(source.matches("self.last_recolor = ").count(), 1, "one writer");
    assert!(function_body(&source, "pub fn emit_pane_scrollbar(")
        .contains("crate::frame_plan::pane_scrollbar_geometry("));
}
