//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// `scene` split into two side-by-side panes, so a trim releases more than one pane's rows.
pub(super) fn split_scene(renderer: &GpuRenderer, titles: &[&str]) -> Scene {
    let mut scene = single_scene(renderer, titles);
    let whole = scene.panes[0].rect;
    let left = PixelRect { w: whole.w / 2, ..whole };
    let right = PixelRect {
        x: whole.x + i32::try_from(left.w).unwrap_or(i32::MAX),
        w: whole.w - left.w,
        ..whole
    };
    scene.panes = vec![
        ScenePane { id: 1, rect: left, grid: text_grid(renderer, left) },
        ScenePane { id: 2, rect: right, grid: text_grid(renderer, right) },
    ];
    scene
}

/// The parts a covered-window trim releases, as a renderer reports them.
pub(super) fn released_parts(renderer: &GpuRenderer) -> [sonicterm_types::ResourceAmount; 5] {
    let parts = renderer.retained_amounts();
    [
        parts.row_glyph_cache,
        parts.row_quad_cache,
        parts.row_ink,
        parts.frame_scratch,
        parts.chrome_cache,
    ]
}

/// A trim on the GPU presenter returns every released part to a new renderer's figure, keeps the
/// glyph atlas, lowers the vertex and upload storage, leaves a 1x1 frame texture marked trimmed, and
/// reports the frame texture and present buffers it gave back as request sizes.
pub(super) fn trim_releases_and_accounts(active: &ActiveEventLoop) -> Result<(), String> {
    // The fresh renderer is built first: the case's own binding shadows the helper's name.
    let (_fresh_window, fresh) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    settle_fallback(&mut renderer, &mut scene)?;
    let surface = renderer.surface_size();
    let before = renderer.retained_amounts();
    let report = renderer.trim_for_occlusion();
    let after = renderer.retained_amounts();
    check(!report.refused, "a usable device admits the trim")?;
    check(released_parts(&renderer) == released_parts(&fresh), "released parts read as new")?;
    check(after.glyph_atlas == before.glyph_atlas, "the glyph atlas is kept")?;
    check(after.image_atlas == fresh.retained_amounts().image_atlas, "no media: the placeholder")?;
    check(
        after.vertex_scratch.bytes < before.vertex_scratch.bytes,
        "vertex and upload storage fall",
    )?;
    check(renderer.frame_texture_extent() == (1, 1), "the frame texture is 1x1")?;
    check(renderer.__frame_texture_trimmed(), "the frame texture is marked trimmed")?;
    check(report.frame_texture_before == surface, "the report names the released texture")?;
    let texture_bytes = u64::from(surface.0) * u64::from(surface.1) * 4 - 4;
    check(
        report.gpu_released_requested_bytes >= texture_bytes
            && report.present_buffer_bytes_before > 0,
        &format!("the report counts the texture and buffers: {report:?}"),
    )
}

/// After a trim, one frame of the same renderer presents at once: the texture is back at the surface
/// size and the pixels equal the settled reference with the same fallback generation. A resize after
/// a trim installs the surface-sized texture itself, so the next present rebuilds nothing.
pub(super) fn trim_restores_without_a_resize(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let reference = pixels(&mut renderer)?;
    let _ = renderer.trim_for_occlusion();
    check(
        matches!(frame(&mut renderer, &mut scene), PresentOutcome::Presented),
        "the first frame after the trim presents",
    )?;
    check(renderer.frame_texture_extent() == renderer.surface_size(), "the texture is restored")?;
    check(!renderer.__frame_texture_trimmed(), "the mark is cleared")?;
    check_settled(&scene, settled, "restored frame")?;
    check(pixels(&mut renderer)? == reference, "the restored frame equals the reference")?;

    let _ = renderer.trim_for_occlusion();
    check(renderer.try_resize(400, 200), "the resize is accepted")?;
    check(!renderer.__frame_texture_trimmed(), "the resize cleared the mark")?;
    let rebuilds = renderer.__frame_texture_rebuilds();
    let mut resized = split_scene(&renderer, &["shell", "logs"]);
    present(&mut renderer, &mut resized)?;
    check(renderer.__frame_texture_rebuilds() == rebuilds, "the next present rebuilds nothing")
}

