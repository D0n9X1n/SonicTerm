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
        |w| w.broadcast_receivers_hash = 1,
        |w| w.inline_media_hash = 1,
        |w| w.hovered_url_cells = HoveredUrlCells::single(7, 0, 0, 1, true),
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
