//! Renderer timer, attempt and growth accounting contracts.

use super::super::frame_stats_tests::*;
use super::*;

#[test]
fn no_frame_code_calls_a_shaping_entry_point_that_may_wait() {
    // The renderer shapes and measures only through the frame entry points, which never wait for
    // fallback discovery; the blocking ones stay for explicit callers and tests.
    // The working-set helper is the one explicit caller: it measures outside any frame and must
    // wait for fallback faces. It must exist, so a rename cannot silently widen the exemption.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let is_measurement = |file: &str| Path::new(file).ends_with("glyph_working_set.rs");
    assert!(sources.iter().any(|(file, _)| is_measurement(file)), "the helper's file exists");
    sources.retain(|(file, _)| !is_measurement(file));
    assert_eq!(blocking_shape_calls(&sources), Vec::<String>::new());
}

#[test]
fn the_blocking_call_scan_ignores_comments_strings_and_line_endings() {
    // Mentions in line and block comments, raw strings and escaped char literals never count, the
    // frame entry points never count, and a CRLF checkout finds exactly what an LF one finds.
    let fixture = r##"
// stack.shape_text("x")
/* font.blocking_shape(a, b) */
const RAW: &str = r#"stack.measure_text_width("y")"#;
const ESCAPED: char = '\'';
fn frame(stack: &FontStack) {
    let quote = "shape_text_with_style(";
    stack.shape_text_for_frame("a", false, false);
    stack.measure_text_width_for_frame("b");
    stack.shape_text_with_style("c", true, false);
}
"##;
    let lf = vec![("fixture.rs".to_owned(), fixture.to_owned())];
    let crlf = vec![("fixture.rs".to_owned(), fixture.replace('\n', "\r\n"))];
    assert_eq!(blocking_shape_calls(&lf), vec!["fixture.rs:10 shape_text_with_style".to_owned()]);
    assert_eq!(blocking_shape_calls(&crlf), blocking_shape_calls(&lf));
}

/// Growth counts and growth-to-present times are recorded only inside a counting scope; a time at a
/// millisecond bound lands in that bucket, a time past the last bound in the overflow, and the sum
/// is exact in microseconds. An abandoned episode reaches the sink only through a finalization
/// note, which lands outside any scope.
#[test]
fn growth_counters_and_growth_to_present_buckets() {
    let sink = FrameStatsSink::default();
    note_glyph_atlas_growths(5);
    record_growth_to_present_us(1_000);
    assert_eq!(sink.snapshot(), FrameStats::ZERO, "no scope, nothing recorded");
    {
        let _counting = CollectGuard::enter(Some(&sink));
        note_glyph_atlas_growths(2);
        for elapsed_us in [4_000, 4_001, 100_000, 100_001] {
            record_growth_to_present_us(elapsed_us);
        }
    }
    sink.note_teardown_growths(1, 1);
    let stats = sink.snapshot();
    assert_eq!((stats.glyph_atlas_growths, stats.atlas_growth_abandoned), (3, 1));
    let mut expected = [0; GROWTH_TO_PRESENT_BUCKETS];
    expected[0] = 1; // 4 ms is at the first bound
    expected[1] = 1; // just past 4 ms
    expected[8] = 1; // 100 ms is at the last bound
    expected[9] = 1; // overflow
    assert_eq!(stats.atlas_growth_to_present_buckets, expected);
    assert_eq!(stats.atlas_growth_to_present_sum_us, 4_000 + 4_001 + 100_000 + 100_001);
}

/// Finalizing growth episodes is idempotent: the first call adds the uncounted growths and one
/// abandoned episode to the sink outside any scope, and a second call, as `Drop` makes after the
/// App's own finalization, adds nothing.
#[test]
fn finalizing_growth_episodes_twice_counts_them_once() {
    let sink = FrameStatsSink::default();
    let mut episodes = GrowthEpisodes::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        episodes.count(1, Instant::now());
    }
    // One growth counted at a frame check, then one more outside any frame before teardown.
    episodes.finalize(2, Some(&sink));
    let first = sink.snapshot();
    assert_eq!((first.glyph_atlas_growths, first.atlas_growth_abandoned), (2, 1));
    episodes.finalize(2, Some(&sink));
    assert_eq!(sink.snapshot(), first, "a second finalization adds nothing");
}

