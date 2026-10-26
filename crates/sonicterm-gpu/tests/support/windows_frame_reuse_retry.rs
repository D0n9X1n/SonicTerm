//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// Draw `scene` once with the retained frame forgotten, so the frame assembles, and report that
/// one call without folding a retry. One line is printed per frame, whatever its outcome.
pub(super) fn single_frame(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    label: &str,
) -> SingleFrame {
    renderer.invalidate_retained_frame();
    let before = renderer.frame_stats();
    let resets_before = renderer.__test_glyph_atlas_resets();
    let (outcome, allocations) = allocations_of(|| frame(renderer, scene));
    let after = renderer.frame_stats();
    let facts = renderer.glyph_atlas_facts();
    let measured = SingleFrame {
        attempts: after.attempts.attempts - before.attempts.attempts,
        presented: after.attempts.presented - before.attempts.presented,
        hits: after.row_cache_hits - before.row_cache_hits,
        misses: after.row_cache_misses - before.row_cache_misses,
        resets: renderer.__test_glyph_atlas_resets() - resets_before,
        allocations,
        outcome,
    };
    println!(
        "frame {label}: {:?}, attempts {}, hits {}, misses {}, atlas {}, resets {}, growths {}, \
         evictions {}, scratch {}",
        measured.outcome,
        measured.attempts,
        measured.hits,
        measured.misses,
        facts.dim,
        measured.resets,
        facts.growths,
        facts.evictions,
        renderer.retained_amounts().frame_scratch.bytes,
    );
    measured
}

/// Whether `measured` presented in exactly one attempt.
pub(super) fn presented_once(measured: &SingleFrame) -> bool {
    matches!(measured.outcome, PresentOutcome::Presented)
        && measured.attempts == 1
        && measured.presented == 1
}

/// Visible rows of every pane of `scene`, one row-cache entry each since every row's text differs.
pub(super) fn visible_rows(scene: &Scene) -> u64 {
    scene.panes.iter().map(|pane| u64::from(pane.grid.rows)).sum()
}

