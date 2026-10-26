//! Class and viewport damage geometry contracts.

use super::super::frame_plan_tests::*;
use super::*;

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
        (|window| window.status_hash = 1, narrow(|class| class.status_band = true)),
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
        status_hash: _,
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
    let source = concat!(
        include_str!("frame_plan.rs"),
        "\n",
        include_str!("frame_plan_damage.rs"),
        "\n",
        include_str!("frame_plan_identity.rs"),
        "\n",
        include_str!("frame_plan_rows.rs")
    )
    .replace("\r\n", "\n");
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
