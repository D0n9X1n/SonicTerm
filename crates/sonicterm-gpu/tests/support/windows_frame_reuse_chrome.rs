//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// The tab-title reuse and prepare counters now.
pub(super) fn title_counts(renderer: &GpuRenderer) -> (u64, u64, u64) {
    let stats = renderer.frame_stats();
    (stats.tab_title_reuses, stats.tab_title_prepares, stats.shape_requests)
}

/// T15: three tabs, one cut, on a warm renderer: a forced assembly reuses all three titles with no
/// prepare and no shaping, and draws the same pixels; each face replacement then makes the next
/// assembly prepare every title again. The warm counts are read across a single render attempt.
pub(super) fn renderer_reuses_titles_and_clears_on_face_replacement(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 240), true)?;
    let long = "a very long tab title that cannot fit its tab and must be cut short";
    let mut scene = single_scene(&renderer, &["shell", "logs", long]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let warm_pixels = pixels(&mut renderer)?;
    let (before, after) = counted_forced(&mut renderer, &mut scene, |_| {}, title_counts)?;
    check_settled(&scene, settled, "warm titles")?;
    check(after.0 - before.0 == 3, &format!("three warm reuses: {before:?} -> {after:?}"))?;
    check(after.1 == before.1, "no title prepares on a warm assembly")?;
    check(after.2 == before.2, "a warm assembly of ASCII rows and kept titles shapes nothing")?;
    check(pixels(&mut renderer)? == warm_pixels, "the reused titles draw the same pixels")?;

    let fonts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono");
    let adopted = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![fonts],
        16.0,
        72,
        1.0,
    )
    .map_err(|error| error.to_string())?;
    type Replace = Box<dyn Fn(&mut GpuRenderer)>;
    let replacements: [(&str, Replace); 4] = [
        ("set_font", Box::new(|renderer| renderer.set_font("Rec Mono St.Helens", 17.0, 1.0, 1.0))),
        (
            "adopted body stack",
            Box::new(move |renderer| {
                renderer.__test_adopt_body_font_stack("adopted", adopted.clone())
            }),
        ),
        (
            "same-scale rebuild",
            Box::new(|renderer| {
                let scale = renderer.scale_factor();
                renderer.force_rebuild_for_scale(scale);
            }),
        ),
        ("clear_shape_cache", Box::new(GpuRenderer::clear_shape_cache)),
    ];
    for (name, replace) in replacements {
        forced(&mut renderer, &mut scene)?;
        replace(&mut renderer);
        let before = title_counts(&renderer);
        forced(&mut renderer, &mut scene)?;
        let after = title_counts(&renderer);
        check(after.1 - before.1 >= 3, &format!("{name}: every title prepares again"))?;
    }
    Ok(())
}

/// T16: over 200 forced assemblies with changing titles, a renderer that keeps titles draws the
/// pixels of one that keeps none, on wgpu and on GDI. Both renderers settle their font fallback
/// first, and every comparison checks that neither applied a newer generation since.
pub(super) fn cached_titles_draw_like_cold_titles(active: &ActiveEventLoop) -> Result<(), String> {
    let titles = ["shell", "logs", "build ~/src/sonicterm", "é ñ ü", "中文标题", "vim main.rs"];
    for mode in MODES {
        let (_window, mut warm) = renderer(active, mode, &Theme::default(), (320, 120), true)?;
        let (_cold_window, mut cold) = renderer(active, mode, &Theme::default(), (320, 120), true)?;
        cold.__set_frame_reuse(false);
        // Every title is drawn once on each renderer and its fallback faces settle first, so no
        // comparison races an asynchronous fallback discovery.
        let mut warm_all = single_scene(&warm, &titles);
        let warm_settled = settle_fallback(&mut warm, &mut warm_all)?;
        let mut cold_all = single_scene(&cold, &titles);
        let cold_settled = settle_fallback(&mut cold, &mut cold_all)?;
        for assembly in 0..200usize {
            let chosen: Vec<&str> =
                (0..3).map(|offset| titles[(assembly / 3 + offset) % titles.len()]).collect();
            let mut warm_scene = single_scene(&warm, &chosen);
            let mut cold_scene = single_scene(&cold, &chosen);
            forced(&mut warm, &mut warm_scene)?;
            forced(&mut cold, &mut cold_scene)?;
            check(
                (warm_scene.generation, cold_scene.generation)
                    == (Some(warm_settled), Some(cold_settled)),
                &format!(
                    "{mode:?} assembly {assembly}: no fallback generation applied after settling \
                     ({:?}, {:?})",
                    warm_scene.generation, cold_scene.generation
                ),
            )?;
            check(
                pixels(&mut warm)? == pixels(&mut cold)?,
                &format!("{mode:?} assembly {assembly}: kept titles draw like cold titles"),
            )?;
        }
    }
    Ok(())
}