/// The first presented frame after an eviction-disabled atlas retry admits every visible row, and
/// the retry's settlement keeps them. Steps, each one `frame()` call:
/// - A: the injected atlas change retries and presents nothing (pin);
/// - B: the first recovered presentation misses every visible row (pin);
/// - C: misses nothing and hits every row, within the steady allocation bound. Regression check:
///   when settlement cleared the rows a second time, C missed every row;
/// - D: misses nothing again, so no delayed clear, and draws the oracle's pixels (pins).
///
/// Then, with a known grid edit and a retry pending, a failed presentation (B′) settles nothing
/// and acknowledges no dirt; the next presentation (C′) settles once and acknowledges it (pins).
/// The fallback generation is checked after every presented frame.
pub(super) fn atlas_retry_keeps_rebuilt_rows(active: &ActiveEventLoop) -> Result<(), String> {
    use tracing_subscriber::layer::SubscriberExt;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let mut scene = single_scene(&candidate, &["shell"]);
    let mut oracle_scene = single_scene(&oracle, &["shell"]);
    let settled = settle_fallback(&mut candidate, &mut scene)?;
    let oracle_settled = settle_fallback(&mut oracle, &mut oracle_scene)?;
    let rows = visible_rows(&scene);
    let mut steady_bound = 0;
    for index in 0..3 {
        let steady = single_frame(&mut candidate, &mut scene, &format!("steady {index}"));
        check(presented_once(&steady) && steady.misses == 0, "the settled scene is steady")?;
        steady_bound = steady_bound.max(steady.allocations);
    }

    candidate.__change_glyph_atlas_during_next_assembly();
    let frame_a = single_frame(&mut candidate, &mut scene, "A");
    check(
        matches!(frame_a.outcome, PresentOutcome::AtlasRetry) && frame_a.presented == 0,
        "A: the injected atlas change retries and presents nothing",
    )?;
    let frame_b = single_frame(&mut candidate, &mut scene, "B");
    check(presented_once(&frame_b), "B: the first recovered frame presents")?;
    check(
        frame_b.misses == rows && frame_b.hits == 0,
        &format!("B: misses every row ({} of {rows}, hits {})", frame_b.misses, frame_b.hits),
    )?;
    check_settled(&scene, settled, "B")?;
    let frame_c = single_frame(&mut candidate, &mut scene, "C");
    check(presented_once(&frame_c), "C: presents")?;
    // Regression check: the settled retry kept the rows B admitted.
    check(
        frame_c.misses == 0 && frame_c.hits == rows,
        &format!(
            "C: reuses every row (misses {}, hits {} of {rows})",
            frame_c.misses, frame_c.hits
        ),
    )?;
    check(
        frame_c.allocations <= steady_bound,
        &format!("C: within the steady bound ({} > {steady_bound})", frame_c.allocations),
    )?;
    check_settled(&scene, settled, "C")?;
    let frame_d = single_frame(&mut candidate, &mut scene, "D");
    check(presented_once(&frame_d) && frame_d.misses == 0, "D: no delayed clear")?;
    check(
        frame_d.allocations <= steady_bound,
        &format!("D: within the steady bound ({} > {steady_bound})", frame_d.allocations),
    )?;
    check_settled(&scene, settled, "D")?;
    forced(&mut oracle, &mut oracle_scene)?;
    check_settled(&oracle_scene, oracle_settled, "oracle")?;
    check(pixels(&mut candidate)? == pixels(&mut oracle)?, "D draws the oracle's pixels")?;

    // A failed presentation while the retry is pending settles nothing and acknowledges nothing.
    let settlements = Arc::new(AtomicUsize::new(0));
    let subscriber =
        tracing_subscriber::Registry::default().with(SettlementCounter(Arc::clone(&settlements)));
    sonicterm_logging::test_capture::with_default(subscriber, || -> Result<(), String> {
        write(&mut scene.panes[0].grid, 0, 0, "edited row");
        check(scene.panes[0].grid.dirty_count() > 0, "precondition: the edit is dirt")?;
        candidate.__change_glyph_atlas_during_next_assembly();
        let retried = single_frame(&mut candidate, &mut scene, "A'");
        check(matches!(retried.outcome, PresentOutcome::AtlasRetry), "A': retries")?;
        check(candidate.__test_glyph_atlas_retry_state() == (true, false), "A': retry armed")?;
        candidate.__fail_next_present();
        let failed = single_frame(&mut candidate, &mut scene, "B'");
        check(
            matches!(failed.outcome, PresentOutcome::Failed(_)) && failed.misses > 0,
            "B': assembles rows, then fails presentation",
        )?;
        check(
            candidate.__test_glyph_atlas_retry_state() == (true, false),
            "B': a failed presentation leaves the retry pending",
        )?;
        check(settlements.load(Ordering::SeqCst) == 0, "B': nothing settled")?;
        check(scene.panes[0].grid.dirty_count() > 0, "B': the edit is still dirt")?;
        let recovered = single_frame(&mut candidate, &mut scene, "C'");
        check(presented_once(&recovered), "C': presents")?;
        check(recovered.misses == 0, "C': reuses the rows the failed frame admitted")?;
        check(
            candidate.__test_glyph_atlas_retry_state() == (false, true),
            "C': the presentation settles the retry",
        )?;
        check(settlements.load(Ordering::SeqCst) == 1, "C': settled exactly once")?;
        check(scene.panes[0].grid.dirty_count() == 0, "C': the edit is acknowledged")?;
        check_settled(&scene, settled, "C'")?;
        let after = single_frame(&mut candidate, &mut scene, "D'");
        check(presented_once(&after), "D': presents")?;
        check(settlements.load(Ordering::SeqCst) == 1, "D': no further settlement")?;
        check_settled(&scene, settled, "D'")
    })
}

