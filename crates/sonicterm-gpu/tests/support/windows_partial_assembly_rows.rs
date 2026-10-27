//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// Test 24 and test 18's closed pane: a 2,000-row pane commits one record per row; shrunk to 40
/// rows its records fall to 40 and the reported bytes fall with the table, inside the class's
/// envelope; closing the pane drops its records.
pub(super) fn row_ink_grows_and_shrinks(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let cols = layout.cols.min(200);
    let mut scene = single(layout);
    scene.panes[0].grid = dense_grid(cols, 2_000);
    present(renderer, &mut scene)?;
    let grown = renderer.retained_amounts().row_ink;
    check(grown.items == 2_000, &format!("one record per row of 2,000: {grown:?}"))?;
    let ClassCoverage::UnchargedRetention { per_owner_bytes } = ResourceClass::RowInk.coverage()
    else {
        return Err(String::from("RowInk records its uncharged envelope"));
    };
    check(grown.bytes <= per_owner_bytes, &format!("{grown:?} within {per_owner_bytes}"))?;
    scene.panes[0].grid = dense_grid(cols, 40);
    present(renderer, &mut scene)?;
    let shrunk = renderer.retained_amounts().row_ink;
    check(
        shrunk.items == 40 && shrunk.bytes * 10 < grown.bytes,
        &format!("the table releases its capacity: {grown:?} -> {shrunk:?}"),
    )?;
    renderer.invalidate_pane_caches(PANE_ID);
    check(
        renderer.retained_amounts().row_ink.items == 0
            && renderer.__test_row_ink(PANE_ID, 0).is_none(),
        "closing the pane drops its records",
    )
}

/// Test 11 at runtime: a degraded renderer presents through the GDI presenter, whose debug
/// assertion rejects a partial frame; a one-row edit there is a `Full` software frame with surface
/// damage and never partial.
pub(super) fn degraded_is_never_partial(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    renderer.set_software_render_degrade(true);
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    write(scene.grid(), EDIT_ROW, 0, "edit");
    let before = counts(renderer);
    let damage = present(renderer, &mut scene)?;
    let moved = delta(before, counts(renderer));
    renderer.set_software_render_degrade(false);
    check(
        moved.partial_frames == 0 && moved.full_frames == 1 && moved.software_frames == 1,
        &format!("a degraded edit is one Full software frame: {moved:?}"),
    )?;
    check(damage.damage == damage.surface, &format!("degraded damage is the surface: {damage:?}"))
}

/// A grid holding only unique, non-blank rows of Rec Mono fixture ASCII and block glyphs, so a cold
/// renderer's frame misses on every emitted row and no row can hit another's entry.
pub(super) fn unique_grid(cols: u16, rows: u16) -> Grid {
    let mut grid = Grid::new(cols, rows);
    for row in 0..rows {
        write(&mut grid, row, 0, &unique_row(u32::from(row), cols));
    }
    grid
}

/// The text of unique row `index`, at most `cols` characters: its index, then ASCII and blocks.
pub(super) fn unique_row(index: u32, cols: u16) -> String {
    format!("{index:04} ab=>cd █▌│─ {index:x} MW").chars().take(usize::from(cols)).collect()
}

/// One active pane over the whole surface holding `unique_grid`, with a hidden cursor.
pub(super) fn unique_scene(layout: &Layout) -> Scene {
    let mut scene = single(layout);
    scene.panes[0].grid = unique_grid(layout.cols, layout.rows);
    scene
}

/// Scroll the scene's grid one line and write a new unique row at the bottom.
pub(super) fn scroll_one_line(scene: &mut Scene, rows: u16, cols: u16, index: u32) {
    let grid = scene.grid();
    grid.goto(rows - 1, 0);
    grid.carriage_return();
    grid.linefeed();
    write(grid, rows - 1, 0, &unique_row(index, cols));
}

/// The row-cache hits and misses `renderer` has counted.
pub(super) fn row_cache_counts(renderer: &GpuRenderer) -> (u64, u64) {
    let stats = renderer.frame_stats();
    (stats.row_cache_hits, stats.row_cache_misses)
}

/// Draw `scene` once and require `Presented`, the presenter `degraded` names (a software frame
/// counted under GDI), and return the row-cache hits and misses the frame added.
pub(super) fn presented_frame(
    renderer: &mut GpuRenderer,
    scene: &mut Scene,
    degraded: bool,
    case: &str,
) -> Result<(u64, u64), String> {
    let (hits, misses) = row_cache_counts(renderer);
    let before = counts(renderer);
    let outcome = frame(renderer, scene);
    check(
        matches!(outcome, PresentOutcome::Presented),
        &format!("{case}: presented: {outcome:?}"),
    )?;
    let moved = delta(before, counts(renderer));
    check(
        renderer.is_software_render_degraded() == degraded
            && (!degraded || moved.software_frames >= 1),
        &format!("{case}: the expected presenter drew the frame: {moved:?}"),
    )?;
    let (hits_after, misses_after) = row_cache_counts(renderer);
    Ok((hits_after - hits, misses_after - misses))
}