/// T19: the palette is seeded from the constructor theme, so the first frame derives none; after
/// `set_theme(A)` a frame rendered with theme B draws B's tab bar, as a renderer that only ever
/// rendered B does. Both renderers settle their font fallback before the pixels are compared.
pub(super) fn renderer_palette_follows_the_render_theme(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let theme_a = Theme::default();
    let mut theme_b = Theme::default();
    theme_b.colors.tab.active_fg = Hex("#ff00aa".to_string());
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &theme_a, (640, 240), true)?;
    let mut scene = single_scene(&candidate, &["shell", "logs"]);
    let candidate_settled = settle_fallback(&mut candidate, &mut scene)?;
    check(candidate.__palette_computes() == 0, "the constructor theme's palette is seeded")?;
    candidate.set_theme(&theme_a);
    scene.theme = theme_b.clone();
    forced(&mut candidate, &mut scene)?;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &theme_a, (640, 240), true)?;
    let mut oracle_scene = single_scene(&oracle, &["shell", "logs"]);
    oracle_scene.theme = theme_b;
    let oracle_settled = settle_fallback(&mut oracle, &mut oracle_scene)?;
    check_settled(&scene, candidate_settled, "theme B candidate")?;
    check_settled(&oracle_scene, oracle_settled, "theme B oracle")?;
    check(pixels(&mut candidate)? == pixels(&mut oracle)?, "the render theme's palette is drawn")?;
    check(candidate.__palette_computes() == 1, "the changed colors derive the palette once")
}

/// The chrome-run reuse and prepare counters and the shaping requests now.
pub(super) fn run_counts(renderer: &GpuRenderer) -> (u64, u64, u64) {
    let stats = renderer.frame_stats();
    (stats.chrome_run_reuses, stats.chrome_run_prepares, stats.shape_requests)
}

/// T25: with search open on a fixed query and the tab bar hidden, a cold assembly prepares the
/// icon and label runs (2) and reuses them three times; a warm one prepares none, reuses five and
/// shapes nothing; and the overlay draws as a renderer that keeps no runs, on wgpu and on GDI.
/// Each count is read across a single render attempt, and both renderers settle their font
/// fallback before the pixels are compared.
pub(super) fn search_overlay_reuses_its_runs(active: &ActiveEventLoop) -> Result<(), String> {
    for mode in MODES {
        let (_window, mut warm) = renderer(active, mode, &Theme::default(), (640, 240), false)?;
        let (_cold_window, mut cold) =
            renderer(active, mode, &Theme::default(), (640, 240), false)?;
        cold.__set_frame_reuse(false);
        let mut scenes = Vec::new();
        for target in [&warm, &cold] {
            let mut scene = single_scene(target, &["shell"]);
            let mut search = SearchState::new();
            search.bind_pane(1);
            search.set_query("abc", &scene.panes[0].grid);
            scene.search = Some(search);
            scenes.push(scene);
        }
        let (warm_scene, cold_scene) = scenes.split_at_mut(1);
        let (warm_scene, cold_scene) = (&mut warm_scene[0], &mut cold_scene[0]);
        // Settling fills the atlas; the cache is then emptied before each try at a cold assembly.
        let warm_settled = settle_fallback(&mut warm, warm_scene)?;
        let cold_settled = settle_fallback(&mut cold, cold_scene)?;
        let (before, cold_counts) =
            counted_forced(&mut warm, warm_scene, GpuRenderer::clear_shape_cache, run_counts)?;
        check(
            (cold_counts.1 - before.1, cold_counts.0 - before.0) == (2, 3),
            &format!(
                "{mode:?}: a cold overlay prepares 2 and reuses 3: {before:?} -> {cold_counts:?}"
            ),
        )?;
        let (cold_counts, warm_counts) = counted_forced(&mut warm, warm_scene, |_| {}, run_counts)?;
        check(
            (warm_counts.1 - cold_counts.1, warm_counts.0 - cold_counts.0) == (0, 5),
            &format!("{mode:?}: a warm overlay reuses 5: {cold_counts:?} -> {warm_counts:?}"),
        )?;
        check(warm_counts.2 == cold_counts.2, "a warm overlay shapes nothing")?;
        forced(&mut cold, cold_scene)?;
        forced(&mut cold, cold_scene)?;
        check_settled(warm_scene, warm_settled, &format!("{mode:?} warm overlay"))?;
        check_settled(cold_scene, cold_settled, &format!("{mode:?} cold overlay"))?;
        check(
            pixels(&mut warm)? == pixels(&mut cold)?,
            &format!("{mode:?}: kept runs draw the overlay as cold runs do"),
        )?;
    }
    Ok(())
}