/// One pane of exactly two rows: `first` on row 0 and `second` on row 1, no tab bar.
pub(super) fn two_row_scene(renderer: &GpuRenderer, first: &str, second: &str) -> Scene {
    let (width_px, _) = renderer.surface_size();
    let (cell_w, cell_h) = renderer.cell_size();
    let rect = PixelRect { x: 0, y: 0, w: width_px, h: (2.0 * cell_h).ceil() as u32 };
    let cols = ((rect.w as f32 / cell_w).floor() as u16).max(1);
    let mut grid = Grid::new(cols, 2);
    write(&mut grid, 1, 0, second);
    write(&mut grid, 0, 0, first);
    Scene {
        panes: vec![ScenePane { id: 1, rect, grid }],
        tabs: TabBar::new(),
        search: None,
        theme: Theme::default(),
        generation: None,
    }
}

/// A retried frame whose capped atlas admits one complete row and refuses a glyph of the other
/// keeps the complete row when the retry settles. A probe renderer measures the atlas entries the
/// first row alone needs; the candidate's retry runs at that cap, so its second row is refused
/// (asserted from the frame's missing characters, not assumed). With the cap raised, the next
/// frame presents with no reset and nothing missing, and draws the oracle's pixels (pins).
/// Regression check: that frame hits the complete row and misses only the refused one; when
/// settlement cleared every row, it missed both.
pub(super) fn atlas_retry_keeps_the_complete_row_of_a_refused_frame(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (_probe_window, mut probe) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let mut probe_scene = two_row_scene(&probe, "abc", "");
    present(&mut probe, &mut probe_scene)?;
    let cap = probe.retained_amounts().glyph_atlas.items;
    check(cap > 0, "the probe measured the first row's atlas entries")?;

    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let mut scene = two_row_scene(&candidate, "abc", "xyz");
    let mut oracle_scene = two_row_scene(&oracle, "abc", "xyz");
    let settled = settle_fallback(&mut candidate, &mut scene)?;
    let oracle_settled = settle_fallback(&mut oracle, &mut oracle_scene)?;

    candidate.__change_glyph_atlas_during_next_assembly();
    let retried = single_frame(&mut candidate, &mut scene, "refusal A");
    check(matches!(retried.outcome, PresentOutcome::AtlasRetry), "the atlas change retries")?;
    candidate.__set_glyph_atlas_entry_cap(cap);
    let capped = single_frame(&mut candidate, &mut scene, "refusal B");
    check(presented_once(&capped), "the capped retry presents")?;
    check(capped.misses == 2 && capped.hits == 0, "the retried frame shapes both rows")?;
    check(
        candidate.retained_amounts().glyph_atlas.items == cap,
        "the capped atlas is full after the retried frame",
    )?;
    check(
        !candidate.last_missing_tofu().is_empty(),
        "the second row's glyph was refused, so the frame drew a missing outline",
    )?;
    let resets = candidate.__test_glyph_atlas_resets();

    candidate.__set_glyph_atlas_entry_cap(sonicterm_text::glyph_atlas::MAX_ATLAS_ENTRIES);
    let recovered = single_frame(&mut candidate, &mut scene, "refusal C");
    check(presented_once(&recovered), "the frame after the cap is raised presents")?;
    check(candidate.__test_glyph_atlas_resets() == resets, "no further reset")?;
    check(
        candidate.last_missing_tofu().is_empty() && candidate.last_missing_chrome().is_empty(),
        "nothing is missing once the cap is raised",
    )?;
    // Regression check: the complete row was kept; only the refused row is shaped again.
    check(
        recovered.hits == 1 && recovered.misses == 1,
        &format!(
            "one row hits and one misses (hits {}, misses {})",
            recovered.hits, recovered.misses
        ),
    )?;
    check_settled(&scene, settled, "refusal recovery")?;
    forced(&mut oracle, &mut oracle_scene)?;
    check_settled(&oracle_scene, oracle_settled, "refusal oracle")?;
    check(
        pixels(&mut candidate)? == pixels(&mut oracle)?,
        "the recovered frame draws the oracle's pixels",
    )
}

