//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// Test 6's cases at one background `opacity`, over ANSI row backgrounds when `ansi_rows`.
pub(super) fn parity_cases(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    opacity: f32,
    ansi_rows: bool,
) -> Result<(), String> {
    let scene = || {
        let mut scene = single(layout);
        scene.opacity = opacity;
        if ansi_rows {
            paint_ansi_rows(scene.grid(), layout.cols);
        }
        scene
    };
    let label = |case: &str| {
        format!("{case} at opacity {opacity}{}", if ansi_rows { " over ANSI rows" } else { "" })
    };
    let mut edit = scene();
    baseline(renderer, &mut edit)?;
    write(edit.grid(), EDIT_ROW, 0, "edit");
    narrow_matches_full(renderer, &mut edit, &label("one-row edit"), Expect::Partial)?;

    // A glyph two and a half rows tall attached to the row above, and one attached to the row
    // below reaching up into the edit row: each row's record reaches the damage.
    for (case, slot, top_row) in [
        ("tall glyph from the row above", EDIT_ROW - 1, f32::from(EDIT_ROW - 1)),
        ("tall glyph from the row below", EDIT_ROW + 1, f32::from(EDIT_ROW) - 0.5),
    ] {
        let rect_px = (
            layout.col_left(3),
            layout.grid_y + top_row * layout.cell_h,
            layout.cell_w,
            2.5 * layout.cell_h,
        );
        renderer.__inject_row_glyph(Some(InjectedRowGlyph {
            pane_id: PANE_ID,
            slot,
            rect_px,
            color: INJECTED_COLOR,
        }));
        let mut overhang = scene();
        baseline(renderer, &mut overhang)?;
        write(overhang.grid(), EDIT_ROW, 0, "xyz");
        narrow_matches_full(renderer, &mut overhang, &label(case), Expect::Partial)?;
    }
    renderer.__inject_row_glyph(None);

    let mut marks = scene();
    baseline(renderer, &mut marks)?;
    write(marks.grid(), EDIT_ROW, 0, "e\u{301}a\u{308}o\u{302}");
    narrow_matches_full(renderer, &mut marks, &label("combining marks"), Expect::Partial)?;

    let mut wide = scene();
    baseline(renderer, &mut wide)?;
    write(wide.grid(), EDIT_ROW, layout.cols - 4, "中😀");
    narrow_matches_full(
        renderer,
        &mut wide,
        &label("wide CJK and emoji at the edge"),
        Expect::Partial,
    )?;

    // Every underline style on the edit row, under an unchanged underlined neighbour row.
    let styles = [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ];
    let mut underlined = scene();
    let styled = |grid: &mut Grid, row: u16| {
        grid.goto(row, 0);
        for (style, character) in styles.into_iter().zip("uline".chars()) {
            grid.put_char_styled(
                character,
                Color::Default,
                Color::Default,
                CellFlags::UNDERLINE,
                None,
                style,
                None,
            );
        }
    };
    styled(underlined.grid(), EDIT_ROW - 1);
    underlined.grid().goto(EDIT_ROW, 0);
    baseline(renderer, &mut underlined)?;
    styled(underlined.grid(), EDIT_ROW);
    narrow_matches_full(
        renderer,
        &mut underlined,
        &label("every underline style"),
        Expect::Partial,
    )?;

    // An unchanged image anchored on the row after the edit, three rows tall: the edit damages its
    // own row padded by one row each way, so the image's top row lies inside the damage and its
    // lower two rows outside it.
    let mut image = scene();
    let (image_row, image_col) = (EDIT_ROW + 1, 6);
    let (width, height) = ((3.0 * layout.cell_w) as u32, (3.0 * layout.cell_h) as u32);
    image.panes[0].images.push(InlineImage {
        id: 1,
        row: image_row,
        col: image_col,
        width,
        height,
        bgra: Arc::from(vec![200u8; (width * height * 4) as usize]),
    });
    let image_rect = outward((
        layout.col_left(image_col),
        layout.row_top(image_row),
        width as f32,
        height as f32,
    ));
    let strip_bottom = layout.strip_rect(EDIT_ROW).ok_or("the edit row's strip")?.bottom();
    check(
        image_rect.y < strip_bottom && strip_bottom < image_rect.bottom(),
        &format!("the image {image_rect:?} crosses the strip's edge {strip_bottom}"),
    )?;
    baseline(renderer, &mut image)?;
    write(image.grid(), EDIT_ROW, 0, "edit");
    let (damage, _, _) = narrow_matches_full(
        renderer,
        &mut image,
        &label("inline image crossing the damage"),
        Expect::Partial,
    )?;
    let inside = image_rect.intersect(damage.damage);
    check(
        inside.is_some_and(|inside| inside.h > 0 && inside.h < image_rect.h),
        &format!(
            "part of the image {image_rect:?} lies inside the damage {:?} and part outside: \
             {inside:?}",
            damage.damage
        ),
    )?;

    let mut select = scene();
    let selected_row = select.grid().scrollback_len() as u64 + u64::from(EDIT_ROW - 1);
    let mut selection = Selection::new(selected_row, 0);
    selection.extend(selected_row, 3);
    select.selection = Some(selection);
    baseline(renderer, &mut select)?;
    if let Some(selection) = select.selection.as_mut() {
        selection.extend(selected_row, 9);
    }
    narrow_matches_full(renderer, &mut select, &label("selection change"), Expect::Partial)?;

    let mut cursor = scene();
    cursor.grid().goto(CURSOR_ROW, CURSOR_COL);
    baseline(renderer, &mut cursor)?;
    for visible in [true, false] {
        cursor.cursor_visible = visible;
        narrow_matches_full(renderer, &mut cursor, &label("cursor toggle"), Expect::Partial)?;
    }
    Ok(())
}

