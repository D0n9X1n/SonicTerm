//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// T7: forced assemblies reuse the frame scratch: ten forced frames with one changed cell each
/// allocate fewer times with reuse on than off, by at least one per non-empty scratch vector per
/// frame, and the last frames' pixels are equal. Each renderer settles its font fallback first.
pub(super) fn assembled_frames_reuse_scratch(active: &ActiveEventLoop) -> Result<(), String> {
    let mut runs = Vec::new();
    for reuse in [true, false] {
        let (_window, mut renderer) =
            renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
        renderer.__set_frame_reuse(reuse);
        let mut scene = single_scene(&renderer, &["shell"]);
        let settled = settle_fallback(&mut renderer, &mut scene)?;
        // Every edit's glyphs are drawn once first, so no measured frame rasterizes or grows the
        // atlas and both sides count only their assembly.
        for index in 0..10u16 {
            write(&mut scene.panes[0].grid, 0, 0, &format!("edit {index:02}"));
            forced(&mut renderer, &mut scene)?;
        }
        for _ in 0..5 {
            forced(&mut renderer, &mut scene)?;
        }
        let vectors = renderer.retained_amounts().frame_scratch.items;
        let (outcome, allocations) = allocations_of(|| -> Result<(), String> {
            for index in 0..10u16 {
                write(&mut scene.panes[0].grid, 0, 0, &format!("edit {index:02}"));
                forced(&mut renderer, &mut scene)?;
            }
            Ok(())
        });
        outcome?;
        check_settled(&scene, settled, &format!("reuse {reuse}"))?;
        runs.push((allocations, vectors, pixels(&mut renderer)?));
    }
    let (with_reuse, vectors, reused_pixels) = &runs[0];
    let (without_reuse, _, cold_pixels) = &runs[1];
    check(*vectors > 0, "the warm scratch holds allocated vectors")?;
    check(
        with_reuse + 10 * vectors <= *without_reuse,
        &format!("reuse saves a vector per frame: {with_reuse} vs {without_reuse} ({vectors})"),
    )?;
    check(reused_pixels == cold_pixels, "reuse draws the same pixels")
}

/// Draw one forced frame of `scene`, counting its allocations and reading the identity figures
/// that say whether it refilled a cache. Atlas growths are counted after assembly in the same
/// render call, so the growth delta is exact; unchanged atlas bytes (pixels plus the dirty list)
/// are a stricter stability condition on top of it.
pub(super) fn measured_forced(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
) -> Result<ForcedFrame, String> {
    let before = renderer.frame_stats();
    let atlas_before = renderer.retained_amounts().glyph_atlas.bytes;
    let resets_before = renderer.__test_glyph_atlas_resets();
    let (outcome, allocations) = allocations_of(|| forced(renderer, scene));
    outcome?;
    let after = renderer.frame_stats();
    let atlas_after = renderer.retained_amounts().glyph_atlas.bytes;
    let attempts = after.attempts.attempts - before.attempts.attempts;
    let row_misses = after.row_cache_misses - before.row_cache_misses;
    let growths = after.glyph_atlas_growths - before.glyph_atlas_growths;
    let resets = renderer.__test_glyph_atlas_resets() - resets_before;
    let scratch_bytes = renderer.retained_amounts().frame_scratch.bytes;
    Ok(ForcedFrame {
        allocations,
        steady: attempts == 1
            && row_misses == 0
            && growths == 0
            && atlas_after == atlas_before
            && resets == 0,
        scratch_bytes,
        figures: format!(
            "allocations {allocations}, attempts {attempts}, row misses {row_misses}, row hits {}, \
             atlas growths {growths}, atlas bytes {atlas_before} -> {atlas_after}, resets {resets}, \
             scratch {scratch_bytes}",
            after.row_cache_hits - before.row_cache_hits
        ),
    })
}