/// Under the software presenter a trim leaves the 1x1 texture alone and never marks it, releases
/// the CPU parts and any upload storage, keeps the software frame, and the next GDI frame equals the
/// settled reference with the same fallback generation.
pub(super) fn trim_on_the_software_presenter(active: &ActiveEventLoop) -> Result<(), String> {
    // The fresh renderer is built first: the case's own binding shadows the helper's name.
    let (_fresh_window, fresh) =
        renderer(active, SoftwareRenderMode::Force, &Theme::default(), (480, 240), true)?;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Force, &Theme::default(), (480, 240), true)?;
    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    let settled = settle_fallback(&mut renderer, &mut scene)?;
    let reference = pixels(&mut renderer)?;
    let rebuilds = renderer.__frame_texture_rebuilds();
    let before = renderer.retained_amounts();
    let _ = renderer.trim_for_occlusion();
    let after = renderer.retained_amounts();
    check(renderer.frame_texture_extent() == (1, 1), "the GDI texture stays 1x1")?;
    check(!renderer.__frame_texture_trimmed(), "the GDI texture is never marked")?;
    check(renderer.__frame_texture_rebuilds() == rebuilds, "no texture was built")?;
    check(released_parts(&renderer) == released_parts(&fresh), "released parts read as new")?;
    check(after.software_frame == before.software_frame, "the software frame is kept")?;
    check(after.vertex_scratch.bytes <= before.vertex_scratch.bytes, "upload storage is released")?;
    forced(&mut renderer, &mut scene)?;
    check_settled(&scene, settled, "GDI frame after the trim")?;
    check(pixels(&mut renderer)? == reference, "the next GDI frame equals the reference")
}

/// Contract: the graphics ledger follows a real renderer through its production hooks, not hand
/// calls: construction records the surface at its size times its drawables, the frame texture,
/// both atlases and three present buffers; a resize replaces the surface and the frame texture;
/// a covered-window trim moves the ledger by exactly what its report says it released; and the
/// drop releases every request, leaving created equal to released in every class.
pub(super) fn graphics_ledger_follows_create_resize_trim_and_drop(
    active: &ActiveEventLoop,
) -> Result<(), String> {
    use sonicterm_gpu::graphics_ledger::GraphicsClass;
    let (_window, mut renderer) =
        renderer(active, SoftwareRenderMode::Off, &Theme::default(), (480, 240), true)?;
    let ledger = renderer.__graphics_ledger_handle();
    let bgra_bytes =
        |size: (u32, u32), copies: u64| u64::from(size.0) * u64::from(size.1) * 4 * copies;

    let created = ledger.snapshot();
    let surface = created.class(GraphicsClass::Surface);
    let size = renderer.surface_size();
    check(
        surface.live_resources >= 2
            && surface.live_bytes == bgra_bytes(size, surface.live_resources),
        &format!("the surface is recorded at {size:?} times its drawables: {surface:?}"),
    )?;
    check(
        created.class(GraphicsClass::FrameTexture).live_bytes
            == bgra_bytes(renderer.frame_texture_extent(), 1),
        "the frame texture is recorded at its extent",
    )?;
    for class in [GraphicsClass::GlyphAtlas, GraphicsClass::ImageAtlas] {
        check(created.class(class).live_resources == 1, "each atlas texture is recorded")?;
    }
    check(
        created.class(GraphicsClass::PresentBuffers).live_resources == 3,
        "three present buffers",
    )?;

    check(renderer.try_resize(400, 200), "the resize is accepted")?;
    let resized = ledger.snapshot();
    let size = renderer.surface_size();
    let surface = resized.class(GraphicsClass::Surface);
    check(
        surface.live_bytes == bgra_bytes(size, surface.live_resources)
            && surface.released_resources
                > created.class(GraphicsClass::Surface).released_resources,
        &format!("the resize replaces the surface: {surface:?} for {size:?}"),
    )?;
    let frame = resized.class(GraphicsClass::FrameTexture);
    check(
        frame.live_bytes == bgra_bytes(renderer.frame_texture_extent(), 1)
            && frame.released_resources
                > created.class(GraphicsClass::FrameTexture).released_resources,
        "the resize replaces the frame texture",
    )?;

    let mut scene = split_scene(&renderer, &["shell", "logs"]);
    settle_fallback(&mut renderer, &mut scene)?;
    let before = ledger.snapshot();
    let report = renderer.trim_for_occlusion();
    let after = ledger.snapshot();
    check(!report.refused, "a usable device admits the trim")?;
    check(after.class(GraphicsClass::FrameTexture).live_bytes == 4, "the trimmed texture is 1x1")?;
    let trimmed = |snapshot: &sonicterm_gpu::graphics_ledger::GraphicsLedgerSnapshot| {
        snapshot.class(GraphicsClass::FrameTexture).live_bytes
            + snapshot.class(GraphicsClass::PresentBuffers).live_bytes
    };
    check(
        trimmed(&before) - trimmed(&after) == report.gpu_released_requested_bytes,
        &format!("the ledger moves by the trim report: {report:?}"),
    )?;

    drop(renderer);
    let dropped = ledger.snapshot();
    check(
        dropped.live_bytes() == 0 && dropped.live_resources() == 0,
        "the drop releases every request",
    )?;
    for class in GraphicsClass::ALL {
        let tally = dropped.class(class);
        check(
            tally.created_bytes == tally.released_bytes
                && tally.created_resources == tally.released_resources,
            &format!("{} balances after the drop: {tally:?}", class.as_str()),
        )?;
    }
    Ok(())
}
