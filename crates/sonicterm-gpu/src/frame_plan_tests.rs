use super::*;

fn facts(degraded: bool) -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 160, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [2.0; 4],
        vertical_ink_pad: 0.0,
        scrollbar_mode: ScrollbarMode::Auto,
        degraded,
        tab_bar_top: Some(140.0),
        scale: 1.0,
        previous_recolor: RecolorRecord::default(),
    }
}

/// Preview appearance, movement, replacement, and dismissal repaint covered pixels in both presenters.
#[test]
fn link_preview_changes_repaint_full_surface() {
    for degraded in [false, true] {
        let baseline = FramePlan::build(facts(degraded), [pane(7, 1)], None);
        let mut shown_facts = facts(degraded);
        shown_facts.window.renderer_hash = 11;
        shown_facts.window.overlay_active = true;
        let shown = FramePlan::build(shown_facts.clone(), [pane(7, 1)], Some(&baseline.key));
        assert_eq!(shown.mode, RenderMode::Full);
        assert_eq!(shown.damage, baseline.damage);
        let same = FramePlan::build(shown_facts.clone(), [pane(7, 1)], Some(&shown.key));
        assert!(same.unchanged);
        shown_facts.window.renderer_hash = 12;
        let moved = FramePlan::build(shown_facts, [pane(7, 1)], Some(&shown.key));
        assert_eq!(moved.mode, RenderMode::Full);
        assert_eq!(moved.damage, baseline.damage);
        let hidden = FramePlan::build(facts(degraded), [pane(7, 1)], Some(&moved.key));
        assert_eq!(hidden.mode, RenderMode::Full);
        assert_eq!(hidden.damage, baseline.damage);
    }
}

/// Hover-only transitions repaint both old and new rows without invalidating unrelated window pixels.
#[test]
fn hover_only_damage_stays_within_its_pane_rows() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None);
    let mut shown = facts(false);
    shown.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
    let first = FramePlan::build(shown.clone(), [pane(7, 1)], Some(&baseline.key));
    assert_eq!(first.damage, PixelRect { x: 0, y: 22, w: 100, h: 20 });
    shown.window.hovered_url_cells = HoveredUrlCells::single(7, 2, 1, 5, true);
    let moved = FramePlan::build(shown, [pane(7, 1)], Some(&first.key));
    assert_eq!(moved.damage, PixelRect { x: 0, y: 22, w: 100, h: 40 });
    let cleared = FramePlan::build(facts(false), [pane(7, 1)], Some(&moved.key));
    assert_eq!(cleared.damage, PixelRect { x: 0, y: 42, w: 100, h: 20 });
}

/// Modifier-only changes repaint stationary hover ink in GPU and degraded paths without changing terminal content.
#[test]
fn stationary_hover_modifier_changes_invalidate_presented_ink() {
    for degraded in [false, true] {
        let mut state = facts(degraded);
        state.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
        let inactive = FramePlan::build(state.clone(), [pane(7, 1)], None);
        state.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, true);
        // Records seeded from the presented frame let the hardware plan narrow to the hover row.
        let recorded = seeded(&state, &inactive, pane(7, 1));
        let narrowed = if degraded { RenderMode::Full } else { RenderMode::Partial };
        let active = FramePlan::build(state.clone(), [recorded.clone()], Some(&inactive.key));
        assert!(!active.unchanged);
        assert_eq!(active.mode, narrowed);
        let expected =
            if degraded { inactive.damage } else { PixelRect { x: 0, y: 22, w: 100, h: 20 } };
        assert_eq!(active.damage, expected);
        let stable = FramePlan::build(state.clone(), [pane(7, 1)], Some(&active.key));
        assert_eq!(stable.mode, RenderMode::Noop);
        state.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
        let released = FramePlan::build(state, [recorded], Some(&active.key));
        assert!(!released.unchanged);
        assert_eq!(released.mode, narrowed);
        assert_eq!(released.damage, expected);
    }
}

/// Hover row damage includes ink overhang but stays out of a neighboring pane; degradation still repaints fully.
#[test]
fn hover_damage_preserves_pane_ink_and_degraded_rules() {
    for degraded in [false, true] {
        let mut base_facts = facts(degraded);
        base_facts.vertical_ink_pad = 3.0;
        let mut neighbor = pane(9, 1);
        neighbor.rect.x = 100;
        neighbor.is_active = false;
        let baseline = FramePlan::build(base_facts.clone(), [pane(7, 1), neighbor.clone()], None);
        base_facts.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
        let shown = FramePlan::build(
            base_facts.clone(),
            [pane(7, 1), neighbor.clone()],
            Some(&baseline.key),
        );
        assert_eq!(
            shown.damage,
            if degraded { baseline.damage } else { PixelRect { x: 0, y: 19, w: 100, h: 26 } }
        );
        let same =
            FramePlan::build(base_facts.clone(), [pane(7, 1), neighbor.clone()], Some(&shown.key));
        assert_eq!(same.mode, RenderMode::Noop);
        base_facts.window.palette_hash = 1;
        let overlay = FramePlan::build(base_facts, [pane(7, 1), neighbor], Some(&shown.key));
        assert_eq!(overlay.damage, baseline.damage);
    }
}

/// Whole text rows stay against bottom padding as the pane grows, without changing its outer rectangle.
#[test]
fn bottom_alignment_moves_only_fractional_row_slack() {
    for (height, expected_y) in [(91, 9.0), (111, 9.0), (100, 18.0), (84, 2.0)] {
        let mut input = pane(7, 1);
        input.rect.h = height;
        input.rows = ((height - 4) / 20) as u16;
        let plan = FramePlan::build(facts(false), [input], None);
        let p = &plan.panes[0];
        assert_eq!(p.layout.y, expected_y, "height={height}");
        assert_eq!(p.layout.y + f32::from(p.row_count) * 20.0, height as f32 - 2.0);
        assert_eq!(p.full_rect, PixelRect { x: 0, y: 0, w: 100, h: height });
        assert_eq!(p.chrome, PaneRect::new(2.0, 2.0, 96.0, height as f32 - 4.0));
        assert_eq!(p.background_rows, p.row_count);
    }
}

/// Limited grids consume no whole-row slack, while overfull truncated panes retain their old origin.
#[test]
fn bottom_alignment_preserves_row_limit_and_overfull_cases() {
    for (rows, origin, background_rows) in [(2, 9.0, 2), (4, 9.0, 4), (5, 2.0, 4)] {
        let mut input = pane(7, 1);
        input.rect.h = 91;
        input.rows = rows;
        let plan = FramePlan::build(facts(false), [input], None);
        assert_eq!(plan.panes[0].layout.y, origin);
        assert_eq!(plan.panes[0].background_rows, background_rows);
        assert_eq!(plan.panes[0].chrome, PaneRect::new(2.0, 2.0, 96.0, 87.0));
    }
}

/// Dirty damage uses the shifted row origin; resize repositions ink without leaving retained pixels behind.
/// The view is not scrolled back, so the dirty live row is drawn at its own slot.
#[test]
fn bottom_alignment_damage_and_resize_follow_grid_origin() {
    let mut input = live_pane(7, 1);
    input.rect.h = 91;
    let first = FramePlan::build(facts(false), [input.clone()], None);
    input.revision += 1;
    input.dirty_rows = vec![3];
    let dirty = FramePlan::build(facts(false), [input.clone()], Some(&first.key));
    assert_eq!(dirty.damage, PixelRect { x: 0, y: 69, w: 100, h: 20 });
    assert_eq!(dirty.panes[0].content_clip, PaneRect::new(2.0, 9.0, 96.0, 80.0));
    input.rect.h = 100;
    let resized = FramePlan::build(facts(false), [input.clone()], Some(&dirty.key));
    assert_eq!(resized.damage, first.damage);
    assert_eq!(resized.panes[0].layout.y, 18.0);
    input.dirty_rows.clear();
    let same = FramePlan::build(facts(false), [input], Some(&resized.key));
    assert!(same.unchanged);
}

/// The ink-padded strip of `slot` in `planned`, or an empty rectangle when it has no pixels.
fn padded_strip(frame_facts: &FrameFacts, planned: &PlannedPane, slot: u16) -> PixelRect {
    dirty_rows_damage_rect_with_ink_pad(
        [usize::from(slot)],
        planned.full_rect,
        planned.layout.x,
        planned.layout.y,
        planned.cols,
        frame_facts.cell_w,
        frame_facts.cell_h,
        frame_facts.vertical_ink_pad,
        frame_facts.window.width,
        frame_facts.window.height,
    )
    .unwrap_or(PixelRect { x: 0, y: 0, w: 0, h: 0 })
}

/// `input` with a complete record per slot, as the renderer hands the planner after `prior`
/// presented: each slot's record is its padded strip in `prior`'s projection of the same pane.
fn seeded(frame_facts: &FrameFacts, prior: &FramePlan, input: PaneMetadata) -> PaneMetadata {
    seeded_with(frame_facts, prior, input, |_, strip| Some(strip))
}

/// `input` seeded like [`seeded`], with `record` choosing each slot's record from its strip.
fn seeded_with(
    frame_facts: &FrameFacts,
    prior: &FramePlan,
    input: PaneMetadata,
    record: impl Fn(u16, PixelRect) -> Option<PixelRect>,
) -> PaneMetadata {
    let planned = prior.panes.iter().find(|pane| pane.id == input.id).expect("planned pane");
    let row_ink = (0..input.rows)
        .map(|slot| record(slot, padded_strip(frame_facts, planned, slot)))
        .collect();
    PaneMetadata { row_ink, ..input }
}

/// A `Rows` acknowledgement of `rows`.
fn rows_ack<const ROWS: usize>(rows: [usize; ROWS]) -> sonicterm_render_model::AckRows {
    sonicterm_render_model::AckRows::Rows(rows.into_iter().collect())
}

/// A primary pane that is not scrolled back, so each live row is drawn at its own slot.
fn live_pane(id: u64, revision: u64) -> PaneMetadata {
    PaneMetadata { viewport_top_abs: None, ..pane(id, revision) }
}

fn pane(id: u64, revision: u64) -> PaneMetadata {
    PaneMetadata {
        id,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 100, h: 84 },
        cols: 8,
        rows: 4,
        scrollback_len: 20,
        viewport_top_abs: Some(10),
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows: Vec::new(),
        row_ink: Vec::new(),
    }
}

/// One production plan composes key, pane geometry, row slots, primary damage, and unchanged policy.
/// The view is not scrolled back, so the dirty live row is drawn at its own slot.
#[test]
fn primary_plan_composes_complete_decisions() {
    let first = FramePlan::build(facts(false), [live_pane(7, 1)], None);
    assert_eq!(first.mode, RenderMode::Full);
    assert!(first.first_frame);
    assert_eq!(first.damage, PixelRect { x: 0, y: 0, w: 240, h: 160 });
    assert_eq!(first.panes[0].layout, PaneRect::new(2.0, 2.0, 96.0, 80.0));
    assert_eq!(first.panes[0].content_clip, first.panes[0].layout);
    assert_eq!(first.panes[0].rows().collect::<Vec<_>>(), [(0, 20), (1, 21), (2, 22), (3, 23)]);
    let key = first.key.clone();
    let unchanged = FramePlan::build(facts(false), [live_pane(7, 1)], Some(&key));
    assert!(unchanged.unchanged);
    assert_eq!(unchanged.mode, RenderMode::Noop);
    assert_eq!(unchanged.key, key);
    let mut changed = seeded(&facts(false), &first, live_pane(7, 2));
    changed.dirty_rows = vec![1];
    let edited = FramePlan::build(facts(false), [changed], Some(&key));
    assert!(!edited.unchanged);
    // Seeded with complete records, a one-row edit narrower than the surface is partial.
    assert_eq!(edited.mode, RenderMode::Partial);
    assert_eq!(edited.damage, PixelRect { x: 0, y: 22, w: 100, h: 20 });
    assert_eq!(edited.damaged_rows, 1);
    assert_eq!(edited.acknowledged_rows(0, 7, 2), Some(rows_ack([1])));
    assert_eq!(edited.acknowledged_rows(0, 7, 3), None);
    assert_eq!(edited.acknowledged_rows(0, 8, 2), None);
    assert_eq!(edited.acknowledged_rows(1, 7, 2), None);
}

