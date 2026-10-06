use sonicterm_text::glyph_atlas::GlyphAtlas;

use super::*;

/// Chrome text drawn inside a render attempt is counted there: the first layout shapes and
/// rasterizes its glyphs, the same layout again shapes but finds every tile in the atlas. Tab
/// titles, the palette, search, preedit and notifications all lay out through this function.
#[test]
fn chrome_layout_counts_its_shaping_and_rasterizing_in_the_open_attempt_once() {
    use crate::frame_stats::{test_clock, FrameStatsSink, RenderScope};
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().unwrap();
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let stack = FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![assets],
        14.0,
        96,
        1.0,
    )
    .unwrap();
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(512, 512);
    let sink = FrameStatsSink::default();
    // Every timer reads the clock twice, one step apart, so each timed call adds exactly 1 ns.
    test_clock::install(0, 1);
    let mut lay_out = |owed: bool| {
        let mut owed = owed;
        let scope = RenderScope::enter(Some(&sink), &mut owed);
        layout(
            &stack,
            &mut raster,
            &mut atlas,
            "tab",
            ChromeColor::WHITE,
            ChromeAttrs::default(),
            14.0,
            14.0,
            (20.0, 30.0),
            (800.0, 100.0),
            None,
        );
        // The attempt's notes reach the sink only when its scope closes.
        drop(scope);
        sink.snapshot()
    };
    let first = lay_out(true);
    let second = lay_out(false);
    test_clock::remove();
    assert!(first.raster_calls >= 3 && first.raster_tiles >= 3, "{first:?}");
    assert!(first.shape_requests >= 1);
    assert_eq!((first.shape_ns, first.raster_ns), (first.shape_requests, first.raster_calls));
    let applied = first.apply_attempts;
    assert_eq!((applied.attempts, applied.raster_calls), (1, first.raster_calls));
    assert_eq!(applied.shape_requests, first.shape_requests);
    assert_eq!(second.raster_calls, first.raster_calls, "every tile is an atlas hit");
    assert!(second.shape_requests > first.shape_requests, "the text is shaped again");
    assert_eq!(second.attempts.shape_requests, second.shape_requests);
    assert_eq!(second.apply_attempts, applied, "only the first attempt carried the apply");
}

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

/// Lay out `text` with the tracked 15 px font, the font itself as rasterizer, at origin
/// (10, 30) on a 400 × 100 surface, with an optional clip.
fn lay_out_with_tracked_font(text: &str, clip: Option<ChromeClip>) -> (ChromeTextLayout, f32) {
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(256, 256);
    let run = layout(
        &stack,
        &mut raster,
        &mut atlas,
        text,
        ChromeColor::WHITE,
        ChromeAttrs::default(),
        15.0,
        15.0,
        (10.0, 30.0),
        (400.0, 100.0),
        clip,
    );
    let shaped_px: f32 = shaped_advances(&stack, text, ChromeAttrs::default(), 15.0, 15.0)
        .expect("the tracked font shapes the run")
        .iter()
        .map(|(_, advance)| advance)
        .sum();
    (run, shaped_px)
}

/// Convert an NDC quad rect from `px_to_ndc` back to `[x, y, w, h]` raster px on 400 × 100.
fn quad_px(rect: [f32; 4]) -> [f32; 4] {
    let (sw, sh) = (400.0, 100.0);
    let width = rect[2] * 0.5 * sw;
    let height = rect[3] * 0.5 * sh;
    [(rect[0] + 1.0) * 0.5 * sw, (1.0 - rect[1]) * 0.5 * sh - height, width, height]
}

/// A real font's space is an empty tile: it advances the pen and draws neither a glyph nor
/// a tofu box, so chrome text never shows boxes between words.
#[test]
fn a_real_space_advances_the_pen_without_a_box() {
    let _lock = font_fixture_lock();
    let (run, shaped_px) = lay_out_with_tracked_font("a b", None);

    assert_eq!(run.glyphs.len(), 2, "only a and b draw tiles");
    assert!(run.missing_boxes.is_empty(), "a space is empty, not missing");
    assert!(
        (run.width_px - shaped_px).abs() < 0.01,
        "drawn {} vs shaped {shaped_px}",
        run.width_px
    );
}