#[test]
fn a_timed_request_adds_exactly_its_clock_pair_and_the_gate_off_reads_no_clock() {
    // The test clock steps 7 ns per reading, so one request reads it twice and adds exactly 7 ns.
    // With no counting scope each closure still runs once and returns its value, and no clock,
    // counter, attempt or preparation timer reads the clock.
    test_clock::install(100, 7);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        assert_eq!(shape_request(|| 42), 42);
    }
    assert_eq!(test_clock::reads(), 2);
    assert_eq!((sink.snapshot().shape_ns, sink.snapshot().shape_requests), (7, 1));

    let mut runs = 0;
    assert_eq!(
        shape_request(|| {
            runs += 1;
            5
        }),
        5
    );
    let mut scripted = Scripted::new(vec![Some(coverage_tile(2, 2))]);
    assert!(CountingRasterizer::new(&mut scripted).rasterize(glyph('a')).is_some());
    let _attempt = AttemptScope::enter(true);
    assert_eq!(prepare_clock(), None);
    assert_eq!(runs, 1);
    assert_eq!(test_clock::reads(), 2, "the gate-off calls read no clock");
    assert_eq!(sink.snapshot().shape_requests, 1, "and write no counter");
    test_clock::remove();
}

#[test]
fn nested_shaping_and_rasterizing_count_once_under_the_outer_timer() {
    // Glyph-zero shaping inside a rasterizer call is raster time, and a rasterizer reached from a
    // shaping request is shape time: only the outermost timer reads the clock.
    test_clock::install(0, 10);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        CountingRasterizer::new(&mut ShapingRasterizer).rasterize(glyph('a'));
        shape_request(|| {
            let mut scripted = Scripted::new(vec![Some(coverage_tile(1, 1))]);
            CountingRasterizer::new(&mut scripted).rasterize(glyph('b'))
        });
    }
    let stats = sink.snapshot();
    assert_eq!((stats.raster_ns, stats.shape_ns), (10, 10));
    assert_eq!((stats.raster_calls, stats.shape_requests), (2, 2));
    assert_eq!(test_clock::reads(), 4, "two outer timers, two readings each");
    test_clock::remove();
}

#[test]
fn a_rasterizer_call_counts_once_and_a_tile_only_when_it_has_pixels() {
    // No tile and an empty tile are calls but not tiles; an atlas hit calls nothing.
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        let mut scripted =
            Scripted::new(vec![None, Some(coverage_tile(0, 0)), Some(coverage_tile(2, 2))]);
        let mut counting = CountingRasterizer::new(&mut scripted);
        for character in ['a', 'b', 'c'] {
            counting.rasterize(glyph(character));
        }
        let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(64, 64);
        let mut scripted = Scripted::new(vec![Some(coverage_tile(2, 2))]);
        for _ in 0..2 {
            atlas.get_or_insert(glyph('d'), &mut CountingRasterizer::new(&mut scripted));
        }
    }
    let stats = sink.snapshot();
    assert_eq!((stats.raster_calls, stats.raster_tiles), (4, 2));
}

#[test]
fn an_attempt_folds_its_own_work_once_and_an_apply_attempt_into_both_classes() {
    // Clock step 10. Shaping before any attempt adds to the phase only; the apply attempt (enter,
    // shape pair, raster pair, close) and the retry after it (enter, shape pair, close) each fold
    // once, and only the apply attempt joins the apply class.
    test_clock::install(0, 10);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        shape_request(|| ());
        {
            let _attempt = AttemptScope::enter(true);
            shape_request(|| ());
            let mut scripted = Scripted::new(vec![Some(coverage_tile(2, 2))]);
            CountingRasterizer::new(&mut scripted).rasterize(glyph('a'));
            note_attempt_presented();
            note_attempt_presented();
        }
        {
            let _retry = AttemptScope::enter(false);
            shape_request(|| ());
        }
    }
    let stats = sink.snapshot();
    assert_eq!((stats.shape_ns, stats.raster_ns, stats.shape_requests), (30, 10, 3));
    let apply = AttemptStats {
        attempts: 1,
        presented: 1,
        attempt_ns: 50,
        shape_ns: 10,
        raster_ns: 10,
        shape_requests: 1,
        raster_calls: 1,
        raster_tiles: 1,
    };
    assert_eq!(stats.apply_attempts, apply);
    let retry = AttemptStats {
        attempts: 1,
        attempt_ns: 30,
        shape_ns: 10,
        shape_requests: 1,
        ..AttemptStats::ZERO
    };
    let mut every = apply;
    every.add(&retry);
    assert_eq!(stats.attempts, every);
    test_clock::remove();
}