/// Topology, inactive-pane scrolling, overlays, opacity, and degraded policy combine into one full-damage decision.
#[test]
fn interacting_signals_compose_instead_of_relying_on_individual_flags() {
    let mut second = pane(9, 1);
    second.rect.x = 100;
    second.is_active = false;
    let first = FramePlan::build(facts(false), [pane(7, 1), second.clone()], None);
    let mut changed_facts = facts(true);
    changed_facts.window.palette_hash = 43;
    changed_facts.window.background = [0, 0, 0, 0.5_f64.to_bits()];
    second.viewport_top_abs = Some(19);
    second.rect.w = 140;
    second.revision = 2;
    let changed =
        FramePlan::build(changed_facts.clone(), [pane(7, 1), second.clone()], Some(&first.key));
    assert_eq!(changed.mode, RenderMode::Full);
    assert_eq!(changed.damage, PixelRect { x: 0, y: 0, w: 240, h: 160 });
    assert_eq!(changed.panes[1].rows().next(), Some((0, 19)));
    assert_eq!(changed.key.window.palette_hash, 43);
    assert_eq!(changed.key.window.background[3], 0.5_f64.to_bits());
    let same = FramePlan::build(changed_facts, [pane(7, 1), second], Some(&changed.key));
    assert_eq!(same.key, changed.key);
    assert!(same.unchanged);
    let mut only_scroll = pane(7, 1);
    only_scroll.viewport_top_abs = Some(11);
    let moved = FramePlan::build(facts(false), [only_scroll], Some(&first.key));
    assert_eq!(moved.damage, first.damage);
}

/// Alternate-screen dirt repaints its clipped pane; software policy expands the final retained damage to the surface.
#[test]
fn alternate_and_degraded_damage_keep_their_distinct_bounds() {
    let mut input = pane(7, 1);
    input.rect = PixelRect { x: -10, y: -5, w: 100, h: 84 };
    input.is_alt = true;
    let first = FramePlan::build(facts(false), [input.clone()], None);
    input.revision += 1;
    input.dirty_rows = vec![1];
    let changed = FramePlan::build(facts(false), [input.clone()], Some(&first.key));
    assert_eq!(changed.panes[0].full_clip, Some(PixelRect { x: 0, y: 0, w: 90, h: 79 }));
    assert_eq!(changed.damage, PixelRect { x: 0, y: 0, w: 90, h: 79 });
    let software = FramePlan::build(facts(true), [input], Some(&first.key));
    assert_eq!(software.mode, RenderMode::Full);
    assert_eq!(software.damage, PixelRect { x: 0, y: 0, w: 240, h: 160 });
}

/// Image clipping never inherits the one-cell layout floor, and hidden history adds no row records to the plan.
#[test]
fn small_pane_and_hidden_history_keep_plan_storage_bounded() {
    let mut input = pane(7, 1);
    input.rect = PixelRect { x: 10, y: 10, w: 3, h: 3 };
    input.scrollback_len = u64::MAX - 100;
    let plan = FramePlan::build(facts(false), [input.clone()], None);
    assert_eq!(plan.panes[0].layout.w, 10.0);
    assert_eq!(plan.panes[0].layout.h, 20.0);
    assert_eq!(plan.panes[0].content_clip.w, 0.0);
    assert_eq!(plan.panes[0].content_clip.h, 0.0);
    assert_eq!(plan.panes.len(), 1);
    assert_eq!(plan.key.panes.len(), 1);
    assert_eq!(plan.panes[0].rows().count(), 4);
    assert!(plan.panes[0].dirty_live_rows.is_empty());
    assert!(plan.panes[0].dirty_slots.is_empty());
    input.viewport_top_abs = Some(u64::MAX);
    let clamped = FramePlan::build(facts(false), [input], Some(&plan.key));
    assert_eq!(clamped.panes[0].view_top_abs, u64::MAX - 100);
    assert_eq!(clamped.panes[0].rows().count(), 4);
}

/// Every former pane revision triple member and each drawn geometry/scrollbar field affects the key.
#[test]
fn pane_identity_preserves_each_input_discriminator() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None);
    let mutations: [fn(&mut PaneMetadata); 10] = [
        |p| p.id += 1,
        |p| p.revision += 1,
        |p| p.viewport_top_abs = Some(11),
        |p| p.rect.x += 1,
        |p| p.cols += 1,
        |p| p.rows += 1,
        |p| p.scrollback_len += 1,
        |p| p.is_active = false,
        |p| p.is_alt = true,
        |p| p.scrollbar_alpha = 1.0,
    ];
    for change in mutations {
        let mut input = pane(7, 1);
        change(&mut input);
        let changed = FramePlan::build(facts(false), [input], Some(&baseline.key));
        assert_ne!(changed.key, baseline.key);
        assert!(!changed.unchanged);
    }
}

/// Copy-mode identity hashes all quick-select fields without retaining any hint or text payload.
#[test]
fn compact_copy_identity_covers_every_drawn_and_owned_field() {
    use sonicterm_render_model::boundary::ui::copy_mode::{
        CopyMode, CopyModeState, QuickSelectHint, QuickSelectState,
    };
    let mut baseline = CopyModeState::new_at((1, 2));
    baseline.quick_select = Some(QuickSelectState {
        hints: vec![QuickSelectHint {
            hint: 'a',
            row: 3,
            col_start: 4,
            col_end: 5,
            text: "https://example.com".into(),
        }],
    });
    let identity = CopyModeIdentity::from(&baseline);
    assert_eq!(identity, CopyModeIdentity::from(&baseline));
    let changes: [fn(&mut CopyModeState); 11] = [
        |s| s.cursor.0 += 1,
        |s| s.cursor.1 += 1,
        |s| s.anchor = Some((0, 0)),
        |s| s.mode = CopyMode::Select,
        |s| s.read_only = true,
        |s| s.quick_select = None,
        |s| s.quick_select.as_mut().unwrap().hints[0].hint = 'b',
        |s| s.quick_select.as_mut().unwrap().hints[0].row += 1,
        |s| s.quick_select.as_mut().unwrap().hints[0].col_start += 1,
        |s| s.quick_select.as_mut().unwrap().hints[0].col_end += 1,
        |s| s.quick_select.as_mut().unwrap().hints[0].text.push('x'),
    ];
    for change in changes {
        let mut changed = baseline.clone();
        change(&mut changed);
        assert_ne!(identity, CopyModeIdentity::from(&changed));
    }
}

/// Every window-level field without a narrow damage class rejects the old key and repaints the
/// whole surface without requiring a grid mutation. Narrow-class fields are covered by their own
/// tests (cursor, focus, tab band, selection).
#[test]
fn window_identity_mutations_require_full_repaint() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None);
    let changes: Vec<fn(&mut WindowIdentity)> = vec![
        |w| w.copy_mode = Some(CopyModeIdentity::from(&CopyModeState::new_at((0, 0)))),
        |w| w.quick_select_hint_count = 1,
        |w| w.tab = 3,
        |w| w.search_hash = 1,
        |w| w.palette_hash = 1,
        |w| w.ime_hash = 1,
        |w| w.notification_hash = 1,
        |w| w.width += 1,
        |w| w.height += 1,
        |w| w.viewport_top_abs = Some(9),
        |w| w.pane_focus_flash_bucket = 1,
        |w| w.broadcast_participants_hash = 1,
        |w| w.inline_media_hash = 1,
        |w| w.subpixel_aa = SubpixelAaMode::Rgb,
        |w| w.background[3] = 0.5_f64.to_bits(),
        |w| w.style_rev = 1,
        |w| w.renderer_hash = 1,
        |w| w.overlay_active = true,
    ];
    for change in changes {
        let mut input = facts(false);
        change(&mut input.window);
        let expected = input.window.clone();
        let changed = FramePlan::build(input, [pane(7, 1)], Some(&baseline.key));
        assert_eq!(changed.key.window, expected);
        assert_ne!(changed.key, baseline.key);
        assert!(!changed.unchanged);
        assert_eq!(changed.mode, RenderMode::Full);
        assert_eq!(changed.damage.w, expected.width);
        assert_eq!(changed.damage.h, expected.height);
    }
}

/// A search toggle that keeps the match count and focus but moves the highlight is not an unchanged frame.
#[test]
fn search_toggle_that_moves_the_highlight_is_not_an_unchanged_frame() {
    use sonicterm_render_model::boundary::{
        grid::grid::{CellFlags, Color, Grid},
        ui::search::SearchState,
    };

    let mut grid = Grid::new(8, 1);
    for ch in "aa+".chars() {
        grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
    }
    let mut search = SearchState::new();
    search.set_query("a+", &grid);
    search.anchor_to_viewport(0);
    let literal = search.presentation_hash(0, grid.rows);
    let (count, current) = (search.matches.len(), search.current);
    search.toggle_regex(&grid);
    search.anchor_to_viewport(0);
    assert_eq!((search.matches.len(), search.current), (count, current));
    let regex = search.presentation_hash(0, grid.rows);
    assert_ne!(literal, regex);

    let mut first = facts(false);
    first.window.search_hash = literal;
    let baseline = FramePlan::build(first, [pane(7, 1)], None);
    let mut same = facts(false);
    same.window.search_hash = literal;
    assert!(FramePlan::build(same, [pane(7, 1)], Some(&baseline.key)).unchanged);
    let mut toggled = facts(false);
    toggled.window.search_hash = regex;
    let plan = FramePlan::build(toggled, [pane(7, 1)], Some(&baseline.key));
    assert!(!plan.unchanged);
    assert_eq!(plan.mode, RenderMode::Full);
}

#[test]
fn broadcast_toggle_off_repaints_unchanged_terminal_content() {
    // Removing safety chrome requires full damage even when every grid revision stays unchanged.
    let mut armed = facts(false);
    armed.window.broadcast_participants_hash = 42;
    let baseline = FramePlan::build(armed, [pane(7, 1)], None);
    let disabled = FramePlan::build(facts(false), [pane(7, 1)], Some(&baseline.key));
    assert!(!disabled.unchanged);
    assert_eq!(disabled.mode, RenderMode::Full);
    assert_eq!(disabled.damage, baseline.damage);
}

/// Viewport-only changes on an inactive pane still repaint even when the active pane and grid revisions are unchanged.
#[test]
fn inactive_pane_scroll_alone_is_not_lost_in_composition() {
    let mut inactive = pane(9, 1);
    inactive.is_active = false;
    inactive.rect.x = 100;
    let baseline = FramePlan::build(facts(true), [pane(7, 1), inactive.clone()], None);
    inactive.viewport_top_abs = Some(11);
    let moved = FramePlan::build(facts(true), [pane(7, 1), inactive], Some(&baseline.key));
    assert!(!moved.unchanged);
    assert_eq!(moved.mode, RenderMode::Full);
    assert_eq!(moved.damage, baseline.damage);
    assert_eq!(moved.panes[1].rows().next(), Some((0, 11)));
}

/// Off-surface panes and padding-exhausted images share planned empty clips while logical layout stays cell-sized.
#[test]
fn planned_clips_and_background_bounds_use_actual_pane_surface_intersection() {
    let mut input = pane(7, 1);
    input.rect = PixelRect { x: 300, y: 200, w: 100, h: 100 };
    let plan = FramePlan::build(facts(false), [input], None);
    assert_eq!(plan.panes[0].full_clip, None);
    assert_eq!(plan.panes[0].content_clip.w, 0.0);
    assert_eq!(plan.panes[0].content_clip.h, 0.0);
    let mut input = pane(7, 1);
    input.rect = PixelRect { x: -10, y: -5, w: 35, h: 31 };
    let plan = FramePlan::build(facts(false), [input], None);
    assert_eq!(plan.panes[0].full_clip, Some(PixelRect { x: 0, y: 0, w: 25, h: 26 }));
    assert_eq!(plan.panes[0].content_clip, PaneRect::new(0.0, 0.0, 23.0, 24.0));
    assert_eq!(plan.panes[0].background_cols, 3);
    assert_eq!(plan.panes[0].background_rows, 1);
}