/// The cold oracle: `oracle` first presents `scene` itself, so its atlas and font preparation are
/// warm, then drops every test pane's cached rows and the retained frame, which leave the atlas
/// alone, and presents `scene` again. That frame must hit no row and miss every emitted row.
pub(super) fn cold_oracle_frame(
    oracle: &mut GpuRenderer,
    scene: &mut Scene,
    degraded: bool,
    case: &str,
) -> Result<(), String> {
    presented_frame(oracle, scene, degraded, case)?;
    for pane in &scene.panes {
        oracle.invalidate_pane_caches(pane.id);
    }
    oracle.invalidate_retained_frame();
    let rows: u64 = scene.panes.iter().map(|pane| u64::from(pane.grid.rows)).sum();
    let (hits, misses) = presented_frame(oracle, scene, degraded, case)?;
    check(
        hits == 0 && misses == rows,
        &format!("{case}: the oracle's row cache is cold: {hits} hits, {misses} misses of {rows}"),
    )
}

/// T8a and T8b at scales 1, 1.25, 1.5 and 2 on two wgpu renderers built alike: a warm candidate
/// scrolls one line (its moved rows hit) and must equal a cold oracle's Full repaint; then a
/// one-cell edit and a cursor toggle present one `Partial` frame that also equals the oracle.
pub(super) fn warm_rows_match_a_cold_renderer(
    _renderer: &mut GpuRenderer,
    _layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    for scale in [1.0_f32, 1.25, 1.5, 2.0] {
        let case = format!("wgpu scale {scale}");
        let (window, mut candidate) = wgpu_renderer(active)?;
        let (_oracle_window, mut oracle) = wgpu_renderer(active)?;
        candidate.set_scale_factor(scale);
        oracle.set_scale_factor(scale);
        let layout = layout(&candidate, &window);
        let (mut warm_scene, mut cold_scene) = (unique_scene(&layout), unique_scene(&layout));
        baseline(&mut candidate, &mut warm_scene)?;
        let generation = warm_scene.font_generation;
        scroll_one_line(&mut warm_scene, layout.rows, layout.cols, 9_000);
        scroll_one_line(&mut cold_scene, layout.rows, layout.cols, 9_000);
        let before = counts(&candidate);
        let (hits, misses) = presented_frame(&mut candidate, &mut warm_scene, false, &case)?;
        let moved = delta(before, counts(&candidate));
        check(
            warm_scene.font_generation == generation,
            &format!("{case}: the font generation held across the scroll"),
        )?;
        check(
            moved.full_frames == 1 && moved.partial_frames == 0,
            &format!("{case}: the scroll is one Full assembly: {moved:?}"),
        )?;
        check(
            (hits, misses) == (u64::from(layout.rows) - 1, 1),
            &format!("{case}: every moved row hits and only the new row misses: {hits}, {misses}"),
        )?;
        let (warm, width) = retained_pixels(&mut candidate)?;
        cold_oracle_frame(&mut oracle, &mut cold_scene, false, &case)?;
        let (cold, _) = retained_pixels(&mut oracle)?;
        check(
            first_difference(&warm, &cold, width).is_none(),
            &format!(
                "{case}: scrolled warm frame differs from the cold one at {:?}",
                first_difference(&warm, &cold, width)
            ),
        )?;

        for scene in [&mut warm_scene, &mut cold_scene] {
            write(scene.grid(), EDIT_ROW, 3, "Z");
            scene.cursor_visible = true;
        }
        let expected = expected_partial_rows(&layout, &warm_scene, &candidate);
        let (hits_before, misses_before) = row_cache_counts(&candidate);
        let before = counts(&candidate);
        candidate.__enable_emitted_rows();
        let narrow = present(&mut candidate, &mut warm_scene)?;
        let emitted = candidate.__take_emitted_rows();
        check(
            emitted == vec![(PANE_ID, expected.clone())],
            &format!("{case}: the frame emits exactly the rows {expected:?}: {emitted:?}"),
        )?;
        let moved = delta(before, counts(&candidate));
        let (hits_after, misses_after) = row_cache_counts(&candidate);
        let expected_rows = expected.len() as u64;
        check(
            moved.partial_frames == 1
                && moved.full_frames == 0
                && moved.partial_fallbacks == 0
                && !narrow.first_frame
                && narrow.is_narrow(),
            &format!("{case}: the edit presents one Partial frame: {moved:?} {narrow:?}"),
        )?;
        check(
            moved.row_cells_hashed == expected_rows * u64::from(layout.cols),
            &format!(
                "{case}: the frame emits exactly the rows {expected:?}: hashed {} cells",
                moved.row_cells_hashed
            ),
        )?;
        check(
            (hits_after - hits_before, misses_after - misses_before) == (expected_rows - 1, 1),
            &format!(
                "{case}: only the edited row misses among {expected:?}: {} hits, {} misses",
                hits_after - hits_before,
                misses_after - misses_before
            ),
        )?;
        let (warm, width) = retained_pixels(&mut candidate)?;
        cold_oracle_frame(&mut oracle, &mut cold_scene, false, &case)?;
        let (cold, _) = retained_pixels(&mut oracle)?;
        check(
            first_difference(&warm, &cold, width).is_none(),
            &format!("{case}: the Partial frame differs from the cold Full repaint"),
        )?;
    }
    Ok(())
}