/// A trim between an atlas retry and its present keeps the retry armed and eviction off and adds no
/// reset; the first recovered present settles it once, with the reference pixels and generation, and
/// the next frames settle nothing more. Steps, each one `frame()` call:
/// - A retries; the trim is admitted, keeps the retry and adds no reset or settlement (pins);
/// - B, the first recovered presentation, refills every visible row and settles once (pins);
/// - C hits every row and misses none. Regression check: when settlement cleared the rows the
///   trim's recovery had just admitted, C missed every row;
/// - D misses none again, so no delayed clear; pixels equal the reference (pins).
///
/// The trim releases scratch on purpose, so no scratch-retention bound applies here.
pub(super) fn trim_during_a_pending_atlas_retry(active: &ActiveEventLoop) -> Result<(), String> {
    use tracing_subscriber::layer::SubscriberExt;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let reference = pixels(&mut renderer)?;
    let rows = visible_rows(&scene);
    let settlements = Arc::new(AtomicUsize::new(0));
    let subscriber =
        tracing_subscriber::Registry::default().with(SettlementCounter(Arc::clone(&settlements)));
    sonicterm_logging::test_capture::with_default(subscriber, || -> Result<(), String> {
        renderer.__change_glyph_atlas_during_next_assembly();
        let frame_a = single_frame(&mut renderer, &mut scene, "trim A");
        check(matches!(frame_a.outcome, PresentOutcome::AtlasRetry), "the changed atlas retries")?;
        check(renderer.__test_glyph_atlas_retry_state() == (true, false), "the retry is armed")?;
        let resets = renderer.__test_glyph_atlas_resets();
        let report = renderer.trim_for_occlusion();
        check(!report.refused, "the trim is admitted")?;
        check(renderer.__test_glyph_atlas_retry_state() == (true, false), "the trim keeps it")?;
        check(renderer.__test_glyph_atlas_resets() == resets, "the trim adds no reset")?;
        check(settlements.load(Ordering::SeqCst) == 0, "the trim settles nothing")?;

        let frame_b = single_frame(&mut renderer, &mut scene, "trim B");
        check(presented_once(&frame_b), "the first recovered frame presents once")?;
        check(
            frame_b.misses == rows && frame_b.hits == 0,
            &format!("B refills every row ({} of {rows}, hits {})", frame_b.misses, frame_b.hits),
        )?;
        check(renderer.__test_glyph_atlas_retry_state() == (false, true), "the retry settled")?;
        check(settlements.load(Ordering::SeqCst) == 1, "the settlement is logged once")?;
        check_settled(&scene, settled, "settled retry")?;
        check(pixels(&mut renderer)? == reference, "the settled frame equals the reference")?;

        let frame_c = single_frame(&mut renderer, &mut scene, "trim C");
        check(presented_once(&frame_c), "C presents once")?;
        // Regression check: the settled retry kept the rows the trim's recovery admitted.
        check(
            frame_c.misses == 0 && frame_c.hits == rows,
            &format!(
                "C reuses every row (misses {}, hits {} of {rows})",
                frame_c.misses, frame_c.hits
            ),
        )?;
        check(frame_c.resets == 0, "C adds no reset")?;
        check(settlements.load(Ordering::SeqCst) == 1, "C settles nothing more")?;
        check_settled(&scene, settled, "C")?;

        let frame_d = single_frame(&mut renderer, &mut scene, "trim D");
        check(presented_once(&frame_d) && frame_d.misses == 0, "D: no delayed clear")?;
        check(renderer.__test_glyph_atlas_retry_state() == (false, true), "still settled")?;
        check(settlements.load(Ordering::SeqCst) == 1, "no second settlement")?;
        check(renderer.__test_glyph_atlas_resets() == resets, "no reset afterwards")?;
        check_settled(&scene, settled, "D")?;
        check(pixels(&mut renderer)? == reference, "the next frame equals the reference")
    })
}
