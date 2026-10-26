//! Partial-frame row emission and ink-record validity contracts.

use super::super::frame_plan_tests::*;
use super::*;

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
        status_band: None,
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

/// The completeness certificate's scene is the frame key without the dirt fields: a pane revision or
/// dirty-generation change leaves it equal, while a viewport, overlay or title-width change (the
/// tab-strip hash) makes it differ, so those changes read `scene changed`.
#[test]
fn the_scene_ignores_only_dirt() {
    let base = FramePlan::build(facts(false), [pane(7, 1)], None);
    let dirtied = FramePlan::build(
        facts(false),
        [PaneMetadata { dirty_generation: 5, ..pane(7, 2) }],
        Some(&base.key),
    );
    assert_ne!(dirtied.key, base.key, "the key itself records the dirt");
    assert_eq!(dirtied.key.scene(), base.key.scene(), "the scene does not");
    // Field by field: only the two dirt fields are cleared; every other pane and window field is kept.
    let scene = dirtied.key.scene();
    assert_eq!(scene.window, dirtied.key.window);
    assert_eq!(
        scene.panes,
        vec![PaneIdentity { revision: 0, dirty_generation: 0, ..dirtied.key.panes[0] }]
    );
    let changes: Vec<fn(&mut FrameFacts)> = vec![
        |input| input.window.viewport_top_abs = Some(9),
        |input| input.window.overlay_active = true,
        |input| input.window.tab_hash = 1,
    ];
    for change in changes {
        let mut input = facts(false);
        change(&mut input);
        let changed = FramePlan::build(input, [pane(7, 1)], Some(&base.key));
        assert_ne!(changed.key.scene(), base.key.scene());
    }
    let moved = FramePlan::build(
        facts(false),
        [PaneMetadata { viewport_top_abs: Some(11), ..pane(7, 1) }],
        Some(&base.key),
    );
    assert_ne!(moved.key.scene(), base.key.scene(), "a pane viewport move is a new scene");
}

/// Contract: a pane whose whole content was replaced in place while its view shows history (every
/// live row dirty, none on screen) is planned as `Noop` against a retained key, so a caller that
/// replaces content must drop the retained frame; without it the plan is a full first frame.
#[test]
fn an_offscreen_replacement_is_noop_only_with_a_retained_key() {
    let shape = ReplacementShape {
        cols: 100,
        rows: 40,
        scrollback_len: 160,
        viewport_top_abs: Some(0),
        dirty_rows: (0..40).collect(),
    };
    assert!(__replacement_plans_noop(&shape, true), "the retained key hides the replaced rows");
    assert!(!__replacement_plans_noop(&shape, false), "a dropped key forces a full frame");
}

/// A strip-only change damages exactly the strip band on the hardware path, with or without
/// the tab bar, because the strip's glyphs are clipped to that band.
#[test]
fn status_line_change_damages_only_the_strip_band() {
    let mut after = strip_facts(false);
    after.window.status_hash = 1;
    let plan = transition(strip_facts(false), after, pane(7, 1));
    assert!(plan.change.status_band && !plan.change.full && !plan.change.tab_band);
    assert_eq!(plan.damage, PixelRect { x: 0, y: 128, w: 240, h: 12 });

    // With the tab bar hidden the strip sits at the bottom edge and its band is still all that changes.
    let bottom = |degraded| FrameFacts {
        tab_bar_top: None,
        status_band: Some((148.0, 12.0)),
        ..facts(degraded)
    };
    let mut after = bottom(false);
    after.window.status_hash = 7;
    let plan = transition(bottom(false), after, pane(7, 1));
    assert_eq!(plan.damage, PixelRect { x: 0, y: 148, w: 240, h: 12 });
}

/// The existing full-frame fallbacks keep the whole surface for a strip change: degraded
/// (software) rendering and an active overlay. An unchanged strip hash presents nothing.
#[test]
fn status_line_change_keeps_full_frame_fallbacks() {
    let surface = PixelRect { x: 0, y: 0, w: 240, h: 160 };
    let mut degraded_after = strip_facts(true);
    degraded_after.window.status_hash = 1;
    let plan = transition(strip_facts(true), degraded_after, pane(7, 1));
    assert_eq!((plan.mode, plan.damage), (RenderMode::Full, surface));

    let mut overlay_before = strip_facts(false);
    overlay_before.window.overlay_active = true;
    let mut overlay_after = overlay_before.clone();
    overlay_after.window.status_hash = 1;
    let plan = transition(overlay_before, overlay_after, pane(7, 1));
    assert_eq!((plan.mode, plan.damage), (RenderMode::Full, surface));

    let plan = transition(strip_facts(false), strip_facts(false), pane(7, 1));
    assert_eq!(plan.mode, RenderMode::Noop);
}