/// A revision-only change with no visible damage is a distinct Noop exit, not
/// an unchanged frame; recording its key must never acknowledge its revision. The degraded
/// path always planned it so; the hardware path does too (A7), on a live view as on a scrolled one.
#[test]
fn changed_revision_without_damage_is_noop_and_never_acknowledged() {
    for (degraded, input) in [(true, pane as fn(u64, u64) -> PaneMetadata), (false, live_pane)] {
        let baseline = FramePlan::build(facts(degraded), [input(7, 1)], None);
        let noop = FramePlan::build(facts(degraded), [input(7, 2)], Some(&baseline.key));
        assert!(!noop.unchanged);
        assert_eq!(noop.mode, RenderMode::Noop, "degraded={degraded}");
        assert_ne!(noop.key, baseline.key);
        assert_eq!(noop.key.panes[0].revision, 2, "the key records the new revision");
        assert_eq!(noop.acknowledged_rows(0, 7, 2), None);
    }
}

/// On the hardware path, a revision bump with no dirty row anywhere (the grid's `set_autowrap`)
/// and no class or hover change draws nothing new: it is the A7 `Noop` with empty damage, its key
/// recorded and nothing acknowledged, as the degraded path already planned it. This is not the
/// offscreen-only rule, which needs non-empty live dirt.
#[test]
fn hardware_revision_change_without_dirty_rows_is_an_a7_noop() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None);
    let bumped = FramePlan::build(facts(false), [pane(7, 2)], Some(&baseline.key));
    assert!(bumped.panes[0].view_top_abs < bumped.panes[0].scrollback_len, "scrolled back");
    assert!(bumped.panes[0].dirty_live_rows.is_empty());
    assert!(!bumped.unchanged);
    assert_eq!(bumped.mode, RenderMode::Noop);
    assert_eq!(bumped.damage, PixelRect { x: 0, y: 0, w: 0, h: 0 });
    assert_eq!(bumped.key.panes[0].revision, 2);
    assert_eq!(bumped.acknowledged_rows(0, 7, 2), None);
}

/// A renderer that clears its retained frame key, as a device rebuild does, draws
/// a full first frame: a plan built without a previous key is a full first frame.
#[test]
fn plan_without_a_previous_key_is_a_full_first_frame() {
    let plan = FramePlan::build(facts(false), [pane(7, 1)], None);
    assert!(plan.first_frame);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 0, w: 240, h: 160 });
}

#[test]
fn a_full_plan_counts_one_full_frame_and_a_noop_plan_none() {
    // full_frames counts where the plan decides its mode, only inside a counting renderer's scope.
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        let first = FramePlan::build(facts(false), [pane(7, 1)], None);
        assert_eq!(first.mode, RenderMode::Full);
        let unchanged = FramePlan::build(facts(false), [pane(7, 1)], Some(&first.key));
        assert_eq!(unchanged.mode, RenderMode::Noop);
    }
    assert_eq!(sink.snapshot().full_frames, 1);
    let uncounted = FramePlan::build(facts(false), [pane(7, 1)], None);
    assert_eq!(uncounted.mode, RenderMode::Full);
    assert_eq!(sink.snapshot().full_frames, 1, "nothing counts with the gate off");
}

/// An in-place edit of a live row while the primary screen is scrolled back
/// must damage the screen slot where that live row is drawn. Grid dirty rows
/// index the live buffer, so live row 5 is absolute row `scrollback_len + 5`,
/// which a view scrolled back by three rows draws at slot 8, not slot 5.
#[test]
fn scrolled_back_in_place_edit_damages_the_slot_that_draws_the_live_row() {
    let window = WindowIdentity { width: 240, height: 600, ..Default::default() };
    let tall_facts = FrameFacts { window, ..facts(false) };
    let mut input = pane(7, 1);
    input.rect = PixelRect { x: 0, y: 0, w: 100, h: 484 };
    input.rows = 24;
    input.scrollback_len = 100;
    input.viewport_top_abs = Some(97);
    let first = FramePlan::build(tall_facts.clone(), [input.clone()], None);
    // A CR-redrawn progress bar: only the revision and one live dirty row change.
    input.revision += 1;
    input.dirty_rows = vec![5];
    let edited = FramePlan::build(tall_facts.clone(), [input], Some(&first.key));
    let planned = &edited.panes[0];
    let live_row_abs = 100 + 5;
    let drawn_slot = planned
        .rows()
        .find(|(_, row_abs)| *row_abs == live_row_abs)
        .map(|(slot, _)| slot)
        .expect("the scrolled-back view draws live row 5");
    assert_eq!(drawn_slot, 8);
    let slot_rect = dirty_rows_damage_rect_with_ink_pad(
        [usize::from(drawn_slot)],
        planned.full_rect,
        planned.layout.x,
        planned.layout.y,
        planned.cols,
        tall_facts.cell_w,
        tall_facts.cell_h,
        tall_facts.vertical_ink_pad,
        tall_facts.window.width,
        tall_facts.window.height,
    )
    .expect("slot 8 has pixels");
    assert_eq!(edited.mode, RenderMode::Full);
    assert_eq!(
        edited.damage.intersect(slot_rect),
        Some(slot_rect),
        "damage {:?} must cover slot {drawn_slot} at {slot_rect:?}",
        edited.damage
    );
}

/// Facts for a 24-row pane: a surface tall enough that every viewport slot has pixels.
fn tall_facts(degraded: bool) -> FrameFacts {
    let window = WindowIdentity { width: 240, height: 600, ..Default::default() };
    FrameFacts { window, ..facts(degraded) }
}

/// A primary pane with 100 history rows whose view is scrolled back three rows, so live
/// row `r` is drawn at slot `r + 3` and live rows 21 to 23 are below the view.
fn scrolled_back_pane() -> PaneMetadata {
    PaneMetadata {
        rect: PixelRect { x: 0, y: 0, w: 100, h: 484 },
        rows: 24,
        scrollback_len: 100,
        viewport_top_abs: Some(97),
        ..pane(7, 1)
    }
}

/// The damage rectangle one viewport slot of `planned` occupies under `frame_facts`.
fn slot_rect(frame_facts: &FrameFacts, planned: &PlannedPane, slot: usize) -> PixelRect {
    dirty_rows_damage_rect_with_ink_pad(
        [slot],
        planned.full_rect,
        planned.layout.x,
        planned.layout.y,
        planned.cols,
        frame_facts.cell_w,
        frame_facts.cell_h,
        frame_facts.vertical_ink_pad,
        frame_facts.window.width,
        frame_facts.window.height,
    )
    .expect("the slot has pixels")
}

/// Plan an edit to live row 22 of the scrolled-back pane, which no slot draws, and
/// return the plan together with the full frames counted while building it.
fn offscreen_only_edit(degraded: bool) -> (FramePlan, u64) {
    let first = FramePlan::build(tall_facts(degraded), [scrolled_back_pane()], None);
    let mut edited = scrolled_back_pane();
    edited.revision = 2;
    edited.dirty_rows = vec![22];
    let sink = crate::frame_stats::FrameStatsSink::default();
    let plan = {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        FramePlan::build(tall_facts(degraded), [edited], Some(&first.key))
    };
    (plan, sink.snapshot().full_frames)
}

/// A revision change whose only dirt is scrolled below the view presents nothing on the
/// hardware path: a Noop with empty damage that acknowledges no revision and counts no
/// full frame, so the grid keeps its dirt for the frame that scrolls the row into view.
#[test]
fn offscreen_only_edit_is_a_noop_on_the_hardware_path() {
    let (plan, full_frames) = offscreen_only_edit(false);
    assert!(!plan.unchanged);
    assert_eq!(plan.mode, RenderMode::Noop);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 0, w: 0, h: 0 });
    assert_eq!(plan.acknowledged_rows(0, 7, 2), None);
    assert_eq!(full_frames, 0);
}

/// The degraded path makes the same Noop decision for offscreen-only dirt; mapping the live
/// row to its slot is what keeps the dirt from landing on a visible slot and forcing a Full.
#[test]
fn offscreen_only_edit_is_a_noop_on_the_degraded_path() {
    let (plan, full_frames) = offscreen_only_edit(true);
    assert!(!plan.unchanged);
    assert_eq!(plan.mode, RenderMode::Noop);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 0, w: 0, h: 0 });
    assert_eq!(plan.acknowledged_rows(0, 7, 2), None);
    assert_eq!(full_frames, 0);
}

/// With one dirty live row drawn and one scrolled below the view, damage covers only the
/// slot that draws the visible row and nothing at the live rows' unmapped slots.
#[test]
fn mixed_onscreen_and_offscreen_edits_damage_only_the_drawn_slot() {
    let first = FramePlan::build(tall_facts(false), [scrolled_back_pane()], None);
    let mut edited = scrolled_back_pane();
    edited.revision = 2;
    edited.dirty_rows = vec![5, 22];
    let plan = FramePlan::build(tall_facts(false), [edited], Some(&first.key));
    let planned = &plan.panes[0];
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, slot_rect(&tall_facts(false), planned, 8));
    assert_eq!(plan.damage.intersect(slot_rect(&tall_facts(false), planned, 5)), None);
}

/// While a terminal preedit is active, an offscreen-only edit still repaints in full on
/// both paths: the preedit is drawn at the live cursor, which the key does not carry.
#[test]
fn offscreen_only_edit_with_an_active_preedit_stays_full() {
    for degraded in [false, true] {
        let mut composing = tall_facts(degraded);
        composing.window.ime_hash = 0xFEED;
        composing.window.overlay_active = true;
        let first = FramePlan::build(composing.clone(), [scrolled_back_pane()], None);
        let mut edited = scrolled_back_pane();
        edited.revision = 2;
        edited.dirty_rows = vec![22];
        let plan = FramePlan::build(composing, [edited], Some(&first.key));
        assert!(!plan.unchanged);
        assert_eq!(plan.mode, RenderMode::Full, "degraded={degraded}");
    }
}

/// A real grid write below a scrolled-back view moves the terminal cursor column without
/// changing the IME hash, so with a preedit active the plan must not be a Noop; only the
/// overlay exclusion of the offscreen-only rule keeps the preedit from going stale.
#[test]
fn offscreen_write_that_moves_the_cursor_under_a_preedit_is_not_a_noop() {
    use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};

    let mut grid = Grid::new(8, 4);
    for _ in 0..10 {
        grid.linefeed();
    }
    let scrollback_len = grid.scrollback_len() as u64;
    assert!(scrollback_len >= 4, "the history is deep enough to scroll the cursor row away");
    let view_top_abs = scrollback_len - 4;
    let metadata = |grid: &Grid| PaneMetadata {
        revision: grid.revision(),
        dirty_generation: grid.dirty_generation(),
        scrollback_len: grid.scrollback_len() as u64,
        viewport_top_abs: Some(view_top_abs),
        dirty_rows: grid.dirty_rows().collect(),
        ..pane(7, 1)
    };
    grid.clear_dirty();
    let mut composing = facts(false);
    composing.window.ime_hash = 0xFEED;
    composing.window.overlay_active = true;
    for degraded in [false, true] {
        composing.degraded = degraded;
        let first = FramePlan::build(composing.clone(), [metadata(&grid)], None);
        let column_before = grid.cursor.col;
        grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        assert_ne!(grid.cursor.col, column_before, "the write advances the cursor column");
        let written = metadata(&grid);
        assert_eq!(written.dirty_rows, vec![usize::from(grid.cursor.row)]);
        let plan = FramePlan::build(composing.clone(), [written], Some(&first.key));
        assert_ne!(plan.mode, RenderMode::Noop, "degraded={degraded}");
        grid.clear_dirty();
    }
}

/// Not scrolled back, slots equal live rows: the dirty row's own slot is damaged, exactly
/// as before live rows were mapped to slots.
#[test]
fn live_view_damages_the_dirty_rows_own_slot() {
    let live = PaneMetadata { viewport_top_abs: None, ..scrolled_back_pane() };
    let first = FramePlan::build(tall_facts(false), [live.clone()], None);
    let mut edited = live;
    edited.revision = 2;
    edited.dirty_rows = vec![5];
    let plan = FramePlan::build(tall_facts(false), [edited], Some(&first.key));
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, slot_rect(&tall_facts(false), &plan.panes[0], 5));
    assert_eq!(plan.damaged_rows, 1);
}

