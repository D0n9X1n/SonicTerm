use super::*;

pub(super) fn facts(degraded: bool) -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 160, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [2.0; 4],
        vertical_ink_pad: 0.0,
        scrollbar_mode: ScrollbarMode::Auto,
        degraded,
        tab_bar_top: Some(140.0),
        status_band: None,
        scale: 1.0,
        previous_recolor: RecolorRecord::default(),
    }
}

/// The ink-padded strip of `slot` in `planned`, or an empty rectangle when it has no pixels.
pub(super) fn padded_strip(
    frame_facts: &FrameFacts,
    planned: &PlannedPane,
    slot: u16,
) -> PixelRect {
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
pub(super) fn seeded(
    frame_facts: &FrameFacts,
    prior: &FramePlan,
    input: PaneMetadata,
) -> PaneMetadata {
    seeded_with(frame_facts, prior, input, |_, strip| Some(strip))
}

/// `input` seeded like [`seeded`], with `record` choosing each slot's record from its strip.
pub(super) fn seeded_with(
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
pub(super) fn rows_ack<const ROWS: usize>(rows: [usize; ROWS]) -> sonicterm_render_model::AckRows {
    sonicterm_render_model::AckRows::Rows(rows.into_iter().collect())
}

/// A primary pane that is not scrolled back, so each live row is drawn at its own slot.
pub(super) fn live_pane(id: u64, revision: u64) -> PaneMetadata {
    PaneMetadata { viewport_top_abs: None, ..pane(id, revision) }
}

pub(super) fn pane(id: u64, revision: u64) -> PaneMetadata {
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

/// Facts for a 24-row pane: a surface tall enough that every viewport slot has pixels.
pub(super) fn tall_facts(degraded: bool) -> FrameFacts {
    let window = WindowIdentity { width: 240, height: 600, ..Default::default() };
    FrameFacts { window, ..facts(degraded) }
}

/// A primary pane with 100 history rows whose view is scrolled back three rows, so live
/// row `r` is drawn at slot `r + 3` and live rows 21 to 23 are below the view.
pub(super) fn scrolled_back_pane() -> PaneMetadata {
    PaneMetadata {
        rect: PixelRect { x: 0, y: 0, w: 100, h: 484 },
        rows: 24,
        scrollback_len: 100,
        viewport_top_abs: Some(97),
        ..pane(7, 1)
    }
}

/// The damage rectangle one viewport slot of `planned` occupies under `frame_facts`.
pub(super) fn slot_rect(frame_facts: &FrameFacts, planned: &PlannedPane, slot: usize) -> PixelRect {
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
pub(super) fn offscreen_only_edit(degraded: bool) -> (FramePlan, u64) {
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

/// The tab bar band `facts` draws: full width from y=140 to the 160 px surface bottom.
pub(super) const TAB_BAND: PixelRect = PixelRect { x: 0, y: 140, w: 240, h: 20 };

/// The ink-padded damage of viewport slot `slot` of the default 100 px pane (zero ink pad).
pub(super) fn row_rect(slot: i32) -> PixelRect {
    PixelRect { x: 0, y: 2 + 20 * slot, w: 100, h: 20 }
}

/// A drawn cursor on pane 7 at viewport slot `slot`, column 2.
pub(super) fn cursor_at(slot: u16) -> Option<CursorCell> {
    Some(CursorCell { pane_id: 7, slot, col: 2, span: 1 })
}

/// Hardware facts with a visible, focused cursor at slot 1 of pane 7.
pub(super) fn cursor_facts() -> FrameFacts {
    let mut cursor = facts(false);
    cursor.window.cursor_visible = true;
    cursor.window.window_focused = true;
    cursor.window.cursor_cell = cursor_at(1);
    cursor
}

/// Plan `after` against a presented `before` frame of the same pane input.
pub(super) fn transition(before: FrameFacts, after: FrameFacts, input: PaneMetadata) -> FramePlan {
    let first = FramePlan::build(before, [input.clone()], None);
    FramePlan::build(after, [input], Some(&first.key))
}

/// Facts whose pane padding leaves 12 px on the right, at `scale`.
pub(super) fn padded_facts(scale: f32) -> FrameFacts {
    FrameFacts { padding: [2.0, 12.0, 2.0, 2.0], scale, ..facts(false) }
}

/// Plan a scrollbar opacity change from `alpha_before` to `alpha_after` on `input`.
pub(super) fn scrollbar_fade(
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

/// A 10-row pane with 100 history rows viewed from row 90, so live row 5 (slot 15) is offscreen.
pub(super) fn offscreen_dirt_pane() -> PaneMetadata {
    PaneMetadata {
        rect: PixelRect { x: 0, y: 0, w: 100, h: 204 },
        rows: 10,
        scrollback_len: 100,
        viewport_top_abs: Some(90),
        ..pane(7, 1)
    }
}

/// Facts for a 24-row live pane with a visible, focused cursor at slot 5 (y=102..122).
pub(super) fn tall_cursor_facts() -> FrameFacts {
    let mut cursor = tall_facts(false);
    cursor.tab_bar_top = None;
    cursor.window.cursor_visible = true;
    cursor.window.window_focused = true;
    cursor.window.cursor_cell = cursor_at(5);
    cursor
}

/// The 24-row live pane `tall_cursor_facts` draws its cursor in.
pub(super) fn tall_live_pane() -> PaneMetadata {
    PaneMetadata { viewport_top_abs: None, ..scrolled_back_pane() }
}

/// A recolor record over `rect`.
pub(super) fn recolored(rect: PixelRect, hash: u64) -> RecolorRecord {
    RecolorRecord { bounds: RecolorBounds::Rect(rect), hash }
}

/// The tall glyph a block cursor at slot 5 recolors, reaching y=72 above the cursor row.
pub(super) const TALL_GLYPH: PixelRect = PixelRect { x: 50, y: 72, w: 10, h: 40 };
/// A glyph exactly under the slot-5 cursor cell.
pub(super) const SHORT_GLYPH: PixelRect = PixelRect { x: 50, y: 102, w: 10, h: 20 };

/// The tall glyph of slot 3 of the 24-row cursor pane, reaching from y=72 into the slot-5 cursor
/// cell at x=22..32; its padded strip (y=62..82) misses that row.
pub(super) const CURSOR_TALL_RECORD: PixelRect = PixelRect { x: 22, y: 72, w: 10, h: 40 };

/// The 24-row cursor pane seeded from `prior`, with slot 3 holding `CURSOR_TALL_RECORD`.
pub(super) fn tall_glyph_records(frame_facts: &FrameFacts, prior: &FramePlan) -> PaneMetadata {
    seeded_with(frame_facts, prior, tall_live_pane(), |slot, strip| {
        Some(if slot == 3 { CURSOR_TALL_RECORD } else { strip })
    })
}

/// Facts for a 10-row live pane with room on the surface for every slot and no tab bar.
pub(super) fn ten_row_facts(vertical_ink_pad: f32) -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 240, ..Default::default() },
        vertical_ink_pad,
        tab_bar_top: None,
        status_band: None,
        ..facts(false)
    }
}

/// A live 10-row pane, 100 px wide, with every slot on the surface of `ten_row_facts`.
pub(super) fn ten_row_pane() -> PaneMetadata {
    PaneMetadata { rect: PixelRect { x: 0, y: 0, w: 100, h: 204 }, rows: 10, ..live_pane(7, 1) }
}

/// Present `input` under `frame_facts`, then plan an edit of its live rows `dirty` against that
/// frame with complete seeded records.
pub(super) fn seeded_edit(
    frame_facts: &FrameFacts,
    input: PaneMetadata,
    dirty: &[usize],
) -> FramePlan {
    let first = FramePlan::build(frame_facts.clone(), [input.clone()], None);
    let mut edited = seeded(frame_facts, &first, input);
    edited.revision += 1;
    edited.dirty_rows = dirty.to_vec();
    FramePlan::build(frame_facts.clone(), [edited], Some(&first.key))
}

/// The one-row edit on a 10-row pane with a one-cell ink pad.
pub(super) fn ten_row_edit(degraded: bool) -> FramePlan {
    seeded_edit(&FrameFacts { degraded, ..ten_row_facts(20.0) }, ten_row_pane(), &[5])
}

/// Two side-by-side 4-row panes: pane 7's unchanged row 1 overhangs to x=108, and pane 9 edits
/// its row 1, damaging x>=100 on that row.
pub(super) fn neighbour_edit(degraded: bool) -> FramePlan {
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
pub(super) fn cursor_reach_edit(degraded: bool) -> FramePlan {
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
pub(super) fn emitted(plan: &FramePlan, index: usize) -> Vec<usize> {
    plan.panes[index]
        .emit_rows
        .iter()
        .enumerate()
        .filter(|(_, emit)| **emit)
        .map(|(row, _)| row)
        .collect()
}

/// Facts for the 16-row scrolled-view fixtures: room for two 324 px panes, no tab bar.
pub(super) fn sixteen_row_facts() -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 400, ..Default::default() },
        tab_bar_top: None,
        status_band: None,
        ..facts(false)
    }
}

/// A 16-row pane with 100 history rows, at `x`, viewing from `view_top`.
pub(super) fn sixteen_row_pane(id: u64, x: i32, view_top: Option<u64>) -> PaneMetadata {
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
pub(super) fn seeded_edits(inputs: Vec<(PaneMetadata, Vec<usize>)>) -> FramePlan {
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

/// Facts with a 20 px tab bar at 140 and a 12 px status line band directly above it at 128.
pub(super) fn strip_facts(degraded: bool) -> FrameFacts {
    FrameFacts { status_band: Some((128.0, 12.0)), ..facts(degraded) }
}
