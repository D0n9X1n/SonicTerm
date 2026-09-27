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
#[test]
fn bottom_alignment_damage_and_resize_follow_grid_origin() {
    let mut input = pane(7, 1);
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

fn pane(id: u64, revision: u64) -> PaneMetadata {
    PaneMetadata {
        id,
        revision,
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
#[test]
fn primary_plan_composes_complete_decisions() {
    let first = FramePlan::build(facts(false), [pane(7, 1)], None);
    assert_eq!(first.mode, RenderMode::Full);
    assert!(first.first_frame);
    assert_eq!(first.damage, PixelRect { x: 0, y: 0, w: 240, h: 160 });
    assert_eq!(first.panes[0].layout, PaneRect::new(2.0, 2.0, 96.0, 80.0));
    assert_eq!(first.panes[0].content_clip, first.panes[0].layout);
    assert_eq!(first.panes[0].rows().collect::<Vec<_>>(), [(0, 10), (1, 11), (2, 12), (3, 13)]);
    let key = first.key.clone();
    let unchanged = FramePlan::build(facts(false), [pane(7, 1)], Some(&key));
    assert!(unchanged.unchanged);
    assert_eq!(unchanged.mode, RenderMode::Noop);
    assert_eq!(unchanged.key, key);
    let mut changed = pane(7, 2);
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
    assert!(plan.panes[0].dirty_rows.is_empty());
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