/// T8: after a failed assembly, a failed presentation and an atlas retry, the scratch keeps its
/// capacity, never grows while the caches refill, and the first steady frame allocates no more
/// than a steady frame before the fault and draws what a renderer with no failures draws. The
/// partial fallback exit has no renderer seam; the device-free driver covers it. Both renderers
/// settle their font fallback before any pixels are compared.
pub(super) fn scratch_survives_failed_and_retried_frames(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (_window, mut candidate) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let (_oracle_window, mut oracle) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (640, 360), false)?;
    let mut scene = single_scene(&candidate, &["shell"]);
    let mut oracle_scene = single_scene(&oracle, &["shell"]);
    let candidate_settled = settle_fallback(&mut candidate, &mut scene)?;
    let oracle_settled = settle_fallback(&mut oracle, &mut oracle_scene)?;
    // The most allocations a steady forced frame of this unchanged scene makes, over three
    // steady frames; a frame that still refilled a cache is not a steady sample.
    let mut steady_allocations = 0;
    let mut steady_samples = 0;
    let mut baseline_figures = Vec::new();
    for _ in 0..3 + RECOVERY_FRAMES {
        let measured = measured_forced(&mut candidate, &mut scene)?;
        baseline_figures.push(measured.figures.clone());
        if measured.steady {
            // When: `measured.steady` holds, the frame refilled nothing and is a baseline sample.
            steady_allocations = steady_allocations.max(measured.allocations);
            steady_samples += 1;
        }
        if steady_samples == 3 {
            break;
        }
    }
    check(
        steady_samples == 3,
        &format!("the settled scene draws three steady frames: {}", baseline_figures.join("; ")),
    )?;
    type Arm = fn(&mut GpuRenderer);
    let arms: [(&str, Arm); 3] = [
        ("failed assembly", GpuRenderer::__fail_next_assembly),
        ("failed presentation", GpuRenderer::__fail_next_present),
        ("atlas retry", GpuRenderer::__change_glyph_atlas_during_next_assembly),
    ];
    for (name, arm) in arms {
        let before = candidate.retained_amounts().frame_scratch.bytes;
        arm(&mut candidate);
        candidate.invalidate_retained_frame();
        let _ = frame(&mut candidate, &mut scene);
        let after = candidate.retained_amounts().frame_scratch.bytes;
        check(
            after == before,
            &format!("{name}: the scratch kept its capacity ({before} -> {after})"),
        )?;
        // Recovered frames refill the caches the fault dropped; the scratch grows in none of them,
        // and the first steady one is held to the steady bound.
        let mut recovery = Vec::new();
        let mut recovered = None;
        for _ in 0..RECOVERY_FRAMES {
            let measured = measured_forced(&mut candidate, &mut scene)?;
            recovery.push(measured.figures.clone());
            check(
                measured.scratch_bytes <= before,
                &format!("{name}: no recovered frame grows the scratch: {}", recovery.join("; ")),
            )?;
            if measured.steady {
                // When: `measured.steady` holds, the caches are full again and the frame is measured.
                recovered = Some(measured);
                break;
            }
        }
        let recovered = recovered.ok_or_else(|| {
            format!("{name}: a recovered frame is steady within bound: {}", recovery.join("; "))
        })?;
        check(
            recovered.allocations <= steady_allocations && recovered.scratch_bytes == before,
            &format!(
                "{name}: the recovered frame reuses the scratch (steady {steady_allocations}, \
                 scratch {before}): {}",
                recovery.join("; ")
            ),
        )?;
        forced(&mut oracle, &mut oracle_scene)?;
        forced(&mut oracle, &mut oracle_scene)?;
        check_settled(&scene, candidate_settled, &format!("{name} candidate"))?;
        check_settled(&oracle_scene, oracle_settled, &format!("{name} oracle"))?;
        check(
            pixels(&mut candidate)? == pixels(&mut oracle)?,
            &format!("{name}: the next frame draws as a renderer with no failure"),
        )?;
    }
    Ok(())
}

/// T9: tofu in every cell, curly underlines on every row, twelve panes then two: the reported
/// frame scratch stays within its class envelope.
pub(super) fn scratch_caps_hold_after_unbounded_frames(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (960, 540), false)?;
    let ClassCoverage::UnchargedRetention { per_owner_bytes: cap } =
        ResourceClass::FrameScratch.coverage()
    else {
        return Err(String::from("FrameScratch records an uncharged envelope"));
    };
    let (width_px, height_px) = renderer.surface_size();
    let tiled = |count: u32| -> Vec<PixelRect> {
        let columns = count.min(4);
        let rows = count.div_ceil(columns);
        (0..count)
            .map(|index| PixelRect {
                x: i32::try_from((index % columns) * width_px / columns).unwrap_or(i32::MAX),
                y: i32::try_from((index / columns) * height_px / rows).unwrap_or(i32::MAX),
                w: width_px / columns,
                h: height_px / rows,
            })
            .collect()
    };
    for pane_count in [12u32, 2] {
        let mut scene = single_scene(&renderer, &["shell"]);
        scene.panes = tiled(pane_count)
            .into_iter()
            .enumerate()
            .map(|(index, rect)| {
                let mut grid = text_grid(&renderer, rect);
                for row in 0..grid.rows {
                    grid.goto(row, 0);
                    for _ in 0..grid.cols {
                        // A private-use character no face covers draws tofu, under a curly line.
                        grid.put_char_styled(
                            '\u{10fffd}',
                            Color::Default,
                            Color::Default,
                            CellFlags::UNDERLINE,
                            None,
                            UnderlineStyle::Curly,
                            None,
                        );
                    }
                }
                ScenePane { id: index as u64 + 1, rect, grid }
            })
            .collect();
        for _ in 0..3 {
            forced(&mut renderer, &mut scene)?;
        }
        let held = renderer.retained_amounts().frame_scratch.bytes;
        check(held <= cap, &format!("{pane_count} panes: {held} bytes within the {cap}-byte cap"))?;
    }
    Ok(())
}