/// Hover spans are already viewport rows, so a scrolled-back pane's hover on row 2 damages
/// slot 2 and is never shifted by the scrollback offset.
#[test]
fn scrolled_back_hover_spans_stay_viewport_rows() {
    let baseline = FramePlan::build(tall_facts(false), [scrolled_back_pane()], None);
    let mut hovering = tall_facts(false);
    hovering.window.hovered_url_cells = HoveredUrlCells::single(7, 2, 1, 5, false);
    let plan = FramePlan::build(hovering.clone(), [scrolled_back_pane()], Some(&baseline.key));
    assert_eq!(plan.damage, slot_rect(&hovering, &plan.panes[0], 2));
}

/// A live row maps to the slot `scrollback_len + live_row - view_top_abs` when that slot is
/// inside the viewport and to no slot otherwise; a view top past the live top is clamped to
/// it first, exactly as `FramePlan::build` clamps the view, so slots equal live rows there.
#[test]
fn live_row_to_slot_maps_live_rows_into_the_viewport() {
    // (scrollback_len, view_top_abs, rows, live_row, expected slot)
    let cases: [(u64, u64, u16, usize, Option<u16>); 9] = [
        (100, 97, 24, 0, Some(3)),
        (100, 97, 24, 20, Some(23)),
        (100, 97, 24, 21, None),
        (100, 97, 24, 23, None),
        (100, 100, 24, 0, Some(0)),
        (100, 100, 24, 23, Some(23)),
        (100, 100, 24, 24, None),
        (100, 250, 24, 7, Some(7)),
        (0, 0, 4, 3, Some(3)),
    ];
    for (scrollback_len, view_top_abs, rows, live_row, expected) in cases {
        assert_eq!(
            live_row_to_slot(scrollback_len, view_top_abs, rows, live_row),
            expected,
            "scrollback_len={scrollback_len} view_top_abs={view_top_abs} rows={rows} live_row={live_row}"
        );
    }
}

/// A pane whose only change is a newer dirty generation is never the unchanged key, so unacknowledged
/// dirt cannot take the shortcut. With a dirty row inside the clipped surface it plans `Full` with
/// damage on both paths; with dirt only outside it, the existing no-drawable-damage `Noop` is kept.
#[test]
fn a_newer_dirty_generation_never_takes_the_unchanged_shortcut() {
    for degraded in [false, true] {
        let first = FramePlan::build(facts(degraded), [live_pane(7, 1)], None);
        let mut marked = live_pane(7, 1);
        marked.dirty_generation = 1;
        let same_key = FramePlan::build(facts(degraded), [marked.clone()], Some(&first.key));
        assert!(!same_key.unchanged, "degraded={degraded}");
        marked.dirty_rows = vec![1];
        let drawable = FramePlan::build(facts(degraded), [marked], Some(&first.key));
        assert_eq!(drawable.mode, RenderMode::Full, "degraded={degraded}");
        assert!(drawable.damage.h > 0, "the damage covers the dirty row");

        let scrolled = FramePlan::build(tall_facts(degraded), [scrolled_back_pane()], None);
        let mut offscreen = scrolled_back_pane();
        offscreen.dirty_generation = 1;
        offscreen.dirty_rows = vec![22];
        let plan = FramePlan::build(tall_facts(degraded), [offscreen], Some(&scrolled.key));
        assert!(!plan.unchanged);
        assert_eq!(plan.mode, RenderMode::Noop, "degraded={degraded}");
        assert_eq!(plan.damage, PixelRect { x: 0, y: 0, w: 0, h: 0 });
    }
}

/// The tab bar band `facts` draws: full width from y=140 to the 160 px surface bottom.
const TAB_BAND: PixelRect = PixelRect { x: 0, y: 140, w: 240, h: 20 };

/// The ink-padded damage of viewport slot `slot` of the default 100 px pane (zero ink pad).
fn row_rect(slot: i32) -> PixelRect {
    PixelRect { x: 0, y: 2 + 20 * slot, w: 100, h: 20 }
}

/// A drawn cursor on pane 7 at viewport slot `slot`, column 2.
fn cursor_at(slot: u16) -> Option<CursorCell> {
    Some(CursorCell { pane_id: 7, slot, col: 2, span: 1 })
}

/// Hardware facts with a visible, focused cursor at slot 1 of pane 7.
fn cursor_facts() -> FrameFacts {
    let mut cursor = facts(false);
    cursor.window.cursor_visible = true;
    cursor.window.window_focused = true;
    cursor.window.cursor_cell = cursor_at(1);
    cursor
}

/// Plan `after` against a presented `before` frame of the same pane input.
fn transition(before: FrameFacts, after: FrameFacts, input: PaneMetadata) -> FramePlan {
    let first = FramePlan::build(before, [input.clone()], None);
    FramePlan::build(after, [input], Some(&first.key))
}

/// Cursor visibility, shape and blink changes damage only the drawn cursor row on the hardware
/// path, plus the previous frame's recolor bounds when they reach past that row.
#[test]
fn cursor_class_changes_damage_only_the_cursor_row() {
    let changes: [fn(&mut FrameFacts); 3] = [
        |after| {
            after.window.cursor_visible = false;
            after.window.cursor_cell = None;
        },
        |after| after.window.cursor_shape = 1,
        |after| after.window.cursor_blink = true,
    ];
    for change in changes {
        let mut after = cursor_facts();
        change(&mut after);
        let plan = transition(cursor_facts(), after.clone(), live_pane(7, 1));
        assert_eq!(plan.mode, RenderMode::Full);
        assert!(plan.change.cursor && !plan.change.full);
        assert_eq!(plan.damage, row_rect(1));

        // A recolored glyph presented last frame reaching above the row is restored too.
        after.previous_recolor = RecolorRecord {
            bounds: RecolorBounds::Rect(PixelRect { x: 50, y: 10, w: 10, h: 40 }),
            hash: 9,
        };
        let plan = transition(cursor_facts(), after, live_pane(7, 1));
        assert_eq!(plan.damage, PixelRect { x: 0, y: 10, w: 100, h: 40 });
    }
}

/// A drawn cursor that moves with no grid dirt damages its old and new rows, and the parts
/// record both rows rather than the gap between them.
#[test]
fn cursor_cell_move_damages_old_and_new_rows() {
    let mut after = cursor_facts();
    after.window.cursor_cell = cursor_at(3);
    let plan = transition(cursor_facts(), after, live_pane(7, 1));
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, row_rect(1).union(row_rect(3)));
    assert_eq!(sonicterm_render_model::covered_area(&plan.damage_parts), 2 * 100 * 20);
}

/// Focus damages the cursor rows and the tab band (its active marker and title); with the tab
/// bar hidden it damages the cursor rows only.
#[test]
fn focus_change_damages_cursor_rows_and_the_tab_band() {
    let mut blurred = cursor_facts();
    blurred.window.window_focused = false;
    blurred.window.cursor_cell = None;
    let plan = transition(cursor_facts(), blurred.clone(), live_pane(7, 1));
    assert!(plan.change.focus && !plan.change.full);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, row_rect(1).union(TAB_BAND));

    let mut hidden_before = cursor_facts();
    hidden_before.tab_bar_top = None;
    blurred.tab_bar_top = None;
    let plan = transition(hidden_before, blurred, live_pane(7, 1));
    assert_eq!(plan.damage, row_rect(1));
}

/// Each tab-bar field damages only the tab band.
#[test]
fn tab_band_fields_damage_only_the_tab_band() {
    let changes: [fn(&mut WindowIdentity); 4] = [
        |window| window.tab_hash = 1,
        |window| window.hover_tab = 1,
        |window| window.close_override = 1,
        |window| window.process_privileged = true,
    ];
    for change in changes {
        let mut after = facts(false);
        change(&mut after.window);
        let plan = transition(facts(false), after, pane(7, 1));
        assert!(plan.change.tab_band && !plan.change.full);
        assert_eq!(plan.mode, RenderMode::Full);
        assert_eq!(plan.damage, TAB_BAND);
    }
}

/// Facts whose pane padding leaves 12 px on the right, at `scale`.
fn padded_facts(scale: f32) -> FrameFacts {
    FrameFacts { padding: [2.0, 12.0, 2.0, 2.0], scale, ..facts(false) }
}

/// Plan a scrollbar opacity change from `alpha_before` to `alpha_after` on `input`.
fn scrollbar_fade(
    frame_facts: FrameFacts,
    mut input: PaneMetadata,
    alpha_before: f32,
    alpha_after: f32,
) -> FramePlan {
    input.scrollbar_alpha = alpha_before;
    let first = FramePlan::build(frame_facts.clone(), [input.clone()], None);
    input.scrollbar_alpha = alpha_after;
    FramePlan::build(frame_facts, [input], Some(&first.key))
}

/// A scrollbar opacity step damages exactly the drawn track: inside the padded chrome (x=80-88
/// for a 100 px pane with 12 px right padding), 16 px wide at scale 2, clamped to a narrow
/// pane's chrome, still damaged when fading below the emit floor, and nothing in mode `Never`.
#[test]
fn scrollbar_fade_damages_exactly_the_drawn_track() {
    let plan = scrollbar_fade(padded_facts(1.0), pane(7, 1), 1.0, 0.5);
    assert!(plan.change.scrollbar && !plan.change.full);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, PixelRect { x: 80, y: 2, w: 8, h: 80 });

    let plan = scrollbar_fade(padded_facts(2.0), pane(7, 1), 1.0, 0.5);
    assert_eq!(plan.damage, PixelRect { x: 72, y: 2, w: 16, h: 80 });

    // A 10 px chrome (the one-cell floor) is narrower than the 16 px bar: the track is the chrome.
    let mut narrow = pane(7, 1);
    narrow.rect.w = 10;
    let plan = scrollbar_fade(padded_facts(2.0), narrow, 1.0, 0.5);
    assert_eq!(plan.damage, PixelRect { x: 2, y: 2, w: 10, h: 80 });

    let floor = sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR;
    let plan = scrollbar_fade(padded_facts(1.0), pane(7, 1), 0.5, floor / 2.0);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, PixelRect { x: 80, y: 2, w: 8, h: 80 });

    let never = FrameFacts { scrollbar_mode: ScrollbarMode::Never, ..padded_facts(1.0) };
    let plan = scrollbar_fade(never, pane(7, 1), 1.0, 0.5);
    assert!(plan.unchanged);
    assert_eq!(plan.mode, RenderMode::Noop);
}

/// Extending a selection by one row damages only the rows whose selection quads differ: the
/// old and new end rows, never the unchanged middle rows, and needs no grid dirt.
#[test]
fn selection_extension_damages_only_the_changed_end_rows() {
    // A live view: slot `s` draws absolute row 20 + s.
    let mut before = facts(false);
    let mut selection = Selection::new(21, 0);
    selection.extend(22, 3);
    before.window.selection = Some(selection);
    let mut after = before.clone();
    selection.extend(23, 1);
    after.window.selection = Some(selection);
    let plan = transition(before.clone(), after, live_pane(7, 1));
    assert!(plan.change.selection && !plan.change.full);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, row_rect(2).union(row_rect(3)));

    let mut same_row = before.clone();
    let mut widened = Selection::new(21, 0);
    widened.extend(22, 5);
    same_row.window.selection = Some(widened);
    let plan = transition(before, same_row, live_pane(7, 1));
    assert_eq!(plan.damage, row_rect(2));
}

/// A hover-only change on a clean grid, even with a revision bump, is repainted: it plans a
/// `Full` frame damaging the hovered row, never the A7 `Noop`, because composed damage is not empty.
#[test]
fn hover_only_change_on_a_clean_grid_is_repainted() {
    let mut hovering = facts(false);
    hovering.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
    for revision in [1, 2] {
        let first = FramePlan::build(facts(false), [live_pane(7, 1)], None);
        let plan = FramePlan::build(hovering.clone(), [live_pane(7, revision)], Some(&first.key));
        assert!(plan.panes[0].dirty_live_rows.is_empty());
        assert_eq!(plan.mode, RenderMode::Full, "revision {revision}");
        assert_eq!(plan.damage, row_rect(1));
    }
}