/// A character no face resolves draws a four-sided outline one advance wide and one ascent
/// tall above the baseline, and the pen still moves by its advance.
#[test]
fn an_unresolved_character_draws_a_tofu_box_and_advances() {
    let _lock = font_fixture_lock();
    // A plane-15 private-use character: no bundled or system font maps it.
    let (run, shaped_px) = lay_out_with_tracked_font("a\u{F0000}", None);

    assert_eq!(run.glyphs.len(), 1, "only a draws a tile");
    assert_eq!(run.missing_boxes.len(), 4, "one outline is four edge quads");
    let edges: Vec<[f32; 4]> = run.missing_boxes.iter().map(|quad| quad_px(quad.rect)).collect();
    let top_edge = edges[0];
    let left_edge = edges[2];
    let advance_a = shaped_px - top_edge[2];
    assert!((top_edge[0] - (10.0 + advance_a)).abs() < 0.01, "box starts at the pen: {top_edge:?}");
    assert!(top_edge[2] >= 1.0, "box is one advance wide: {top_edge:?}");
    let ascent = 15.0 * MISSING_BOX_ASCENT_RATIO;
    assert!((left_edge[3] - ascent).abs() < 0.01, "box is one ascent tall: {left_edge:?}");
    assert!(
        (left_edge[1] - (30.0 - ascent)).abs() < 0.01,
        "box stands on the baseline: {left_edge:?}"
    );
    assert!(run.missing_boxes.iter().all(|quad| quad.color[3] > 0.0), "the outline is visible");
    assert!(
        (run.width_px - shaped_px).abs() < 0.01,
        "drawn {} vs shaped {shaped_px}",
        run.width_px
    );
}

/// A tofu box outside the caller's clip is dropped like a glyph tile, so a modal's tofu
/// cannot paint across the terminal behind it.
#[test]
fn a_tofu_box_outside_the_clip_is_not_drawn() {
    let _lock = font_fixture_lock();
    let clip = ChromeClip { x: 200.0, y: 0.0, w: 100.0, h: 100.0 };
    let (run, _) = lay_out_with_tracked_font("\u{F0000}", Some(clip));

    assert!(run.missing_boxes.is_empty(), "the box lies left of the clip");
}

/// A tofu box straddling the clip's left edge keeps only its part inside the clip, so a
/// horizontally scrolled field never paints an outline beyond its edge.
#[test]
fn a_tofu_box_straddling_the_clip_edge_is_cut_to_it() {
    let _lock = font_fixture_lock();
    let (unclipped, _) = lay_out_with_tracked_font("\u{F0000}", None);
    let box_left = quad_px(unclipped.missing_boxes[0].rect)[0];
    let clip_left = (box_left + 3.0).round();
    let clip = ChromeClip { x: clip_left, y: 0.0, w: 300.0, h: 100.0 };
    let (run, _) = lay_out_with_tracked_font("\u{F0000}", Some(clip));

    assert_eq!(
        run.missing_boxes.len(),
        3,
        "the left edge lies outside; top, bottom and right stay"
    );
    for quad in &run.missing_boxes {
        let [left, _, width, _] = quad_px(quad.rect);
        assert!(left >= clip_left - 0.01, "edge at {left} starts left of the clip at {clip_left}");
        assert!(width > 0.0);
    }
}

/// Chrome tofu is reported per frame on the same terms as the terminal rows' missing list: a
/// layout inside an open [`MissingChromeScope`] lists each character it drew as tofu, a resolved
/// glyph or a space lists nothing, a layout with no scope open records nowhere, and a nested scope
/// keeps its own list and restores the enclosing one when it closes.
#[test]
fn a_missing_chrome_scope_lists_the_tofu_its_layouts_drew() {
    let _lock = font_fixture_lock();
    // No scope is open: the layout still draws its box, and nothing is recorded.
    let (outside, _) = lay_out_with_tracked_font("a\u{F0000}", None);
    assert_eq!(outside.missing_boxes.len(), 4);

    // U+F0000 is the one character no face maps on any host (U+F0001 is covered on some), so
    // the scopes are told apart by how many times each lists it.
    let outer = MissingChromeScope::enter();
    let _resolved = lay_out_with_tracked_font("a b", None);
    let _tofu = lay_out_with_tracked_font("x\u{F0000}", None);
    let inner = MissingChromeScope::enter();
    let _inner_tofu = lay_out_with_tracked_font("\u{F0000}", None);
    assert_eq!(inner.finish(), vec!['\u{F0000}'], "the nested scope holds only its own tofu");
    let _after_inner = lay_out_with_tracked_font("y\u{F0000}", None);
    assert_eq!(
        outer.finish(),
        vec!['\u{F0000}', '\u{F0000}'],
        "the enclosing list is restored without the nested scope's entry"
    );
}

