use sonicterm_text::glyph_atlas::GlyphAtlas;

use super::*;

/// Notification chrome reuses regular weighted raster tiles and preserves their LCD coverage flags.
#[test]
fn notification_reuses_regular_weighted_glyphs() {
    use sonicterm_text::glyph_atlas::Rasterizer;
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().unwrap();
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    for weight in [0.75, 1.0, 1.5] {
        let stack = FontStack::try_new_with_font_dirs_for_test(
            &[("Rec Mono St.Helens", false)],
            vec![assets.clone()],
            14.0,
            96,
            weight,
        )
        .unwrap();
        let mut atlas = GlyphAtlas::new(512, 512);
        let mut raster = stack.clone();
        for ch in "policy".chars() {
            let shaped = stack.shape_text(&ch.to_string()).unwrap();
            let glyph = shaped.iter().find(|g| g.glyph_pos != 0).unwrap();
            let key = GlyphKey::shaped(ch, glyph.font_idx as u8, glyph.glyph_pos, false, false);
            let tile = raster.rasterize(key).unwrap();
            let regular = atlas.get_or_insert(key, &mut raster).unwrap();
            let before = atlas.len();
            let chrome = layout(
                &stack,
                &mut raster,
                &mut atlas,
                &ch.to_string(),
                ChromeColor::WHITE,
                ChromeAttrs::default(),
                14.0,
                14.0,
                (20.0, 30.0),
                (800.0, 100.0),
                None,
            );
            let instance = chrome.glyphs.first().unwrap();
            assert_eq!(atlas.len(), before, "notification must reuse the regular glyph tile");
            assert_eq!(instance.uv, regular.uv);
            assert_eq!(instance.flags, crate::core::glyph_flags(tile.is_color, tile.is_subpixel));
        }
    }
}

/// Chrome glyph placement combines raster and HarfBuzz offsets before snapping.
#[test]
fn positioned_origin_applies_both_offset_sources() {
    assert_eq!(positioned_glyph_origin(10.25, -1.0, 2.5, 20.5, -8.0, -3.25), (12.0, 9.0));
}

#[test]
fn native_raster_roles_use_distinct_tiles_without_projection_scaling() {
    // Contract: each chrome role has a distinct atlas tile drawn at its native raster size.
    let _font_lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().expect("font fixture lock");
    let mut atlas = GlyphAtlas::new(512, 512);
    let screen = (800.0, 100.0);
    let cases = [
        (12.0, GlyphRasterVariant::PaletteFooter),
        (13.0, GlyphRasterVariant::Normal),
        (14.0, GlyphRasterVariant::TabTitle),
    ];
    let mut tile_sizes = Vec::new();

    for (size, variant) in cases {
        let stack = crate::lib_tests::tracked_font_stack(size);
        let shaped = stack.shape_text("M").expect("tracked font shapes M");
        let glyph = shaped.iter().find(|glyph| glyph.glyph_pos != 0).expect("tracked font has M");
        let key = GlyphKey::shaped(
            'M',
            u8::try_from(glyph.font_idx).expect("fixture font index fits"),
            glyph.glyph_pos,
            false,
            false,
        )
        .with_raster_variant(variant);
        let mut rasterizer = stack.clone();
        let layout = layout_with_raster_variant(
            &stack,
            &mut rasterizer,
            &mut atlas,
            "M",
            ChromeColor::WHITE,
            ChromeAttrs::default(),
            size as f32,
            size as f32,
            (0.0, 30.0),
            screen,
            None,
            variant,
        );
        let info = atlas.get(key).expect("role-specific tile was inserted");
        let instance = layout.glyphs.first().expect("M emits one visible glyph");
        let draw_size =
            [(instance.rect[2] * screen.0 * 0.5).abs(), (instance.rect[3] * screen.1 * 0.5).abs()];

        assert_eq!(draw_size[0].to_bits(), (info.px_size[0] as f32).to_bits());
        assert_eq!(draw_size[1].to_bits(), (info.px_size[1] as f32).to_bits());
        tile_sizes.push(info.px_size);
    }

    assert_eq!(atlas.len(), 3);
    assert_ne!(tile_sizes[0], tile_sizes[1]);
    assert_ne!(tile_sizes[1], tile_sizes[2]);
}