/// Test 7: a one-row edit on a pane of at least 20 rows presents one partial frame, no full frame,
/// and hashes exactly the rows the planner emits, computed from the captured geometry and the
/// baseline records by [`edit_rows`]: the edited row, every row whose rounded strip meets the
/// edit's strip, and every row whose record meets it. How many rows that is depends on the pitch
/// (seven at 19.2 px, five at 20 px). The frame uploads fewer bytes than the same state planned
/// `Full`.
pub(super) fn emission_and_upload_shrink(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let mut scene = single(layout);
    baseline(renderer, &mut scene)?;
    let expected = edit_rows(layout, EDIT_ROW, |row| renderer.__test_row_ink(PANE_ID, row));
    check(
        expected.contains(&EDIT_ROW) && expected.len() < usize::from(layout.rows),
        &format!("the edit reaches some rows, not all {}: {expected:?}", layout.rows),
    )?;
    write(scene.grid(), EDIT_ROW, 0, "edit");
    let before = counts(renderer);
    present(renderer, &mut scene)?;
    let partial = delta(before, counts(renderer));
    let cols = u64::from(layout.cols);
    let expected_cells = expected.len() as u64 * cols;
    check(
        partial.partial_frames == 1
            && partial.full_frames == 0
            && partial.row_cells_hashed == expected_cells,
        &format!(
            "one partial frame hashing rows {expected:?}, {expected_cells} cells: {partial:?}"
        ),
    )?;
    renderer.invalidate_retained_frame();
    let before = counts(renderer);
    present(renderer, &mut scene)?;
    let full = delta(before, counts(renderer));
    check(
        full.full_frames == 1 && full.row_cells_hashed == layout.cells(),
        &format!("the same state planned Full hashes every row: {full:?}"),
    )?;
    check(
        partial.uploaded_bytes < full.uploaded_bytes,
        &format!("the partial frame uploads less: {partial:?} vs {full:?}"),
    )
}