/// A scrolled-back edit (history 100, view top 97, live row 5 at slot 8) in the same frame as a
/// cursor toggle damages slot 8's strip and invalidates absolute row 105; the cursor is not drawn
/// in a scrolled-back view, so its class adds no row.
#[test]
fn scrolled_back_edit_with_a_cursor_toggle_keeps_its_slot_and_invalidation() {
    let mut before = tall_facts(false);
    before.window.cursor_visible = true;
    before.window.window_focused = true;
    before.tab_bar_top = None;
    let mut after = before.clone();
    after.window.cursor_visible = false;
    let first = FramePlan::build(before, [scrolled_back_pane()], None);
    let mut edited = scrolled_back_pane();
    edited.revision = 2;
    edited.dirty_rows = vec![5];
    let plan = FramePlan::build(after.clone(), [edited], Some(&first.key));
    assert!(plan.change.cursor && !plan.change.full);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, slot_rect(&after, &plan.panes[0], 8));
    let invalidated: Vec<u64> = plan.panes[0]
        .dirty_live_rows
        .iter()
        .map(|&row| plan.panes[0].scrollback_len + row as u64)
        .collect();
    assert_eq!(invalidated, [105]);
}

/// A 10-row pane with 100 history rows viewed from row 90, so live row 5 (slot 15) is offscreen.
fn offscreen_dirt_pane() -> PaneMetadata {
    PaneMetadata {
        rect: PixelRect { x: 0, y: 0, w: 100, h: 204 },
        rows: 10,
        scrollback_len: 100,
        viewport_top_abs: Some(90),
        ..pane(7, 1)
    }
}

/// Offscreen-only dirt stays the existing `Noop` with empty damage; adding a tab-bar change
/// makes the frame `Full` damaging only the tab band, with the dirt still invalidated (row 105).
#[test]
fn offscreen_dirt_with_and_without_a_tab_band_change() {
    let with_band = FrameFacts { tab_bar_top: Some(580.0), ..tall_facts(false) };
    let first = FramePlan::build(with_band.clone(), [offscreen_dirt_pane()], None);
    let mut edited = offscreen_dirt_pane();
    edited.revision = 2;
    edited.dirty_rows = vec![5];
    let plan = FramePlan::build(with_band.clone(), [edited.clone()], Some(&first.key));
    assert_eq!(plan.mode, RenderMode::Noop);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 0, w: 0, h: 0 });
    assert_eq!(plan.acknowledged_rows(0, 7, 2), None);

    let mut retitled = with_band;
    retitled.window.tab_hash = 1;
    let plan = FramePlan::build(retitled, [edited], Some(&first.key));
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 580, w: 240, h: 20 });
    assert_eq!(plan.panes[0].scrollback_len + plan.panes[0].dirty_live_rows[0] as u64, 105);
}

/// A tab-band and a cursor change in one frame damage one rectangle, their union, while the
/// parts cover only the two areas, which is what the waste counter measures.
#[test]
fn two_classes_union_into_one_rect_with_measured_parts() {
    let mut after = cursor_facts();
    after.window.cursor_visible = false;
    after.window.cursor_cell = None;
    after.window.tab_hash = 1;
    let plan = transition(cursor_facts(), after, live_pane(7, 1));
    assert_eq!(plan.damage, row_rect(1).union(TAB_BAND));
    let covered = sonicterm_render_model::covered_area(&plan.damage_parts);
    assert_eq!(covered, 100 * 20 + 240 * 20);
    assert!(plan.damage_parts.iter().all(|part| part.intersect(plan.damage) == Some(*part)));
}

/// Conservative rules keep the whole surface: an active overlay with a class change, a degraded
/// plan with a class change, and the first frame. An alternate-screen pane's class damage is
/// the whole pane, not the cursor row.
#[test]
fn conservative_rules_keep_whole_surface_or_whole_pane_damage() {
    let surface = PixelRect { x: 0, y: 0, w: 240, h: 160 };
    let mut overlay_before = facts(false);
    overlay_before.window.overlay_active = true;
    overlay_before.window.palette_hash = 1;
    let mut overlay_after = overlay_before.clone();
    overlay_after.window.tab_hash = 1;
    let plan = transition(overlay_before, overlay_after, pane(7, 1));
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, surface);

    let mut degraded_after = facts(true);
    degraded_after.window.tab_hash = 1;
    let plan = transition(facts(true), degraded_after, pane(7, 1));
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, surface);

    // A class change whose area is not drawn (the tab bar is hidden) still repaints the whole
    // surface on the degraded path; the hardware path draws nothing new and plans the A7 Noop.
    let hidden_bar = |degraded| FrameFacts { tab_bar_top: None, ..facts(degraded) };
    let mut degraded_retitled = hidden_bar(true);
    degraded_retitled.window.tab_hash = 1;
    let plan = transition(hidden_bar(true), degraded_retitled, pane(7, 1));
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, surface);
    let mut hardware_retitled = hidden_bar(false);
    hardware_retitled.window.tab_hash = 1;
    let plan = transition(hidden_bar(false), hardware_retitled, pane(7, 1));
    assert_eq!(plan.mode, RenderMode::Noop);
    assert_eq!(plan.acknowledged_rows(0, 7, 1), None);

    let alternate = PaneMetadata { is_alt: true, ..live_pane(7, 1) };
    let mut toggled = cursor_facts();
    toggled.window.cursor_visible = false;
    toggled.window.cursor_cell = None;
    let plan = transition(cursor_facts(), toggled, alternate);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 0, w: 100, h: 84 });

    let first = FramePlan::build(cursor_facts(), [live_pane(7, 1)], None);
    assert_eq!(first.damage, surface);
    // A first frame that also carries dirty rows repaints the whole surface, not only those rows.
    let dirty_first = PaneMetadata { dirty_rows: vec![1], ..live_pane(7, 1) };
    let first = FramePlan::build(cursor_facts(), [dirty_first], None);
    assert_eq!(first.mode, RenderMode::Full);
    assert_eq!(first.damage, surface);
}

/// Every window field is classified: each narrow field sets exactly its class, and every other
/// field (but hover, which has its own row path) is `full`.
#[test]
fn each_window_field_has_exactly_its_class() {
    let narrow = |class: fn(&mut ChangeClass)| {
        let mut expected = ChangeClass::default();
        class(&mut expected);
        expected
    };
    let full = ChangeClass { full: true, ..ChangeClass::default() };
    let cases: Vec<(fn(&mut WindowIdentity), ChangeClass)> = vec![
        (
            |window| window.selection = Some(Selection::new(0, 0)),
            narrow(|class| class.selection = true),
        ),
        (|window| window.cursor_visible = true, narrow(|class| class.cursor = true)),
        (|window| window.cursor_shape = 1, narrow(|class| class.cursor = true)),
        (|window| window.cursor_blink = true, narrow(|class| class.cursor = true)),
        (|window| window.cursor_cell = cursor_at(0), narrow(|class| class.cursor = true)),
        (|window| window.window_focused = true, narrow(|class| class.focus = true)),
        (|window| window.tab_hash = 1, narrow(|class| class.tab_band = true)),
        (|window| window.hover_tab = 1, narrow(|class| class.tab_band = true)),
        (|window| window.close_override = 1, narrow(|class| class.tab_band = true)),
        (|window| window.process_privileged = true, narrow(|class| class.tab_band = true)),
        (
            |window| window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false),
            ChangeClass::default(),
        ),
        (
            |window| {
                window.copy_mode = Some(CopyModeIdentity::from(&CopyModeState::new_at((0, 0))))
            },
            full,
        ),
        (|window| window.quick_select_hint_count = 1, full),
        (|window| window.tab = 3, full),
        (|window| window.search_hash = 1, full),
        (|window| window.palette_hash = 1, full),
        (|window| window.ime_hash = 1, full),
        (|window| window.notification_hash = 1, full),
        (|window| window.width += 1, full),
        (|window| window.height += 1, full),
        (|window| window.viewport_top_abs = Some(9), full),
        (|window| window.pane_focus_flash_bucket = 1, full),
        (|window| window.broadcast_participants_hash = 1, full),
        (|window| window.inline_media_hash = 1, full),
        (|window| window.subpixel_aa = SubpixelAaMode::Rgb, full),
        (|window| window.background[3] = 1, full),
        (|window| window.style_rev = 1, full),
        (|window| window.renderer_hash = 1, full),
        (|window| window.overlay_active = true, full),
    ];
    let baseline = facts(false).window;
    for (index, (change, expected)) in cases.into_iter().enumerate() {
        let mut changed = baseline.clone();
        change(&mut changed);
        assert_eq!(changed.classify(&baseline), expected, "case {index}");
    }
    assert_eq!(baseline.classify(&baseline), ChangeClass::default());
}

/// A pane's scrollbar bucket alone is the `scrollbar` class, revision and dirty generation are
/// dirt (no class), and every other pane field is `full`.
#[test]
fn each_pane_field_has_exactly_its_class() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None).key.panes[0];
    let full = ChangeClass { full: true, ..ChangeClass::default() };
    let cases: Vec<(fn(&mut PaneIdentity), ChangeClass)> = vec![
        (
            |pane| pane.scrollbar_bucket = 9,
            ChangeClass { scrollbar: true, ..ChangeClass::default() },
        ),
        (|pane| pane.revision += 1, ChangeClass::default()),
        (|pane| pane.dirty_generation += 1, ChangeClass::default()),
        (|pane| pane.id += 1, full),
        (|pane| pane.rect.x += 1, full),
        (|pane| pane.cols += 1, full),
        (|pane| pane.rows += 1, full),
        (|pane| pane.scrollback_len += 1, full),
        (|pane| pane.viewport_top_abs = None, full),
        (|pane| pane.view_top_abs += 1, full),
        (|pane| pane.is_active = false, full),
        (|pane| pane.is_alt = true, full),
    ];
    for (index, (change, expected)) in cases.into_iter().enumerate() {
        let mut changed = baseline;
        change(&mut changed);
        assert_eq!(changed.classify(&baseline), expected, "case {index}");
    }
}

/// Compile-time exhaustiveness: these destructures name every identity field without `..`, so a
/// new field fails to compile here until it is classified.
#[test]
fn identity_destructures_name_every_field() {
    let WindowIdentity {
        selection: _,
        copy_mode: _,
        quick_select_hint_count: _,
        cursor_visible: _,
        tab: _,
        search_hash: _,
        palette_hash: _,
        ime_hash: _,
        notification_hash: _,
        width: _,
        height: _,
        tab_hash: _,
        viewport_top_abs: _,
        cursor_shape: _,
        cursor_blink: _,
        window_focused: _,
        pane_focus_flash_bucket: _,
        hover_tab: _,
        close_override: _,
        broadcast_participants_hash: _,
        inline_media_hash: _,
        hovered_url_cells: _,
        process_privileged: _,
        subpixel_aa: _,
        background: _,
        style_rev: _,
        renderer_hash: _,
        overlay_active: _,
        cursor_cell: _,
    } = WindowIdentity::default();
    let PaneIdentity {
        id: _,
        revision: _,
        dirty_generation: _,
        rect: _,
        cols: _,
        rows: _,
        scrollback_len: _,
        viewport_top_abs: _,
        view_top_abs: _,
        is_active: _,
        is_alt: _,
        scrollbar_bucket: _,
    } = FramePlan::build(facts(false), [pane(7, 1)], None).key.panes[0];
}