/// Hold the shared font fixture even after a failed sibling test poisoned it, so one failure
/// cannot fail every later test that shapes text.
fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Uniform 8 × 8 tiles make the atlas capacity in a test exact.
struct SquareTiles;

impl sonicterm_text::glyph_atlas::Rasterizer for SquareTiles {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 8,
            height: 8,
            offset_x: 0,
            offset_y: -8,
            advance: 8.0,
            coverage: vec![255; 64],
            is_color: false,
            is_subpixel: false,
        })
    }
}

/// A glyph the atlas cannot place is dropped for the frame but still advances the pen, so a
/// drawn run keeps the width its shaped advances measure and a tab never overlaps its neighbour.
#[test]
fn an_atlas_miss_still_advances_the_pen() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut atlas = GlyphAtlas::new(8, 8);
    atlas.set_eviction_enabled(false);

    let run = layout(
        &stack,
        &mut SquareTiles,
        &mut atlas,
        "abc",
        ChromeColor::WHITE,
        ChromeAttrs::default(),
        15.0,
        15.0,
        (0.0, 20.0),
        (400.0, 100.0),
        None,
    );
    let shaped_px: f32 = shaped_advances(&stack, "abc", ChromeAttrs::default(), 15.0, 15.0)
        .expect("the tracked font shapes ASCII")
        .iter()
        .map(|(_, advance)| advance)
        .sum();

    assert!(run.glyphs.len() < 3, "the one-tile atlas must refuse a glyph");
    assert!(
        (run.width_px - shaped_px).abs() < 0.01,
        "drawn {} vs shaped {shaped_px}",
        run.width_px
    );
}

/// Shaped advances follow the pen rules drawing uses, so they sum to the drawn width for ASCII,
/// CJK, emoji and blank runs, and a tab is measured at the width it is drawn.
#[test]
fn shaped_advances_sum_to_the_drawn_width() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    for text in ["#1 zsh", "任务完成", "ab\u{1f469}\u{200d}\u{1f4bb}cd", "a  b", " "] {
        let mut raster = stack.clone();
        let mut atlas = GlyphAtlas::new(2048, 2048);
        let run = layout_with_raster_variant(
            &stack,
            &mut raster,
            &mut atlas,
            text,
            ChromeColor::WHITE,
            ChromeAttrs::default(),
            15.0,
            15.0,
            (0.0, 20.0),
            (4096.0, 256.0),
            None,
            GlyphRasterVariant::TabTitle,
        );
        let advances = shaped_advances(&stack, text, ChromeAttrs::default(), 15.0, 15.0)
            .expect("the tracked font shapes every sample");
        let shaped_px: f32 = advances.iter().map(|(_, advance)| advance).sum();
        assert!(
            (run.width_px - shaped_px).abs() < 0.01,
            "{text:?}: drawn {} vs shaped {shaped_px}",
            run.width_px
        );
        assert!(advances.windows(2).all(|pair| pair[0].0 <= pair[1].0), "{text:?} clusters");
    }
    assert_eq!(shaped_advances(&stack, "", ChromeAttrs::default(), 15.0, 15.0), Some(Vec::new()));
}

/// Lay out one prepared run with uniform tiles and build field boundaries from the same run.
fn lay_out_prepared(
    run: &ChromeShapedRun<'_>,
    origin: (f32, f32),
    screen: (f32, f32),
) -> (ChromeTextLayout, crate::field_geometry::FieldBoundaries) {
    let mut atlas = GlyphAtlas::new(512, 512);
    let layout = layout_prepared(
        run,
        &mut SquareTiles,
        &mut atlas,
        ChromeColor::WHITE,
        origin,
        screen,
        None,
        GlyphRasterVariant::Normal,
    );
    (layout, crate::field_geometry::FieldBoundaries::from_run(run))
}

