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
        let active = FramePlan::build(state.clone(), [pane(7, 1)], Some(&inactive.key));
        assert!(!active.unchanged);
        assert_eq!(active.mode, RenderMode::Full);
        let expected =
            if degraded { inactive.damage } else { PixelRect { x: 0, y: 22, w: 100, h: 20 } };
        assert_eq!(active.damage, expected);
        let stable = FramePlan::build(state.clone(), [pane(7, 1)], Some(&active.key));
        assert_eq!(stable.mode, RenderMode::Noop);
        state.window.hovered_url_cells = HoveredUrlCells::single(7, 1, 1, 5, false);
        let released = FramePlan::build(state, [pane(7, 1)], Some(&active.key));
        assert!(!released.unchanged);
        assert_eq!(released.mode, RenderMode::Full);
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
    let mut changed = live_pane(7, 2);
    changed.dirty_rows = vec![1];
    let edited = FramePlan::build(facts(false), [changed], Some(&key));
    assert!(!edited.unchanged);
    assert_eq!(edited.mode, RenderMode::Full);
    assert_eq!(edited.damage, PixelRect { x: 0, y: 22, w: 100, h: 20 });
    assert_eq!(edited.damaged_rows, 1);
    assert!(edited.acknowledges(0, 7, 2));
    assert!(!edited.acknowledges(0, 7, 3));
    assert!(!edited.acknowledges(0, 8, 2));
    assert!(!edited.acknowledges(1, 7, 2));
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

/// Every window-level presentation discriminator rejects the old key without requiring a grid mutation.
#[test]
fn window_identity_mutations_require_full_repaint() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None);
    let changes: Vec<fn(&mut WindowIdentity)> = vec![
        |w| w.selection = Some(Selection::new(0, 0)),
        |w| w.copy_mode = Some(CopyModeIdentity::from(&CopyModeState::new_at((0, 0)))),
        |w| w.quick_select_hint_count = 1,
        |w| w.cursor_visible = true,
        |w| w.tab = 3,
        |w| w.search_hash = 1,
        |w| w.palette_hash = 1,
        |w| w.ime_hash = 1,
        |w| w.notification_hash = 1,
        |w| w.width += 1,
        |w| w.height += 1,
        |w| w.tab_hash = 1,
        |w| w.viewport_top_abs = Some(9),
        |w| w.cursor_shape = 1,
        |w| w.cursor_blink = true,
        |w| w.window_focused = true,
        |w| w.pane_focus_flash_bucket = 1,
        |w| w.hover_tab = 1,
        |w| w.close_override = 1,
        |w| w.broadcast_participants_hash = 1,
        |w| w.inline_media_hash = 1,
        |w| w.process_privileged = true,
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
/// an unchanged frame; recording its key must never acknowledge its revision.
#[test]
fn changed_revision_without_damage_is_noop_and_never_acknowledged() {
    let baseline = FramePlan::build(facts(true), [pane(7, 1)], None);
    let noop = FramePlan::build(facts(true), [pane(7, 2)], Some(&baseline.key));
    assert!(!noop.unchanged);
    assert_eq!(noop.mode, RenderMode::Noop);
    assert_ne!(noop.key, baseline.key);
    assert!(!noop.acknowledges(0, 7, 2));
}

/// On the hardware path, a revision bump with no dirty rows on a scrolled-back pane is not
/// offscreen-only dirt: the offscreen-only rule needs a non-empty live dirty set, so this
/// frame keeps its whole-surface Full repaint rather than becoming a Noop.
#[test]
fn hardware_revision_change_without_dirty_rows_stays_a_whole_surface_full() {
    let baseline = FramePlan::build(facts(false), [pane(7, 1)], None);
    let bumped = FramePlan::build(facts(false), [pane(7, 2)], Some(&baseline.key));
    assert!(bumped.panes[0].view_top_abs < bumped.panes[0].scrollback_len, "scrolled back");
    assert!(bumped.panes[0].dirty_live_rows.is_empty());
    assert!(!bumped.unchanged);
    assert_eq!(bumped.mode, RenderMode::Full);
    assert_eq!(bumped.damage, PixelRect { x: 0, y: 0, w: 240, h: 160 });
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
    assert!(!plan.acknowledges(0, 7, 2));
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
    assert!(!plan.acknowledges(0, 7, 2));
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