/// Source scan: both `classify` bodies destructure without a rest pattern, so the compiler, not
/// a reviewer, rejects an unclassified field. Line endings are normalized before scanning.
#[test]
fn classify_bodies_use_no_rest_pattern() {
    let source = include_str!("frame_plan.rs").replace("\r\n", "\n");
    let mut bodies = 0;
    for (offset, _) in source.match_indices("fn classify(") {
        let open = offset + source[offset..].find('{').expect("a body");
        let mut depth = 0usize;
        let mut close = open;
        for (index, character) in source[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + index;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &source[open..=close];
        assert!(!body.contains(".."), "a classify body uses `..`:\n{body}");
        bodies += 1;
    }
    assert_eq!(bodies, 2, "WindowIdentity::classify and PaneIdentity::classify");
}

/// Facts for a 24-row live pane with a visible, focused cursor at slot 5 (y=102..122).
fn tall_cursor_facts() -> FrameFacts {
    let mut cursor = tall_facts(false);
    cursor.tab_bar_top = None;
    cursor.window.cursor_visible = true;
    cursor.window.window_focused = true;
    cursor.window.cursor_cell = cursor_at(5);
    cursor
}

/// The 24-row live pane `tall_cursor_facts` draws its cursor in.
fn tall_live_pane() -> PaneMetadata {
    PaneMetadata { viewport_top_abs: None, ..scrolled_back_pane() }
}

/// A recolor record over `rect`.
fn recolored(rect: PixelRect, hash: u64) -> RecolorRecord {
    RecolorRecord { bounds: RecolorBounds::Rect(rect), hash }
}

/// The tall glyph a block cursor at slot 5 recolors, reaching y=72 above the cursor row.
const TALL_GLYPH: PixelRect = PixelRect { x: 50, y: 72, w: 10, h: 40 };
/// A glyph exactly under the slot-5 cursor cell.
const SHORT_GLYPH: PixelRect = PixelRect { x: 50, y: 102, w: 10, h: 20 };

/// Recolor damage in the planner: a cursor toggle restores the previous record's tall glyph
/// (y=72); an `Unbounded` record damages the active pane's clip; without a cursor or focus class
/// an unchanged record adds nothing; a changed record adds both old and new bounds, including
/// `Rect` to `Empty` (the old) and `Empty` to `Rect` (the new).
#[test]
fn recolor_bounds_widen_cursor_and_changed_record_damage() {
    let mut toggled = tall_cursor_facts();
    toggled.window.cursor_visible = false;
    toggled.window.cursor_cell = None;
    toggled.previous_recolor = recolored(TALL_GLYPH, 1);
    let plan = transition(tall_cursor_facts(), toggled.clone(), tall_live_pane());
    assert!(plan.damage.y <= 72 && plan.damage.bottom() >= 122, "{:?}", plan.damage);

    toggled.previous_recolor = RecolorRecord { bounds: RecolorBounds::Unbounded, hash: 1 };
    let plan = transition(tall_cursor_facts(), toggled, tall_live_pane());
    assert_eq!(Some(plan.damage), plan.panes[0].full_clip);

    // A dirty row elsewhere, no cursor or focus class: only a changed record widens.
    let dirty_frame = || {
        let first = FramePlan::build(tall_cursor_facts(), [tall_live_pane()], None);
        let mut edited = tall_live_pane();
        edited.revision = 2;
        edited.dirty_rows = vec![15];
        FramePlan::build(tall_cursor_facts(), [edited], Some(&first.key))
    };
    let row_damage = dirty_frame().damage;
    assert!(!dirty_frame().change.any_class());

    let mut unchanged = dirty_frame();
    unchanged.widen_for_recolor(recolored(TALL_GLYPH, 1), recolored(TALL_GLYPH, 1));
    assert_eq!(unchanged.damage, row_damage, "an unchanged record adds nothing");

    let mut shrunk = dirty_frame();
    shrunk.widen_for_recolor(recolored(TALL_GLYPH, 1), recolored(SHORT_GLYPH, 2));
    assert_eq!(shrunk.damage, row_damage.union(TALL_GLYPH).union(SHORT_GLYPH));
    assert!(shrunk.damage_parts.contains(&TALL_GLYPH));

    let mut removed = dirty_frame();
    removed.widen_for_recolor(recolored(TALL_GLYPH, 1), RecolorRecord::default());
    assert_eq!(removed.damage, row_damage.union(TALL_GLYPH));

    let mut appeared = dirty_frame();
    appeared.widen_for_recolor(RecolorRecord::default(), recolored(TALL_GLYPH, 1));
    assert_eq!(appeared.damage, row_damage.union(TALL_GLYPH));

    // With the cursor class set, the new bounds join the damage even when the record is unchanged.
    let mut shape = tall_cursor_facts();
    shape.window.cursor_shape = 1;
    shape.previous_recolor = recolored(TALL_GLYPH, 1);
    let mut plan = transition(tall_cursor_facts(), shape, tall_live_pane());
    plan.widen_for_recolor(recolored(TALL_GLYPH, 1), recolored(TALL_GLYPH, 1));
    assert!(plan.damage.y <= 72);
}

/// Coverage contract per mode: a Full plan emits every visible row; a Partial plan emits every
/// row whose valid record meets the previous recolor bounds, so the tall glyph a block cursor
/// recolored last frame is in the batches and widening by it needs no fallback; an unchanged
/// key is a Noop that emits nothing.
#[test]
fn every_presenting_mode_emits_the_rows_its_ink_can_reach() {
    let mut toggled = tall_cursor_facts();
    toggled.window.cursor_shape = 1;
    toggled.previous_recolor = recolored(CURSOR_TALL_RECORD, 1);
    let full = transition(tall_cursor_facts(), toggled.clone(), tall_live_pane());
    assert_eq!(full.mode, RenderMode::Full, "unseeded records cannot be narrowed");
    assert!(full.panes[0].emit_rows.iter().all(|emit| *emit));

    let first = FramePlan::build(tall_cursor_facts(), [tall_live_pane()], None);
    let recorded = tall_glyph_records(&tall_cursor_facts(), &first);
    let mut plan = FramePlan::build(toggled.clone(), [recorded.clone()], Some(&first.key));
    assert_eq!(plan.mode, RenderMode::Partial);
    // Slots 3 to 5 hold the recolored tall glyph's ink; slot 6 does not.
    assert_eq!(plan.panes[0].emit_rows[3..7], [true, true, true, false]);
    plan.widen_for_recolor(recolored(CURSOR_TALL_RECORD, 1), recolored(CURSOR_TALL_RECORD, 1));
    assert!(plan.damage.y <= 72);
    assert!(!plan.partial_reaches_unemitted_ink(), "every row the widening reaches was emitted");

    let noop = FramePlan::build(toggled, [recorded], Some(&plan.key));
    assert!(noop.unchanged);
    assert!(noop.panes[0].emit_rows.iter().all(|emit| !*emit));
}

/// While an overlay is active on either side, any change to the frame key repaints the whole
/// surface on both paths, not only a class change: an overlay such as a preedit draws at the
/// live cursor, which the key omits, so a revision-only, dirty-generation-only or hover-only
/// change can move it. An unchanged key still skips.
#[test]
fn any_key_change_under_an_active_overlay_damages_the_whole_surface() {
    let surface = PixelRect { x: 0, y: 0, w: 240, h: 160 };
    for degraded in [false, true] {
        let mut composing = facts(degraded);
        composing.window.ime_hash = 0xFEED;
        composing.window.overlay_active = true;
        let first = FramePlan::build(composing.clone(), [live_pane(7, 1)], None);

        let revised = PaneMetadata { dirty_rows: vec![1], ..live_pane(7, 2) };
        let plan = FramePlan::build(composing.clone(), [revised], Some(&first.key));
        assert_eq!((plan.mode, plan.damage), (RenderMode::Full, surface), "revision, {degraded}");

        let marked = PaneMetadata { dirty_generation: 1, dirty_rows: vec![1], ..live_pane(7, 1) };
        let plan = FramePlan::build(composing.clone(), [marked], Some(&first.key));
        assert_eq!((plan.mode, plan.damage), (RenderMode::Full, surface), "generation, {degraded}");

        let mut hovering = composing.clone();
        hovering.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
        let plan = FramePlan::build(hovering, [live_pane(7, 1)], Some(&first.key));
        assert_eq!((plan.mode, plan.damage), (RenderMode::Full, surface), "hover, {degraded}");

        let plan = FramePlan::build(composing, [live_pane(7, 1)], Some(&first.key));
        assert!(plan.unchanged);
        assert_eq!(plan.mode, RenderMode::Noop, "unchanged, {degraded}");
    }
}

/// Building dirty-row damage allocates nothing per dirty slot: each primary slot is damaged
/// from a one-element iterator, not a collected `Vec`, so a plan with counters off pays no
/// per-row allocation for the waste parts. Line endings are normalized before scanning.
#[test]
fn dirty_slot_damage_allocates_no_vector_per_slot() {
    let source = include_str!("frame_plan.rs").replace("\r\n", "\n");
    let build = source.split_once("    pub(crate) fn build(").expect("FramePlan::build exists").1;
    let build = &build[..build.find("\n    }\n").expect("build ends")];
    assert!(!build.contains("vec![slot]"), "a Vec is built per dirty slot");
    assert!(!build.contains("Vec<Vec<u16>>"), "dirty slots are regrouped into vectors");
}

/// Tab-title ink can reach above the padded tab band: a title glyph drawn at y=100..180 over a
/// band starting at y=140. Each case passes only through the widening it names: a focus change
/// with no drawn cursor and identical tall ink, a tab color change (it reaches the key through
/// `tab_hash`) with identical tall ink, and changed ink under a dirty-row frame with no class,
/// whose damage is exactly the row strip and both sides' ink. Unchanged ink with no class adds
/// nothing, and unknown ink damages the whole surface.
#[test]
fn tab_title_ink_above_the_band_is_damaged_on_a_tab_band_or_focus_change() {
    let surface = PixelRect { x: 0, y: 0, w: 240, h: 160 };
    let tall = RecolorBounds::Rect(PixelRect { x: 20, y: 100, w: 10, h: 80 });
    let short_rect = PixelRect { x: 20, y: 142, w: 10, h: 12 };
    let short = RecolorBounds::Rect(short_rect);
    // The tall ink clipped to the 160 px surface, unioned with the full-width band.
    let band_with_tall = PixelRect { x: 0, y: 100, w: 240, h: 60 };

    // Focus with no drawn cursor: the band alone is planned, and only the focus widening adds
    // the unchanged tall ink above it.
    let focused = FrameFacts {
        window: WindowIdentity { window_focused: true, ..facts(false).window },
        ..facts(false)
    };
    let unfocused = FrameFacts {
        window: WindowIdentity { window_focused: false, ..focused.window.clone() },
        ..focused.clone()
    };
    let mut plan = transition(focused, unfocused, live_pane(7, 1));
    assert!(plan.change.focus && !plan.change.cursor);
    assert_eq!(plan.damage, TAB_BAND);
    plan.widen_for_tab_ink(tall, tall);
    assert_eq!(plan.damage, band_with_tall, "focus repaints the unchanged title ink");

    // A tab color change keeps the title's ink where it was; the tab-band widening repaints it.
    let colored = |tab_hash| FrameFacts {
        window: WindowIdentity { tab_hash, ..facts(false).window },
        ..facts(false)
    };
    let mut plan = transition(colored(1), colored(2), live_pane(7, 1));
    assert!(plan.change.tab_band);
    assert_eq!(plan.damage, TAB_BAND);
    plan.widen_for_tab_ink(tall, tall);
    assert_eq!(plan.damage, band_with_tall, "a recolored title repaints its unchanged ink");

    // Changed ink with no class: a revision with a dirty row whose strip misses the overhang.
    let first = FramePlan::build(facts(false), [live_pane(7, 1)], None);
    let dirty_frame = || {
        let dirty = PaneMetadata { dirty_rows: vec![3], ..live_pane(7, 2) };
        FramePlan::build(facts(false), [dirty], Some(&first.key))
    };
    let mut unchanged_ink = dirty_frame();
    assert_eq!(unchanged_ink.mode, RenderMode::Full);
    assert_eq!(unchanged_ink.change, ChangeClass::default(), "no narrow or full class is set");
    assert_eq!(unchanged_ink.damage, row_rect(3));
    unchanged_ink.widen_for_tab_ink(short, short);
    assert_eq!(unchanged_ink.damage, row_rect(3), "unchanged ink with no class adds nothing");
    let mut plan = dirty_frame();
    plan.widen_for_tab_ink(tall, short);
    let tall_clipped = PixelRect { x: 20, y: 100, w: 10, h: 60 };
    assert_eq!(plan.damage, row_rect(3).union(tall_clipped).union(short_rect));

    let mut plan = transition(colored(1), colored(2), live_pane(7, 1));
    plan.widen_for_tab_ink(RecolorBounds::Unbounded, short);
    assert_eq!(plan.damage, surface);
}

/// The tall glyph of slot 3 of the 24-row cursor pane, reaching from y=72 into the slot-5 cursor
/// cell at x=22..32; its padded strip (y=62..82) misses that row.
const CURSOR_TALL_RECORD: PixelRect = PixelRect { x: 22, y: 72, w: 10, h: 40 };

/// The 24-row cursor pane seeded from `prior`, with slot 3 holding `CURSOR_TALL_RECORD`.
fn tall_glyph_records(frame_facts: &FrameFacts, prior: &FramePlan) -> PaneMetadata {
    seeded_with(frame_facts, prior, tall_live_pane(), |slot, strip| {
        Some(if slot == 3 { CURSOR_TALL_RECORD } else { strip })
    })
}

/// Facts for a 10-row live pane with room on the surface for every slot and no tab bar.
fn ten_row_facts(vertical_ink_pad: f32) -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 240, ..Default::default() },
        vertical_ink_pad,
        tab_bar_top: None,
        ..facts(false)
    }
}