/// A chrome run that cannot be shaped draws nothing, so every visible character of it is missing,
/// and blanks are not: the same rule the terminal rows apply to cells drawn without a tile.
#[test]
fn an_unshaped_chrome_run_lists_its_visible_characters() {
    let scope = MissingChromeScope::enter();
    note_unshaped_chrome("a b\t·");
    assert_eq!(scope.finish(), vec!['a', 'b', '·']);
}

/// Opens once and lets every waiter through; a waiter is bounded so a failed test cannot hang.
#[derive(Default)]
struct Gate {
    open: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let guard = self.open.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(guard, std::time::Duration::from_secs(30), |open| !*open)
            .unwrap();
    }
}

/// Answers fallback requests as the fixture's Rec Mono locator does, only once its gate opens, so
/// the fallback face arrives after the first frame drew without it.
struct LateLocator(std::sync::Arc<Gate>);

impl sonicterm_font::locator::FontLocator for LateLocator {
    fn load_fonts(
        &self,
        requested: &[config::FontAttributes],
        loaded: &mut std::collections::HashSet<config::FontAttributes>,
        pixel_size: u16,
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        crate::lib_tests::RecMonoLocator.load_fonts(requested, loaded, pixel_size)
    }

    fn locate_fallback_for_codepoints(
        &self,
        codepoints: &[char],
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        self.0.wait();
        crate::lib_tests::RecMonoLocator.locate_fallback_for_codepoints(codepoints)
    }
}

/// The footer's fallback face arrives late. The first frame draws the footer's é as tofu while no
/// terminal row draws any, so a completion check on the terminal rows alone passes there; the
/// chrome readout lists é and keeps the check waiting. Once the face is published and its
/// generation applied (missing sentinels forgotten), the footer draws é and the readout is empty.
#[test]
fn a_late_footer_fallback_face_keeps_the_chrome_readout_waiting() {
    let _lock = font_fixture_lock();
    let gate = std::sync::Arc::new(Gate::default());
    let fixture = crate::lib_tests::fallback_stack_with_locator(
        "late-footer",
        std::sync::Arc::new(LateLocator(std::sync::Arc::clone(&gate))),
    );
    // Opens the gate however the test ends, so the fallback worker never waits out its bound.
    struct OpenOnDrop(std::sync::Arc<Gate>);
    impl Drop for OpenOnDrop {
        // Lifecycle: dropping `OpenOnDrop` opens the gate the fallback worker may be waiting on.
        fn drop(&mut self) {
            self.0.open();
        }
    }
    let _open_on_drop = OpenOnDrop(std::sync::Arc::clone(&gate));
    let body_size = 14.0;
    let stacks = crate::core::renderer_font_views(Some(fixture.stack.clone()), body_size);
    let footer_stack = stacks.palette_footer.expect("a footer stack");
    let footer_size = crate::core::palette_footer_font_size(body_size);
    let mut atlas = GlyphAtlas::new(256, 256);
    let draw_footer = |atlas: &mut GlyphAtlas| {
        let scope = MissingChromeScope::enter();
        let mut rasterizer = footer_stack.clone();
        let footer = layout_with_raster_variant(
            &footer_stack,
            &mut rasterizer,
            atlas,
            "é run",
            ChromeColor::WHITE,
            ChromeAttrs::default(),
            footer_size,
            footer_size,
            (10.0, 30.0),
            (400.0, 100.0),
            None,
            GlyphRasterVariant::PaletteFooter,
        );
        (footer, scope.finish())
    };
    let notice = footer_stack.fallback_notice();
    let generation_before = notice.generation();

    // The frame drew no terminal row, so the terminal readout of the old check is empty.
    let terminal_missing: Vec<char> = Vec::new();
    let (first, first_chrome) = draw_footer(&mut atlas);
    assert!(terminal_missing.is_empty(), "the terminal-only check passes on this frame");
    assert!(!first.missing_boxes.is_empty(), "the footer drew é as tofu");
    assert_eq!(first_chrome, vec!['é'], "the chrome readout keeps the check waiting");

    gate.open();
    let started = std::time::Instant::now();
    while notice.generation() == generation_before {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the late fallback face was never published"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Applying the generation forgets the missing sentinels, as `prepare_frame_fonts` does.
    atlas.forget_missing();
    let (second, second_chrome) = draw_footer(&mut atlas);
    assert!(second.missing_boxes.is_empty(), "the footer draws é from the late face");
    assert!(second_chrome.is_empty(), "{second_chrome:?}");
}

/// Lay out `view` with uniform tiles and build field boundaries from the same view.
fn lay_out_view(
    view: ChromeRunView<'_>,
    origin: (f32, f32),
    screen: (f32, f32),
) -> (ChromeTextLayout, crate::field_geometry::FieldBoundaries) {
    let mut atlas = GlyphAtlas::new(512, 512);
    let layout = layout_view(
        view,
        &mut SquareTiles,
        &mut atlas,
        ChromeColor::WHITE,
        origin,
        screen,
        None,
        GlyphRasterVariant::Normal,
    );
    (layout, crate::field_geometry::FieldBoundaries::from_view(view))
}

#[test]
fn cached_view_drives_glyphs_and_field_boundaries() {
    // A prepared run kept by a cache feeds glyph emission and field geometry through one
    // borrowed view: both equal what a fresh run gives the existing entry points, and a change to
    // the prepared run's glyphs is seen by both consumers, so neither shapes the text again.
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let screen = (4096.0, 256.0);
    let origin = (40.0, 20.0);
    let bump_px = 7.0;
    for text in ["a▏b", "x▏中\u{1f642} y", "", "=> != ->"] {
        let fresh = ChromeShapedRun::shape(&stack, text, ChromeAttrs::default(), 15.0, 15.0)
            .expect("the tracked font shapes every sample");
        let (fresh_layout, fresh_boundaries) = lay_out_prepared(&fresh, origin, screen);
        let mut prepared = PreparedChromeRun::from_run(
            ChromeShapedRun::shape(&stack, text, ChromeAttrs::default(), 15.0, 15.0).unwrap(),
        );
        assert_eq!(prepared.view().text(), text, "the view carries the run's own text");
        let (view_layout, view_boundaries) = lay_out_view(prepared.view(), origin, screen);
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&view_layout.glyphs),
            bytemuck::cast_slice::<_, u8>(&fresh_layout.glyphs),
            "{text:?}: the same glyphs"
        );
        assert_eq!(view_layout.width_px, fresh_layout.width_px, "{text:?}: the same width");
        assert_eq!(view_boundaries, fresh_boundaries, "{text:?}: the same boundaries");
        let advances: Vec<_> = prepared.view().advances().collect();
        assert_eq!(advances, fresh.advances().collect::<Vec<_>>(), "{text:?}: same advances");

        let Some(bar) = text.find('▏') else {
            // When: the sample has no bar, the perturbation half does not apply.
            continue;
        };
        let bar_glyph =
            prepared.glyphs.iter_mut().find(|glyph| glyph.cluster == bar).expect("bar glyph");
        bar_glyph.x_advance_px += bump_px;
        let (bumped, bumped_boundaries) = lay_out_view(prepared.view(), origin, screen);
        assert!(
            (bumped_boundaries.total_width() - view_boundaries.total_width() - bump_px).abs()
                < 0.01,
            "{text:?}: the boundaries see the widened bar"
        );
        assert!((bumped.width_px - view_layout.width_px - bump_px).abs() < 0.01, "{text:?} drawn");
    }
}