#[test]
fn a_helper_of_the_same_renderer_joins_its_attempt_and_any_other_renderer_is_isolated() {
    // notification_layout opens its own scope on the drawing renderer's sink, so its shaping joins
    // that renderer's attempt once. Another renderer, or a non-counting one, starts outside every
    // attempt and with no open timer, and the outer attempt resumes when it closes.
    test_clock::install(0, 1);
    let sink = FrameStatsSink::default();
    let other = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        let _attempt = AttemptScope::enter(false);
        {
            let _helper = CollectGuard::enter(Some(&sink));
            shape_request(|| ());
        }
        {
            let _quiet = CollectGuard::enter(None);
            shape_request(|| ());
        }
        {
            let _outer_timer = WorkTimer::start(TimedWork::Raster);
            let _other = CollectGuard::enter(Some(&other));
            assert_eq!(ATTEMPT.with(Cell::get), None, "another renderer is outside any attempt");
            assert_eq!(TIMING_DEPTH.with(Cell::get), 0, "and its timers start unnested");
            shape_request(|| ());
        }
        assert!(ATTEMPT.with(Cell::get).is_some(), "the outer attempt resumes");
    }
    let stats = sink.snapshot();
    assert_eq!((stats.shape_requests, stats.attempts.shape_requests), (1, 1));
    assert_eq!((other.snapshot().shape_requests, other.snapshot().shape_ns), (1, 1));
    assert_eq!(other.snapshot().attempts, AttemptStats::ZERO);
    test_clock::remove();
}

#[test]
fn a_panic_inside_a_request_or_another_renderer_restores_the_timers_and_the_attempt() {
    // A failing request is still counted and timed once, and its timer is closed on unwind; a
    // renderer that panics inside another's attempt restores that attempt and its timer depth.
    test_clock::install(0, 5);
    let sink = FrameStatsSink::default();
    let other = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        let _attempt = AttemptScope::enter(false);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shape_request(|| -> u8 { panic!("shaping failed") })
        }));
        assert!(caught.is_err());
        assert_eq!(TIMING_DEPTH.with(Cell::get), 0);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _other = CollectGuard::enter(Some(&other));
            let _timer = WorkTimer::start(TimedWork::Raster);
            panic!("another renderer failed");
        }));
        assert!(caught.is_err());
        assert!(ATTEMPT.with(Cell::get).is_some());
        assert_eq!(TIMING_DEPTH.with(Cell::get), 0);
        shape_request(|| ());
    }
    let stats = sink.snapshot();
    assert_eq!((stats.attempts.shape_requests, stats.shape_ns), (2, 10));
    test_clock::remove();
}

#[test]
fn font_preparation_time_is_exact_split_by_generation_and_outside_every_attempt() {
    // A preparation reads the clock twice; a generation apply adds to both preparation fields.
    test_clock::install(0, 3);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        note_font_prepare(prepare_clock(), true);
        note_font_prepare(prepare_clock(), false);
    }
    let stats = sink.snapshot();
    assert_eq!((stats.font_prepare_ns, stats.font_generation_prepare_ns), (6, 3));
    assert_eq!(stats.attempts, AttemptStats::ZERO);
    note_font_prepare(prepare_clock(), true);
    assert_eq!(test_clock::reads(), 4, "the gate-off preparation reads no clock");
    test_clock::remove();
}

#[test]
fn an_attempt_nested_in_another_renderers_attempt_folds_into_its_own_renderer_even_on_unwind() {
    // A counting renderer that draws inside another's attempt keeps its own attempt and apply
    // class, and the outer attempt resumes with its own work; an inner attempt that panics still
    // folds once, unpresented, and restores the outer attempt and timer depth.
    let outer = FrameStatsSink::default();
    let inner = FrameStatsSink::default();
    {
        let mut outer_owed = false;
        let _outer = RenderScope::enter(Some(&outer), &mut outer_owed);
        shape_request(|| ());
        // An outer raster timer stays open across both inner attempts, so the depth they must
        // restore is 1, not 0: restoring 0 would let the outer renderer time nested work twice.
        let _outer_timer = WorkTimer::start(TimedWork::Raster);
        assert_eq!(TIMING_DEPTH.with(Cell::get), 1);
        {
            let mut inner_owed = true;
            let _inner = RenderScope::enter(Some(&inner), &mut inner_owed);
            shape_request(|| ());
            note_attempt_presented();
        }
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut inner_owed = true;
            let _inner = RenderScope::enter(Some(&inner), &mut inner_owed);
            shape_request(|| ());
            panic!("the inner renderer failed mid-attempt");
        }));
        assert!(caught.is_err());
        assert!(ATTEMPT.with(Cell::get).is_some(), "the outer attempt resumes");
        assert_eq!(TIMING_DEPTH.with(Cell::get), 1, "the outer timer is still open");
        // Nested inside the open outer timer, this request is counted but not timed again.
        shape_request(|| ());
    }
    assert_eq!(TIMING_DEPTH.with(Cell::get), 0);
    let inner = inner.snapshot();
    assert_eq!((inner.apply_attempts.attempts, inner.apply_attempts.presented), (2, 1));
    assert_eq!((inner.attempts.attempts, inner.attempts.shape_requests), (2, 2));
    let outer = outer.snapshot();
    assert_eq!((outer.attempts.attempts, outer.attempts.shape_requests), (1, 2));
    assert_eq!(outer.apply_attempts, AttemptStats::ZERO);
}