/// A live 10-row pane, 100 px wide, with every slot on the surface of `ten_row_facts`.
fn ten_row_pane() -> PaneMetadata {
    PaneMetadata { rect: PixelRect { x: 0, y: 0, w: 100, h: 204 }, rows: 10, ..live_pane(7, 1) }
}

/// Present `input` under `frame_facts`, then plan an edit of its live rows `dirty` against that
/// frame with complete seeded records.
fn seeded_edit(frame_facts: &FrameFacts, input: PaneMetadata, dirty: &[usize]) -> FramePlan {
    let first = FramePlan::build(frame_facts.clone(), [input.clone()], None);
    let mut edited = seeded(frame_facts, &first, input);
    edited.revision += 1;
    edited.dirty_rows = dirty.to_vec();
    FramePlan::build(frame_facts.clone(), [edited], Some(&first.key))
}

/// The one-row edit on a 10-row pane with a one-cell ink pad.
fn ten_row_edit(degraded: bool) -> FramePlan {
    seeded_edit(&FrameFacts { degraded, ..ten_row_facts(20.0) }, ten_row_pane(), &[5])
}

/// Two side-by-side 4-row panes: pane 7's unchanged row 1 overhangs to x=108, and pane 9 edits
/// its row 1, damaging x>=100 on that row.
fn neighbour_edit(degraded: bool) -> FramePlan {
    let frame_facts = FrameFacts { degraded, ..facts(false) };
    let right = PaneMetadata {
        rect: PixelRect { x: 100, y: 0, w: 100, h: 84 },
        is_active: false,
        ..live_pane(9, 1)
    };
    let first = FramePlan::build(frame_facts.clone(), [live_pane(7, 1), right.clone()], None);
    let left = seeded_with(&frame_facts, &first, live_pane(7, 1), |slot, strip| {
        Some(if slot == 1 { PixelRect { x: 96, y: 22, w: 12, h: 20 } } else { strip })
    });
    let mut right = seeded(&frame_facts, &first, right);
    right.revision = 2;
    right.dirty_rows = vec![1];
    FramePlan::build(frame_facts, [left, right], Some(&first.key))
}

/// The 24-row cursor pane edits row 15 far from its unmoved slot-5 cursor; slot 3's record holds
/// a tall glyph reaching into the cursor cell, and the last frame recolored a glyph in slot 12.
fn cursor_reach_edit(degraded: bool) -> FramePlan {
    let frame_facts = FrameFacts {
        degraded,
        previous_recolor: recolored(PixelRect { x: 22, y: 250, w: 5, h: 5 }, 1),
        ..tall_cursor_facts()
    };
    let first = FramePlan::build(frame_facts.clone(), [tall_live_pane()], None);
    let mut edited = tall_glyph_records(&frame_facts, &first);
    edited.revision = 2;
    edited.dirty_rows = vec![15];
    FramePlan::build(frame_facts, [edited], Some(&first.key))
}

/// The indices of the emitted rows of `plan`'s pane `index`.
fn emitted(plan: &FramePlan, index: usize) -> Vec<usize> {
    plan.panes[index]
        .emit_rows
        .iter()
        .enumerate()
        .filter(|(_, emit)| **emit)
        .map(|(row, _)| row)
        .collect()
}

/// A one-row hardware edit narrower than the surface is `Partial` and emits only the rows whose
/// padded strip (here also their record) meets its damage: dirty row 1 of a 4-row pane reaches
/// every row, and dirty row 5 of a 10-row pane, padded one cell, reaches rows 3 to 7.
#[test]
fn a_one_row_edit_emits_only_rows_meeting_its_padded_damage() {
    let four =
        seeded_edit(&FrameFacts { vertical_ink_pad: 20.0, ..facts(false) }, live_pane(7, 1), &[1]);
    assert_eq!(four.mode, RenderMode::Partial);
    assert_eq!(emitted(&four, 0), [0, 1, 2, 3]);

    let ten = ten_row_edit(false);
    assert_eq!(ten.mode, RenderMode::Partial);
    assert_eq!(ten.damage, PixelRect { x: 0, y: 82, w: 100, h: 60 });
    assert_eq!(emitted(&ten, 0), [3, 4, 5, 6, 7]);
}

/// Every case the planner cannot narrow stays `Full` and emits every row: the first frame (a
/// cleared key), the surface size, metrics, padding, the scrollbar mode, every full-class
/// window field, an overlay before or after, a pane-count change and a pane projection change.
#[test]
fn every_case_the_planner_cannot_narrow_stays_full() {
    let base = ten_row_facts(0.0);
    let first = FramePlan::build(base.clone(), [ten_row_pane()], None);
    let edited = || {
        let mut edited = seeded(&base, &first, ten_row_pane());
        edited.revision = 2;
        edited.dirty_rows = vec![5];
        edited
    };
    let assert_full = |plan: FramePlan, label: &str| {
        assert_eq!(plan.mode, RenderMode::Full, "{label}");
        assert!(plan.panes.iter().all(|pane| pane.emit_rows.iter().all(|emit| *emit)), "{label}");
    };
    assert_eq!(
        FramePlan::build(base.clone(), [edited()], Some(&first.key)).mode,
        RenderMode::Partial,
        "the unchanged-facts edit narrows, so each case below is what forces Full"
    );
    assert_full(FramePlan::build(base.clone(), [edited()], None), "first frame or cleared key");

    let changes: [(&str, fn(&mut FrameFacts)); 22] = [
        ("width", |changed| changed.window.width += 1),
        ("height", |changed| changed.window.height += 1),
        ("cell width", |changed| changed.cell_w = 11.0),
        ("cell height", |changed| changed.cell_h = 19.0),
        ("ink pad", |changed| changed.vertical_ink_pad = 1.0),
        ("padding", |changed| changed.padding = [3.0; 4]),
        ("scrollbar mode", |changed| changed.scrollbar_mode = ScrollbarMode::Always),
        ("quick-select hints", |changed| changed.window.quick_select_hint_count = 1),
        ("active tab", |changed| changed.window.tab = 1),
        ("search", |changed| changed.window.search_hash = 1),
        ("palette", |changed| changed.window.palette_hash = 1),
        ("ime", |changed| changed.window.ime_hash = 1),
        ("notification", |changed| changed.window.notification_hash = 1),
        ("window viewport", |changed| changed.window.viewport_top_abs = Some(0)),
        ("focus flash", |changed| changed.window.pane_focus_flash_bucket = 1),
        ("broadcast", |changed| changed.window.broadcast_participants_hash = 1),
        ("inline media", |changed| changed.window.inline_media_hash = 1),
        ("subpixel", |changed| changed.window.subpixel_aa = SubpixelAaMode::Rgb),
        ("background", |changed| changed.window.background = [1, 0, 0, 0]),
        ("style", |changed| changed.window.style_rev = 1),
        ("renderer", |changed| changed.window.renderer_hash = 1),
        ("overlay after", |changed| changed.window.overlay_active = true),
    ];
    for (label, change) in changes {
        let mut changed = base.clone();
        change(&mut changed);
        assert_full(FramePlan::build(changed, [edited()], Some(&first.key)), label);
    }

    let overlay = FrameFacts {
        window: WindowIdentity { overlay_active: true, ..base.window.clone() },
        ..base.clone()
    };
    let shown = FramePlan::build(overlay, [ten_row_pane()], None);
    assert_full(FramePlan::build(base.clone(), [edited()], Some(&shown.key)), "overlay before");

    let other = PaneMetadata {
        rect: PixelRect { x: 100, y: 0, w: 100, h: 204 },
        is_active: false,
        ..ten_row_pane()
    };
    let other = PaneMetadata { id: 9, ..other };
    assert_full(FramePlan::build(base.clone(), [edited(), other], Some(&first.key)), "pane count");
    let scrolled = PaneMetadata { viewport_top_abs: Some(10), ..edited() };
    assert_full(FramePlan::build(base.clone(), [scrolled], Some(&first.key)), "projection");
    let moved = PaneMetadata { rect: PixelRect { w: 90, ..ten_row_pane().rect }, ..edited() };
    assert_full(FramePlan::build(base, [moved], Some(&first.key)), "pane rectangle");
}

/// The degraded path is never `Partial`: every input that narrows on the hardware path plans a
/// `Full` frame with surface damage that emits every row.
#[test]
fn degraded_plans_are_never_partial() {
    for (label, hardware, degraded) in [
        ("one-row edit", ten_row_edit(false), ten_row_edit(true)),
        ("neighbour overhang", neighbour_edit(false), neighbour_edit(true)),
        ("cursor reach", cursor_reach_edit(false), cursor_reach_edit(true)),
    ] {
        assert_eq!(hardware.mode, RenderMode::Partial, "{label} narrows on hardware");
        assert_eq!(degraded.mode, RenderMode::Full, "{label}");
        assert_eq!(degraded.damage, degraded.surface, "{label}");
        assert!(degraded.panes.iter().all(|pane| pane.emit_rows.iter().all(|emit| *emit)));
    }
}

/// A presented plan acknowledges per pane: `Full` every row, `Noop` nothing, and `Partial` the
/// dirty live rows whose slot it emitted. A dirty slot whose padded strip lies wholly below the
/// surface is still emitted and acknowledged; a dirty live row below a scrolled view has no slot
/// and keeps its bit.
#[test]
fn partial_plans_acknowledge_exactly_the_emitted_dirty_live_rows() {
    let ten = ten_row_edit(false);
    assert_eq!(ten.acknowledged_rows(0, 7, 2), Some(rows_ack([5])));
    assert_eq!(ten.acknowledged_rows(0, 7, 1), None, "another revision");
    assert_eq!(ten.acknowledged_rows(0, 9, 2), None, "another pane");
    assert_eq!(ten.acknowledged_rows(1, 7, 2), None, "another index");
    let first = FramePlan::build(ten_row_facts(0.0), [ten_row_pane()], None);
    assert_eq!(first.acknowledged_rows(0, 7, 1), Some(sonicterm_render_model::AckRows::All));
    let unchanged = FramePlan::build(ten_row_facts(0.0), [ten_row_pane()], Some(&first.key));
    assert_eq!(unchanged.acknowledged_rows(0, 7, 1), None);

    // A 10-row pane on a 160 px surface: slots 8 and 9 have no pixels.
    let short = FrameFacts { tab_bar_top: None, ..facts(false) };
    let clipped = seeded_edit(&short, ten_row_pane(), &[2, 8]);
    assert_eq!(clipped.mode, RenderMode::Partial);
    assert_eq!(clipped.damage, PixelRect { x: 0, y: 42, w: 100, h: 20 });
    assert_eq!(emitted(&clipped, 0), [2, 8]);
    assert_eq!(clipped.acknowledged_rows(0, 7, 2), Some(rows_ack([2, 8])));

    // Scrolled back three rows: live rows 0, 5 and 9 are slots 3, 8 and 12, and slot 12 is below
    // the 10-row view.
    let scrolled =
        PaneMetadata { scrollback_len: 100, viewport_top_abs: Some(97), ..ten_row_pane() };
    let offscreen = seeded_edit(&short, scrolled, &[0, 5, 9]);
    assert_eq!(offscreen.panes[0].dirty_slots, [3, 8]);
    assert_eq!(offscreen.mode, RenderMode::Partial);
    assert_eq!(offscreen.acknowledged_rows(0, 7, 2), Some(rows_ack([0, 5])));
}