#[test]
fn raw_width_is_the_measured_frame_width_bit_for_bit() {
    // A chrome measure served from a prepared run must equal `measure_text_width_for_frame`
    // exactly: both sum the shaper's unscaled advances in order, with no blank-cluster estimate.
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    for text in ["\u{f002}", "search: foo 1/3", "検索 テキスト", "=> != ->", "a b  c"] {
        let measured = stack.measure_text_width_for_frame(text).unwrap();
        let run = PreparedChromeRun::from_run(
            ChromeShapedRun::shape(&stack, text, ChromeAttrs::default(), 15.0, 15.0).unwrap(),
        );
        assert_eq!(run.view().raw_width_px().to_bits(), measured.to_bits(), "{text:?}");
    }
}

/// A chrome glyph whose tile is larger than the atlas can place is cached as a zero-area sentinel;
/// it draws nothing, so it is noted as missing chrome rather than skipped as an empty glyph, and
/// the pen still advances.
#[test]
fn an_oversize_chrome_glyph_is_noted_missing_and_advances() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut raster = stack.clone();
    // A fixed 4x4 atlas cannot place any 15 px letter.
    let mut atlas = GlyphAtlas::new(4, 4);
    let scope = MissingChromeScope::enter();
    let run = layout(
        &stack,
        &mut raster,
        &mut atlas,
        "a",
        ChromeColor::WHITE,
        ChromeAttrs::default(),
        15.0,
        15.0,
        (10.0, 30.0),
        (400.0, 100.0),
        None,
    );
    assert_eq!(scope.finish(), vec!['a'], "the oversize glyph is missing chrome");
    assert!(run.glyphs.is_empty(), "it draws no tile");
    assert!(run.width_px > 0.0, "the pen still advances");
}