/// Tests 16 and 17, pixel parts: a row whose glyph overhangs into the damage from a neighbouring
/// pane, and a row whose tall glyph reaches the damage from five rows above, are emitted through
/// their ink records though their padded strips miss it, and the frame equals a full repaint.
pub(super) fn overhanging_records(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    // Two side-by-side panes; pane 1's edit-row glyph runs four pixels past its right edge.
    let half_w = layout.pane.w / 2;
    let half_cols = ((half_w as f32 / layout.cell_w).floor() as u16).max(1);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell"));
    let mut split = Scene {
        opacity: OPACITY,
        receipts: Vec::new(),
        font_generation: None,
        panes: vec![
            ScenePane {
                id: PANE_ID,
                rect: PixelRect { w: half_w, ..layout.pane },
                grid: dense_grid(half_cols, layout.rows),
                images: Vec::new(),
                active: true,
            },
            ScenePane {
                id: NEIGHBOUR_ID,
                rect: PixelRect { x: half_w as i32, w: layout.pane.w - half_w, ..layout.pane },
                grid: dense_grid(half_cols, layout.rows),
                images: Vec::new(),
                active: false,
            },
        ],
        cursor_visible: false,
        selection: None,
        tabs,
    };
    renderer.__inject_row_glyph(Some(InjectedRowGlyph {
        pane_id: PANE_ID,
        slot: EDIT_ROW,
        rect_px: (half_w as f32 - 4.0, layout.row_top(EDIT_ROW), 12.0, layout.cell_h),
        color: INJECTED_COLOR,
    }));
    baseline(renderer, &mut split)?;
    let record =
        renderer.__test_row_ink(PANE_ID, EDIT_ROW).ok_or("the overhanging row's record")?;
    check(record.right() > half_w as i32, &format!("the record overhangs: {record:?}"))?;
    write(&mut split.panes[1].grid, EDIT_ROW, 0, "edit");
    narrow_matches_full(renderer, &mut split, "neighbouring pane overhang", Expect::Partial)?;

    // A glyph four and a half rows tall attached to the row five above the edit.
    let tall_slot = EDIT_ROW - 5;
    renderer.__inject_row_glyph(Some(InjectedRowGlyph {
        pane_id: PANE_ID,
        slot: tall_slot,
        rect_px: (
            layout.col_left(3),
            layout.row_top(tall_slot),
            layout.cell_w,
            4.5 * layout.cell_h,
        ),
        color: INJECTED_COLOR,
    }));
    let mut vertical = single(layout);
    baseline(renderer, &mut vertical)?;
    write(vertical.grid(), EDIT_ROW, 0, "edit");
    let (damage, _, _) =
        narrow_matches_full(renderer, &mut vertical, "tall glyph five rows up", Expect::Partial)?;
    let record = renderer.__test_row_ink(PANE_ID, tall_slot).ok_or("the tall row's record")?;
    let strip = layout.strip_rect(tall_slot);
    check(
        record.intersect(damage.damage).is_some()
            && strip.is_none_or(|strip| strip.intersect(damage.damage).is_none()),
        &format!(
            "the tall row's record {record:?} reaches the damage {:?} its strip {strip:?} misses",
            damage.damage
        ),
    )?;
    renderer.__inject_row_glyph(None);
    Ok(())
}

/// Test 19, pixel part: a glyph four and a half rows tall under the hidden cursor. Showing the
/// cursor recolors it; widening the damage by its bounds reaches a row the partial plan did not
/// emit, so the frame is reassembled `Full` in the same frame, counted as one fallback and one
/// full frame, and equals a full repaint. Hiding it again emits the rows the previous recolor
/// reached, so no fallback is needed. Both frames go through the releasing call: the fallback's
/// receipt acknowledges every row (`AckRows::All`), the partial frame's only the rows it drew.
///
/// The geometry is checked first: the cursor cell covers at least the recolorer's 20% of the
/// glyph, and after the baseline [`skipped_reached_row`] finds, from the rounded strips and the
/// committed records, a dense row whose strip and record both miss the cursor's strip while its
/// record meets the glyph, so only the post-assembly check can catch it.
pub(super) fn post_assembly_fallback(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    let glyph = layout.cursor_glyph(CURSOR_ROW, CURSOR_COL, 4.5);
    let covered = layout.cursor_coverage(CURSOR_ROW, CURSOR_COL, glyph);
    check(covered >= 0.20, &format!("the cursor covers {covered} of the glyph, at least 20%"))?;
    renderer.__inject_test_glyph(Some((glyph, INJECTED_COLOR)));
    let mut scene = single(layout);
    scene.grid().goto(CURSOR_ROW, CURSOR_COL);
    baseline(renderer, &mut scene)?;
    let reached = skipped_reached_row(layout, CURSOR_ROW, outward(glyph), |row| {
        renderer.__test_row_ink(PANE_ID, row)
    });
    check(
        reached.is_some(),
        &format!(
            "a row above {CURSOR_ROW} misses the cursor strip {:?} in strip and record, and its \
             record meets the glyph {:?}",
            layout.strip_rect(CURSOR_ROW),
            outward(glyph)
        ),
    )?;
    scene.cursor_visible = true;
    let (_, _, fallback_receipts) = narrow_matches_full(
        renderer,
        &mut scene,
        "recolor reaching a skipped row",
        Expect::Fallback,
    )?;
    check(
        !fallback_receipts.is_empty()
            && fallback_receipts.iter().all(|receipt| receipt.rows == AckRows::All),
        &format!("the reassembled Full frame acknowledges every row: {fallback_receipts:?}"),
    )?;
    scene.cursor_visible = false;
    let (_, _, partial_receipts) = narrow_matches_full(
        renderer,
        &mut scene,
        "previous recolor rows emitted",
        Expect::Partial,
    )?;
    check(
        !partial_receipts.is_empty()
            && partial_receipts.iter().all(|receipt| matches!(receipt.rows, AckRows::Rows(_))),
        &format!("the partial frame acknowledges only its drawn rows: {partial_receipts:?}"),
    )?;
    renderer.__inject_test_glyph(None);
    Ok(())
}