/// Facts for the 16-row scrolled-view fixtures: room for two 324 px panes, no tab bar.
fn sixteen_row_facts() -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 400, ..Default::default() },
        tab_bar_top: None,
        ..facts(false)
    }
}

/// A 16-row pane with 100 history rows, at `x`, viewing from `view_top`.
fn sixteen_row_pane(id: u64, x: i32, view_top: Option<u64>) -> PaneMetadata {
    PaneMetadata {
        rect: PixelRect { x, y: 0, w: 100, h: 324 },
        rows: 16,
        scrollback_len: 100,
        viewport_top_abs: view_top,
        is_active: id == 7,
        ..pane(id, 1)
    }
}

/// Plan an edit of `dirty` on each `(pane, dirty)` input against a presented first frame.
fn seeded_edits(inputs: Vec<(PaneMetadata, Vec<usize>)>) -> FramePlan {
    let frame_facts = sixteen_row_facts();
    let first =
        FramePlan::build(frame_facts.clone(), inputs.iter().map(|(input, _)| input.clone()), None);
    let edited: Vec<_> = inputs
        .into_iter()
        .map(|(input, dirty)| {
            let mut edited = seeded(&frame_facts, &first, input);
            edited.revision += 1;
            edited.dirty_rows = dirty;
            edited
        })
        .collect();
    FramePlan::build(frame_facts, edited, Some(&first.key))
}

/// Scrolled views acknowledge live rows through their slots. With 16 rows and 100 history rows,
/// view top 90 puts live row 15 at slot 25, offscreen: alone it is a `Noop` with no receipt; in a
/// frame another pane's edit makes `Partial`, the scrolled pane emits only rows meeting the damage
/// and acknowledges `Rows(empty)`. View top 97 puts live rows 5 and 15 at slots 8 and 18: only
/// live row 5 is acknowledged, and a live pane in the same frame acknowledges its own slot.
#[test]
fn scrolled_views_acknowledge_live_rows_through_their_slots() {
    assert_eq!(live_row_to_slot(100, 90, 16, 15), None, "slot 25 is past a 16-row view");
    assert_eq!(live_row_to_slot(100, 97, 16, 5), Some(8));
    assert_eq!(live_row_to_slot(100, 97, 16, 15), None, "slot 18 is past a 16-row view");

    let alone = seeded_edits(vec![(sixteen_row_pane(7, 0, Some(90)), vec![15])]);
    assert_eq!((alone.panes[0].row_count, alone.panes[0].scrollback_len), (16, 100));
    assert_eq!(alone.mode, RenderMode::Noop);
    assert_eq!(alone.acknowledged_rows(0, 7, 2), None);
    assert!(alone.panes[0].emit_rows.iter().all(|emit| !*emit));

    let beside = seeded_edits(vec![
        (sixteen_row_pane(7, 0, Some(90)), vec![15]),
        (sixteen_row_pane(9, 100, None), vec![2]),
    ]);
    assert_eq!(beside.mode, RenderMode::Partial);
    assert!(emitted(&beside, 0).is_empty(), "the scrolled pane's rows only touch the damage");
    assert_eq!(beside.acknowledged_rows(0, 7, 2), Some(rows_ack([])));
    assert_eq!(beside.acknowledged_rows(1, 9, 2), Some(rows_ack([2])));

    let near = seeded_edits(vec![
        (sixteen_row_pane(7, 0, Some(97)), vec![5, 15]),
        (sixteen_row_pane(9, 100, None), vec![2]),
    ]);
    assert_eq!(near.mode, RenderMode::Partial);
    assert_eq!(near.panes[0].dirty_slots, [8]);
    assert!(near.panes[0].emit_rows[8]);
    assert_eq!(near.acknowledged_rows(0, 7, 2), Some(rows_ack([5])));
    assert_eq!(near.acknowledged_rows(1, 9, 2), Some(rows_ack([2])));
}

/// The two dirt consumers stay split: emission and receipts use slots (live row 5 is slot 8) and
/// live rows, while invalidation drops absolute row `scrollback_len + live_row` (105). Dirt only
/// below the view is a `Noop` that issues no receipt.
#[test]
fn emission_receipts_and_invalidation_read_their_own_dirt() {
    let plan = seeded_edits(vec![(sixteen_row_pane(7, 0, Some(97)), vec![5])]);
    assert_eq!(plan.mode, RenderMode::Partial);
    assert!(plan.panes[0].emit_rows[8]);
    assert_eq!(plan.acknowledged_rows(0, 7, 2), Some(rows_ack([5])));
    let pane = &plan.panes[0];
    let invalidated: Vec<u64> =
        pane.dirty_live_rows.iter().map(|row| pane.scrollback_len + *row as u64).collect();
    assert_eq!(invalidated, [105]);

    let offscreen = seeded_edits(vec![(sixteen_row_pane(7, 0, Some(97)), vec![15])]);
    assert_eq!(offscreen.mode, RenderMode::Noop);
    assert_eq!(offscreen.acknowledged_rows(0, 7, 2), None);
}

/// Neighbouring and vertical overhang: an unchanged row of another pane whose record reaches the
/// damage is emitted though its strip only touches it, and an unchanged row whose tall record
/// reaches an edit three rows below is emitted while the rows between are not.
#[test]
fn rows_whose_records_overhang_the_damage_are_emitted() {
    let neighbour = neighbour_edit(false);
    assert_eq!(neighbour.mode, RenderMode::Partial);
    assert_eq!(neighbour.damage, PixelRect { x: 100, y: 22, w: 100, h: 20 });
    assert_eq!(emitted(&neighbour, 0), [1], "pane 7 row 1 overhangs to x=108");
    assert_eq!(emitted(&neighbour, 1), [1]);

    let frame_facts = ten_row_facts(0.0);
    let first = FramePlan::build(frame_facts.clone(), [ten_row_pane()], None);
    let mut edited = seeded_with(&frame_facts, &first, ten_row_pane(), |slot, strip| {
        Some(if slot == 3 { PixelRect { x: 22, y: 62, w: 10, h: 80 } } else { strip })
    });
    edited.revision = 2;
    edited.dirty_rows = vec![6];
    let plan = FramePlan::build(frame_facts, [edited], Some(&first.key));
    assert_eq!(plan.mode, RenderMode::Partial);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 122, w: 100, h: 20 });
    assert_eq!(emitted(&plan, 0), [3, 6]);
}

/// Stale records are never trusted. A dirty slot is emitted whatever its record says, even an
/// empty or missing one, and even when its strip lies below a 100 px surface; a non-emitted slot
/// whose record is missing (its content stamp or absolute row changed) forces `Full`.
#[test]
fn a_dirty_slot_is_emitted_whatever_its_record_and_a_stale_clean_slot_forces_full() {
    let frame_facts = FrameFacts {
        window: WindowIdentity { width: 240, height: 100, ..Default::default() },
        vertical_ink_pad: 12.0,
        tab_bar_top: None,
        ..facts(false)
    };
    let first = FramePlan::build(frame_facts.clone(), [ten_row_pane()], None);
    let edit = |record: fn(u16, PixelRect) -> Option<PixelRect>| {
        let mut edited = seeded_with(&frame_facts, &first, ten_row_pane(), record);
        edited.revision = 2;
        edited.dirty_rows = vec![4, 6];
        FramePlan::build(frame_facts.clone(), [edited], Some(&first.key))
    };
    let plan = edit(|_, strip| Some(strip));
    assert_eq!(plan.damage, PixelRect { x: 0, y: 70, w: 100, h: 30 });
    assert_eq!(plan.mode, RenderMode::Partial);
    assert!(plan.panes[0].emit_rows[6], "slot 6 is below the surface with an empty record");
    let missing_dirty = edit(|slot, strip| (slot != 4).then_some(strip));
    assert_eq!(missing_dirty.mode, RenderMode::Partial, "a dirty slot is emitted anyway");
    assert!(missing_dirty.panes[0].emit_rows[4]);
    let stale_clean = edit(|slot, strip| (slot != 0).then_some(strip));
    assert_eq!(stale_clean.mode, RenderMode::Full);
}

/// A presented frame keeps records only for panes with pixels on the surface, each up to its
/// row count; a pane moved off the surface keeps none, since its return is a full-class change.
#[test]
fn drawn_row_counts_name_only_panes_on_the_surface() {
    let off = PaneMetadata {
        rect: PixelRect { x: 300, y: 0, w: 100, h: 84 },
        is_active: false,
        ..live_pane(9, 1)
    };
    let plan = FramePlan::build(facts(false), [live_pane(7, 1), off], None);
    assert_eq!(plan.drawn_row_counts(), [(7, 4)]);
}

/// Unseeded inputs, as before any frame presented, carry no records and plan `Full`.
#[test]
fn a_pane_without_records_plans_full() {
    let frame_facts = ten_row_facts(0.0);
    let first = FramePlan::build(frame_facts.clone(), [ten_row_pane()], None);
    let mut edited = ten_row_pane();
    edited.revision = 2;
    edited.dirty_rows = vec![5];
    let plan = FramePlan::build(frame_facts, [edited], Some(&first.key));
    assert_eq!(plan.mode, RenderMode::Full);
    assert!(plan.panes[0].emit_rows.iter().all(|emit| *emit));
}

/// The drawn cursor cell and the previous recolor bounds reach rows the damage does not: an
/// unchanged row whose tall record meets the unmoved block cursor is emitted though its strip
/// misses the damage, and so is the row whose record meets the last frame's recolor bounds.
#[test]
fn rows_whose_records_meet_the_cursor_cell_or_previous_recolor_are_emitted() {
    let plan = cursor_reach_edit(false);
    assert_eq!(plan.mode, RenderMode::Partial);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 302, w: 100, h: 20 });
    assert_eq!(emitted(&plan, 0), [3, 5, 12, 15]);
}

/// After assembly the damage can still grow by the recolor and tab-title ink. A partial plan
/// whose final damage reaches a non-emitted row's record must be reassembled `Full`, which emits
/// every row and acknowledges every row; damage that stays on emitted rows needs no fallback.
#[test]
fn widened_damage_reaching_a_non_emitted_record_falls_back_to_full() {
    let plan = || seeded_edit(&ten_row_facts(0.0), ten_row_pane(), &[5]);
    assert_eq!(emitted(&plan(), 0), [5]);

    let mut on_emitted = plan();
    on_emitted.widen_for_recolor(
        RecolorRecord::default(),
        recolored(PixelRect { x: 22, y: 105, w: 10, h: 10 }, 1),
    );
    assert!(!on_emitted.partial_reaches_unemitted_ink());

    let mut recolor = plan();
    recolor.widen_for_recolor(
        RecolorRecord::default(),
        recolored(PixelRect { x: 22, y: 50, w: 10, h: 10 }, 1),
    );
    assert!(recolor.damage.y <= 50, "the recolor widens a partial plan's damage");
    assert!(recolor.partial_reaches_unemitted_ink());
    // Converting a partial plan counts the full frame it becomes; the fallback is counted by the
    // assembly orchestration, and a second call converts nothing.
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        recolor.force_full();
        recolor.force_full();
    }
    let stats = sink.snapshot();
    assert_eq!((stats.partial_fallbacks, stats.full_frames, stats.partial_frames), (0, 1, 0));
    assert_eq!(recolor.mode, RenderMode::Full);
    assert!(recolor.panes[0].emit_rows.iter().all(|emit| *emit));
    assert_eq!(recolor.acknowledged_rows(0, 7, 2), Some(sonicterm_render_model::AckRows::All));
    assert!(!recolor.partial_reaches_unemitted_ink());

    let mut tab_ink = plan();
    tab_ink.widen_for_tab_ink(
        RecolorBounds::Empty,
        RecolorBounds::Rect(PixelRect { x: 0, y: 10, w: 10, h: 10 }),
    );
    assert!(tab_ink.partial_reaches_unemitted_ink());
}