/// The rows a `Partial` frame of `scene` must emit, from the geometry and the grid's actual dirt:
/// the bounding union of every dirty row's ink-padded strip and the cursor row's strip (the cell
/// where the block cursor recolors), then every row whose strip or committed ink record meets
/// it. `Grid::goto` dirties the row the cursor leaves even when it is hidden, so the dirt is read
/// from the grid rather than assumed.
pub(super) fn expected_partial_rows(
    layout: &Layout,
    scene: &Scene,
    renderer: &GpuRenderer,
) -> Vec<u16> {
    let grid = &scene.panes[0].grid;
    let mut reach: Vec<u16> = grid.dirty_rows().map(|row| row as u16).collect();
    if scene.cursor_visible {
        reach.push(grid.cursor.row);
    }
    let damage = reach
        .iter()
        .filter_map(|row| layout.strip_rect(*row))
        .reduce(|union, rect| union.union(rect));
    let Some(damage) = damage else {
        // When: nothing is dirty, a Partial frame emits no row.
        return Vec::new();
    };
    let meets = |rect: Option<PixelRect>| rect.is_some_and(|rect| rect.intersect(damage).is_some());
    (0..layout.rows)
        .filter(|row| {
            reach.contains(row)
                || meets(layout.strip_rect(*row))
                || meets(renderer.__test_row_ink(PANE_ID, *row))
        })
        .collect()
}

/// Every BGRA pixel of a GDI renderer's software frame, each read through the test hook and
/// required to exist. The hook and the GDI presenter exist only on Windows.
#[cfg(not(target_os = "windows"))]
pub(super) fn gdi_pixels(_renderer: &GpuRenderer) -> Result<Vec<[u8; 4]>, String> {
    Err(String::from("the GDI presenter exists only on Windows"))
}

/// Every BGRA pixel of a GDI renderer's software frame, each read through the test hook and
/// required to exist.
#[cfg(target_os = "windows")]
pub(super) fn gdi_pixels(renderer: &GpuRenderer) -> Result<Vec<[u8; 4]>, String> {
    let (width, height) = renderer.surface_size();
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for pixel_y in 0..height {
        for pixel_x in 0..width {
            pixels.push(
                renderer
                    .__test_software_frame_pixel_bgra(pixel_x, pixel_y)
                    .ok_or(format!("GDI pixel ({pixel_x}, {pixel_y}) is readable"))?,
            );
        }
    }
    Ok(pixels)
}

/// Pixels that differ from the frame's most common colour, its background.
pub(super) fn foreground_pixels(pixels: &[[u8; 4]]) -> usize {
    let mut tally = std::collections::HashMap::new();
    for pixel in pixels {
        *tally.entry(*pixel).or_insert(0usize) += 1;
    }
    let background = tally.into_iter().max_by_key(|(_, count)| *count).map(|(pixel, _)| pixel);
    pixels.iter().filter(|pixel| Some(**pixel) != background).count()
}