#[test]
fn every_glyph_atlas_insertion_counts_its_rasterizer() {
    // An insertion that passes the bare rasterizer goes uncounted. The eight sites are the four
    // terminal paths in core.rs, the shared chrome path (tabs, palette, search, preedit), the
    // working-set helper's ASCII pass and representative-codepoint resolver, and the test glyph
    // seams' solid tile in cursor.rs.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let insertions = atlas_insertions(&sources);
    let bare: Vec<_> = insertions.iter().filter(|(_, wrapped)| !wrapped).collect();
    assert!(bare.is_empty(), "uncounted glyph-atlas insertions: {bare:#?}");
    assert_eq!(insertions.len(), 8, "the insertion sites changed; review them: {insertions:#?}");
    assert!(insertions.iter().any(|(site, _)| site.starts_with("chrome_text.rs:")));
    assert!(insertions.iter().any(|(site, _)| site.starts_with("cursor.rs:")));
}

#[test]
fn the_insertion_scan_skips_option_get_or_insert() {
    // `Option::get_or_insert` takes one argument and is never an atlas insertion; a two-argument
    // call with a nested comma inside its key is still found.
    let fixture = "fn track(pending: &mut Option<u64>, atlas: &mut Atlas, wt: &mut Raster) {\n    \
                   pending.get_or_insert(f(1, 2));\n    atlas.get_or_insert(key(1, 2), wt);\n}\n";
    let found = atlas_insertions(&[("fixture.rs".to_owned(), fixture.to_owned())]);
    assert_eq!(found, vec![("fixture.rs:3".to_owned(), false)]);
}

#[test]
fn the_insertion_scan_finds_a_bare_rasterizer_across_lines_and_line_endings() {
    // A wrapped call split over lines counts as wrapped; a bare one does not, on LF and CRLF.
    let fixture = "fn draw(atlas: &mut Atlas, wt: &mut Raster) {\n    atlas.get_or_insert(key, wt);\n    \
                   atlas.get_or_insert(\n        key,\n        &mut CountingRasterizer::new(wt),\n    );\n}\n";
    for text in [fixture.to_owned(), to_crlf(fixture)] {
        let found = atlas_insertions(&[("fixture.rs".to_owned(), text)]);
        assert_eq!(
            found,
            vec![("fixture.rs:2".to_owned(), false), ("fixture.rs:3".to_owned(), true)]
        );
    }
}

#[test]
fn the_attempt_and_the_owed_apply_are_wired_through_the_production_entry_points() {
    // render_releasing opens the attempt inside its collection scope and takes the owed apply, so
    // exactly one attempt carries it; both presenters mark the attempt where a frame passes the
    // present boundary; begin_frame_fonts times the preparation and records the change.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let bodies = function_bodies(&sources);
    let body = |name: &str| {
        bodies
            .iter()
            .find(|function| function.name == name)
            .map(|function| function.body.split_whitespace().collect::<String>())
            .unwrap_or_else(|| panic!("{name} not found"))
    };
    // The order of the scopes and the preparation's arguments live in the seams, which the
    // attribution test drives; here only their use by the real entry points is pinned.
    let releasing = body("render_releasing");
    assert!(releasing
        .starts_with("{let_scope=crate::frame_stats::RenderScope::enter(self.frame_sink.as_ref(),&mutself.unattributed_apply,);"));
    assert!(
        !releasing.contains("AttemptScope::enter") && !releasing.contains("CollectGuard::enter")
    );
    let present = to_lf(include_str!("present.rs")).split_whitespace().collect::<String>();
    assert_eq!(
        present.matches("note_attempt_presented();Ok(PresentOutcome::Presented)").count(),
        2,
        "each presenter marks the attempt where it returns Presented"
    );
    let prepare = body("begin_frame_fonts");
    assert!(prepare.contains(
        "frame_fonts::prepare_and_owe(&mutself.applied_fonts,&mutself.unattributed_apply,"
    ));
    assert!(!prepare.contains("prepare_frame_fonts("), "preparation goes through the seam only");
    assert!(body("notification_layout").contains("CollectGuard::enter(self.frame_sink.as_ref())"));
}