/// `(cluster byte, left px)` of every emitted glyph that opens a cluster, asserting each sits
/// exactly at its field boundary's pen x (`SquareTiles` has no bearing, so only the shaper's
/// x offset and the origin snap separate them).
fn cluster_lefts_on_boundaries(
    run: &ChromeShapedRun<'_>,
    layout: &ChromeTextLayout,
    boundaries: &crate::field_geometry::FieldBoundaries,
    origin: (f32, f32),
    screen: (f32, f32),
) -> Vec<(usize, f32)> {
    let mut emitted = layout.glyphs.iter();
    let mut lefts = Vec::new();
    let mut previous_cluster = None;
    for glyph in &run.glyphs {
        // Blank notdef clusters advance the pen without a tile; every other glyph emits one.
        let blank =
            glyph.glyph_pos == 0 && (glyph.lead_ch == '\0' || glyph.lead_ch.is_whitespace());
        let opens_cluster = previous_cluster != Some(glyph.cluster);
        previous_cluster = Some(glyph.cluster);
        if blank {
            continue;
        }
        let instance = emitted.next().expect("one emitted glyph per drawable shaped glyph");
        if !opens_cluster {
            continue;
        }
        let left_px = (instance.rect[0] + 1.0) * 0.5 * screen.0;
        let expected_px =
            (origin.0 + boundaries.caret_x(glyph.cluster) + glyph.x_offset_px).round();
        assert!(
            (left_px - expected_px).abs() < 0.01,
            "{:?} cluster {}: emitted at {left_px}, boundary pen at {expected_px}",
            run.text,
            glyph.cluster
        );
        lefts.push((glyph.cluster, left_px));
    }
    assert!(emitted.next().is_none(), "{:?}: no glyph emitted beyond the shaped run", run.text);
    lefts
}

/// Field geometry and painted glyphs consume one shaped run: each emitted glyph that opens a
/// cluster lands on that cluster's boundary, the drawn width equals the boundary width, and
/// widening the literal `▏` cluster in the shared run moves later glyphs and boundaries by the
/// same exact amount while earlier ones stay put. A second, independent shape call could not
/// observe the perturbation, so this pins that both consumers read the same result.
#[test]
fn prepared_run_drives_emitted_glyphs_and_field_boundaries() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let screen = (4096.0, 256.0);
    let origin = (40.0, 20.0);
    let bump_px = 7.0;
    for text in ["a▏b", "x▏中\u{1f642} y"] {
        let mut run = ChromeShapedRun::shape(&stack, text, ChromeAttrs::default(), 15.0, 15.0)
            .expect("the tracked font shapes every sample");
        let bar = text.find('▏').expect("sample holds the literal bar");

        let (layout, boundaries) = lay_out_prepared(&run, origin, screen);
        assert!((layout.width_px - boundaries.total_width()).abs() < 0.01, "{text:?} width");
        let before = cluster_lefts_on_boundaries(&run, &layout, &boundaries, origin, screen);
        assert!(before.iter().any(|&(cluster, _)| cluster == bar), "{text:?}: the bar is drawn");

        let bar_glyph = run.glyphs.iter_mut().find(|glyph| glyph.cluster == bar).expect("bar");
        bar_glyph.x_advance_px += bump_px;
        let (bumped, bumped_boundaries) = lay_out_prepared(&run, origin, screen);
        assert!(
            (bumped_boundaries.total_width() - boundaries.total_width() - bump_px).abs() < 0.01,
            "{text:?}: boundaries see the widened bar"
        );
        assert!((bumped.width_px - layout.width_px - bump_px).abs() < 0.01, "{text:?} drawn");
        let after = cluster_lefts_on_boundaries(&run, &bumped, &bumped_boundaries, origin, screen);
        assert_eq!(before.len(), after.len(), "{text:?}: same drawn clusters");
        for (&(cluster, old_px), &(_, new_px)) in before.iter().zip(&after) {
            // An integral bump survives origin snapping exactly.
            let shift_px = if cluster > bar { bump_px } else { 0.0 };
            assert!((new_px - old_px - shift_px).abs() < 0.01, "{text:?} cluster {cluster}");
        }
    }
}