/// T8c at scales 1, 1.25, 1.5 and 2: two renderers forced onto the GDI presenter, one warm (its
/// scrolled rows hit) and one cold, compose byte-identical software frames with visible text.
pub(super) fn gdi_warm_and_cold_frames_match(
    _renderer: &mut GpuRenderer,
    _layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    for scale in [1.0_f32, 1.25, 1.5, 2.0] {
        let case = format!("GDI scale {scale}");
        let (window, mut candidate) = renderer_in_mode(active, SoftwareRenderMode::Force)?;
        let (_oracle_window, mut oracle) = renderer_in_mode(active, SoftwareRenderMode::Force)?;
        candidate.set_scale_factor(scale);
        oracle.set_scale_factor(scale);
        let layout = layout(&candidate, &window);
        let (mut warm_scene, mut cold_scene) = (unique_scene(&layout), unique_scene(&layout));
        candidate.invalidate_retained_frame();
        presented_frame(&mut candidate, &mut warm_scene, true, &case)?;
        scroll_one_line(&mut warm_scene, layout.rows, layout.cols, 9_100);
        scroll_one_line(&mut cold_scene, layout.rows, layout.cols, 9_100);
        let (hits, _) = presented_frame(&mut candidate, &mut warm_scene, true, &case)?;
        check(hits > 0, &format!("{case}: the warm GDI renderer replays moved rows"))?;
        let warm = gdi_pixels(&candidate)?;
        cold_oracle_frame(&mut oracle, &mut cold_scene, true, &case)?;
        let cold = gdi_pixels(&oracle)?;
        check(
            foreground_pixels(&warm) > 100 && foreground_pixels(&cold) > 100,
            &format!("{case}: both GDI frames draw text"),
        )?;
        check(warm == cold, &format!("{case}: warm and cold GDI frames differ"))?;
    }
    Ok(())
}

/// T11's direct `Err` exits, each apart from an ordinary failed outcome: an `Err` from assembly
/// and an `Err` from the presenter call each return `Failed` with no receipt, discard the
/// staged glyph-row keys, keep the committed ones and the retained pixels, and the next
/// presented frame commits only its own keys and equals a full repaint.
pub(super) fn error_exits_discard_staged_keys(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    for (case, assembly) in [("assembly Err", true), ("presenter Err", false)] {
        let (mut scene, record, pixels, before) = failing_edit(renderer, layout)?;
        let committed = renderer.__test_glyph_slot_keys(PANE_ID, EDIT_ROW);
        if assembly {
            renderer.__fail_next_assembly();
        } else {
            // When: the presenter seam is armed instead, the `Err` comes from presentation.
            renderer.__fail_next_present();
        }
        let (lookups_before_hits, lookups_before_misses) = row_cache_counts(renderer);
        let (outcome, raw_receipts) = frame_with_receipts(renderer, &mut scene);
        check(
            matches!(outcome, PresentOutcome::Failed(_)),
            &format!("{case}: Failed: {outcome:?}"),
        )?;
        check(
            raw_receipts.is_empty(),
            &format!("{case}: the releasing call returned no receipt: {raw_receipts:?}"),
        )?;
        let (lookups_after_hits, lookups_after_misses) = row_cache_counts(renderer);
        check(
            lookups_after_hits + lookups_after_misses > lookups_before_hits + lookups_before_misses,
            &format!(
                "{case}: row-cache lookups ran before the fault, so there was a stage to discard"
            ),
        )?;
        let moved = delta(before, counts(renderer));
        check(
            moved.partial_frames == 0 && moved.full_frames == 0,
            &format!("{case}: nothing presented: {moved:?}"),
        )?;
        check(
            scene.panes[0].grid.dirty_rows().any(|row| row == usize::from(EDIT_ROW)),
            &format!("{case}: no receipt cleared the edit's dirt"),
        )?;
        let keys = renderer.__test_glyph_slot_keys(PANE_ID, EDIT_ROW);
        check(
            keys.map(|(_, staged)| staged) == Some(0)
                && keys.map(|(kept, _)| kept) == committed.map(|(kept, _)| kept),
            &format!("{case}: staged keys discarded, committed kept: {committed:?} -> {keys:?}"),
        )?;
        check(
            renderer.__test_row_ink(PANE_ID, EDIT_ROW) == record,
            &format!("{case}: the edited row's record is unchanged"),
        )?;
        let (after, _) = retained_pixels(renderer)?;
        check(after == pixels, &format!("{case}: the retained frame was not drawn"))?;
        present(renderer, &mut scene)?;
        let next = renderer.__test_glyph_slot_keys(PANE_ID, EDIT_ROW);
        check(
            next.is_some_and(|(kept, _)| Some(kept) != committed.map(|(old, _)| old)),
            &format!("{case}: the next presented frame commits its own key: {next:?}"),
        )?;
        let (next_pixels, _) = retained_pixels(renderer)?;
        renderer.invalidate_retained_frame();
        present(renderer, &mut scene)?;
        let (full, _) = retained_pixels(renderer)?;
        check(next_pixels == full, &format!("{case}: the next frame equals a full repaint"))?;
    }
    Ok(())
}