/// A frame's damage waste is the permille of its single union rectangle minus the permille of the
/// exact area its parts cover: a tab band and a cursor row 100 px apart on a 240x160 surface waste
/// the gap between them; parts that exactly fill the union waste nothing; no surface wastes 0.
#[test]
fn damage_waste_is_the_union_share_minus_the_covered_share() {
    let cursor_row = PixelRect { x: 0, y: 22, w: 100, h: 20 };
    let tab_band = PixelRect { x: 0, y: 140, w: 240, h: 20 };
    let union = cursor_row.union(tab_band);
    // Union 240x138 = 33120 px is 862 permille; the parts cover 6800 px, 177 permille.
    assert_eq!(damage_waste_permille(&union, &[cursor_row, tab_band], 240, 160), 862 - 177);
    assert_eq!(damage_waste_permille(&tab_band, &[tab_band], 240, 160), 0);
    let halves =
        [PixelRect { x: 0, y: 0, w: 120, h: 160 }, PixelRect { x: 120, y: 0, w: 120, h: 160 }];
    let whole = PixelRect { x: 0, y: 0, w: 240, h: 160 };
    assert_eq!(damage_waste_permille(&whole, &halves, 240, 160), 0);
    assert_eq!(damage_waste_permille(&union, &[cursor_row], 0, 160), 0);
}

/// The waste is summed per damaged frame beside the damage share, and is never computed with the
/// counting gate off.
#[test]
fn damage_waste_is_summed_only_inside_a_counting_scope() {
    note_damage_waste(|| panic!("waste computed with no counting scope"));
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        note_damage(|| 862);
        note_damage_waste(|| 685);
        note_damage(|| 100);
        note_damage_waste(|| 0);
    }
    let stats = sink.snapshot();
    assert_eq!((stats.damage_waste_permille_sum, stats.damaged_frames), (685, 2));
    let mut total = FrameStats::ZERO;
    total.add(&stats);
    total.add(&stats);
    assert_eq!(total.damage_waste_permille_sum, 1370);
}

/// The partial-assembly counters record only inside a counting scope: a presented frame counts as
/// partial only when its mode was `Partial`, each fallback counts once, and the hashed cells are
/// summed lazily, so with the gate off the cell count is never computed. `add` folds all three.
#[test]
fn partial_counters_record_only_inside_a_counting_scope() {
    note_partial_frame(true);
    note_partial_fallback();
    note_row_cells_hashed(|| panic!("cells counted with no counting scope"));
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        note_partial_frame(true);
        note_partial_frame(false);
        note_partial_fallback();
        note_row_cells_hashed(|| 80);
        note_row_cells_hashed(|| 40);
    }
    let stats = sink.snapshot();
    assert_eq!(
        (stats.partial_frames, stats.partial_fallbacks, stats.row_cells_hashed),
        (1, 1, 120)
    );
    let mut total = FrameStats::ZERO;
    total.add(&stats);
    total.add(&stats);
    assert_eq!(
        (total.partial_frames, total.partial_fallbacks, total.row_cells_hashed),
        (2, 2, 240)
    );
}

#[test]
fn title_and_chrome_run_notes_count_only_inside_a_counting_scope() {
    // With the gate on, each note moves exactly its own counter by one; with the gate off (no
    // scope, or a scope of a renderer that does not count) none of the four moves.
    let sink = FrameStatsSink::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        note_tab_title(true);
        note_tab_title(false);
        note_tab_title(false);
        note_chrome_run(true);
        note_chrome_run(true);
        note_chrome_run(true);
        note_chrome_run(false);
    }
    let on = sink.snapshot();
    assert_eq!(
        (on.tab_title_reuses, on.tab_title_prepares, on.chrome_run_reuses, on.chrome_run_prepares),
        (1, 2, 3, 1)
    );
    let off = FrameStatsSink::default();
    for counting in [false, true] {
        let _scope = counting.then(|| CollectGuard::enter(None));
        note_tab_title(true);
        note_tab_title(false);
        note_chrome_run(true);
        note_chrome_run(false);
    }
    assert_eq!(off.snapshot(), FrameStats::ZERO, "the gate off moves no counter");
    assert_eq!(sink.snapshot(), on, "and nothing reached the counting renderer afterwards");
}
