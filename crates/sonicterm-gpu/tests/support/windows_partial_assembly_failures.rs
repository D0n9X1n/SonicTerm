//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// A one-row edit armed to fail, from a fresh baseline: the edited row's record before the edit,
/// the retained pixels before it, and the counters before the failing frame.
pub(super) fn failing_edit(
    renderer: &mut GpuRenderer,
    layout: &Layout,
) -> Result<(Scene, Option<PixelRect>, Vec<u8>, Counts), String> {
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    let record = renderer.__test_row_ink(PANE_ID, EDIT_ROW);
    let (pixels, _) = retained_pixels(renderer)?;
    write(scene.grid(), EDIT_ROW, 0, "fail");
    Ok((scene, record, pixels, counts(renderer)))
}

/// What a failed partial frame must leave: nothing presented or counted as presented, the plan
/// assembled as partial (some rows hashed, not all), no frame key, the edit's dirt kept, the edited
/// row's record and the retained pixels unchanged, and no presented damage.
pub(super) fn nothing_committed(
    renderer: &mut GpuRenderer,
    scene: &Scene,
    record: Option<PixelRect>,
    pixels: Option<&[u8]>,
    before: Counts,
    case: &str,
) -> Result<(), String> {
    let moved = delta(before, counts(renderer));
    check(
        moved.partial_frames == 0
            && moved.full_frames == 0
            && moved.row_cells_hashed > 0
            && moved.row_cells_hashed < scene.cells(),
        &format!("{case}: a partial plan was assembled and nothing presented: {moved:?}"),
    )?;
    check(!renderer.__test_has_frame_key(), &format!("{case}: the frame key is cleared"))?;
    check(
        scene.panes[0].grid.dirty_rows().any(|row| row == usize::from(EDIT_ROW)),
        &format!("{case}: no receipt cleared the edit's dirt"),
    )?;
    check(
        renderer.__test_row_ink(PANE_ID, EDIT_ROW) == record,
        &format!("{case}: the edited row's record is unchanged"),
    )?;
    check(renderer.__take_presented_damage().is_none(), &format!("{case}: nothing presented"))?;
    if let Some(pixels) = pixels {
        let (after, _) = retained_pixels(renderer)?;
        check(after == pixels, &format!("{case}: the retained frame was not drawn"))?;
    }
    Ok(())
}

/// The frame after a failure is a whole-surface `Full` first frame that equals a full repaint and
/// rewrites the edited row's record.
pub(super) fn retry_is_full(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    record: Option<PixelRect>,
    case: &str,
) -> Result<(), String> {
    let before = counts(renderer);
    let retried = present(renderer, scene)?;
    let moved = delta(before, counts(renderer));
    check(
        retried.first_frame && retried.damage == retried.surface,
        &format!("{case}: the retry repaints the surface: {retried:?}"),
    )?;
    check(
        moved.full_frames >= 1 && moved.partial_frames == 0,
        &format!("{case}: the retry is Full: {moved:?}"),
    )?;
    let (retried_pixels, _) = retained_pixels(renderer)?;
    renderer.invalidate_retained_frame();
    present(renderer, scene)?;
    let (full_pixels, _) = retained_pixels(renderer)?;
    check(retried_pixels == full_pixels, &format!("{case}: the retry equals a full repaint"))?;
    check(
        renderer.__test_row_ink(PANE_ID, EDIT_ROW) != record,
        &format!("{case}: the retry rewrites the edited row's record"),
    )
}

/// Tests 8a, 18 and 21: an acquisition Timeout or Occlusion after a partial plan was assembled
/// returns `SurfaceRetry` before the retained draw, commits no record, receipt or partial frame,
/// clears the key, and the retry is `Full`.
pub(super) fn acquisition_failure(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    for reason in [SurfaceRetryReason::Timeout, SurfaceRetryReason::Occluded] {
        let case = format!("acquisition {reason:?}");
        let (mut scene, record, pixels, before) = failing_edit(renderer, layout)?;
        renderer.__fail_next_surface_acquire(reason);
        let outcome = frame(renderer, &mut scene);
        check(
            matches!(outcome, PresentOutcome::SurfaceRetry(retried) if retried == reason),
            &format!("{case}: the frame is a surface retry: {outcome:?}"),
        )?;
        nothing_committed(renderer, &scene, record, Some(&pixels), before, &case)?;
        retry_is_full(renderer, &mut scene, record, &case)?;
    }
    Ok(())
}

/// Tests 9, 18 and 21: an atlas change during a partial assembly presents nothing, issues no
/// receipt, commits no record or partial frame, requests a redraw and clears the key, and the
/// retry is `Full`.
pub(super) fn atlas_change(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let (mut scene, record, pixels, before) = failing_edit(renderer, layout)?;
    renderer.__change_glyph_atlas_during_next_assembly();
    let outcome = frame(renderer, &mut scene);
    check(
        matches!(outcome, PresentOutcome::AtlasRetry),
        &format!("the frame is an atlas retry: {outcome:?}"),
    )?;
    check(
        delta(before, counts(renderer)).native_request_redraw >= 1,
        "the atlas retry requests a redraw",
    )?;
    nothing_committed(renderer, &scene, record, Some(&pixels), before, "atlas change")?;
    retry_is_full(renderer, &mut scene, record, "atlas change")
}

/// Tests 8b, 18 and 21: a submission that fails validation after the retained draw stops the
/// device; the frame is `RenderingUnavailable`, commits no record, receipt or partial frame and
/// clears the key. After recovery onto a new device, the first frame is `Full`.
pub(super) fn submission_failure(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (mut scene, record, _pixels, before) = failing_edit(renderer, layout)?;
    renderer.__fail_next_frame_submission();
    let outcome = frame(renderer, &mut scene);
    check(
        matches!(outcome, PresentOutcome::RenderingUnavailable(_)),
        &format!("the frame is unavailable: {outcome:?}"),
    )?;
    // The retained frame was drawn into before the failure, so its pixels are not compared.
    nothing_committed(renderer, &scene, record, None, before, "submission failure")?;
    recover(renderer, active)?;
    retry_is_full(renderer, &mut scene, record, "submission failure")
}

/// Rebind `renderer` onto a newly requested device, in production's order: request, run,
/// prepare, commit.
pub(super) fn recover(renderer: &mut GpuRenderer, active: &ActiveEventLoop) -> Result<(), String> {
    let request = renderer.recovery_request(active).map_err(|error| error.to_string())?;
    let recovered = request
        .run(|_generation| -> DeviceStateWaker { Arc::new(|| {}) })
        .map_err(|failure| failure.error().to_string())?;
    let (context, surface) = recovered.into_parts();
    let prepared = renderer
        .prepare_rebind(&context, Some(surface), SoftwareRenderMode::Off)
        .map_err(|error| error.to_string())?;
    renderer.commit_rebind(prepared).map_err(|error| error.to_string())?;
    check(renderer.device_accepts_gpu_work(), "the recovered device accepts work")
}
