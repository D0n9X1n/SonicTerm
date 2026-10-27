use std::hash::{Hash, Hasher};

use sonicterm_render_model::{
    boundary::{
        cfg::config::{ScrollbarMode, SubpixelAaMode},
        ui::{
            copy_mode::{CopyMode, CopyModeState, QuickSelectHint, QuickSelectState},
            pane::Rect as PaneRect,
            selection::Selection,
        },
    },
    inputs::HoveredUrlCells,
    DamageRect, PixelRect,
};

use crate::cursor::{RecolorBounds, RecolorRecord};

/// The terminal cursor cell as the frame draws it: its pane, viewport slot, first column and
/// width in columns. Absent when no cursor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorCell {
    pub pane_id: u64,
    pub slot: u16,
    pub col: u16,
    pub span: u16,
}

/// Which damage classes a changed frame key touches. `full` damages the whole surface; each other
/// class damages only the area its fields draw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChangeClass {
    /// A field without a narrow class changed, so the whole surface is damaged.
    pub full: bool,
    /// The tab bar's content, hover, close control or privilege marker changed.
    pub tab_band: bool,
    /// The status line's visible content changed; only its band is damaged.
    pub status_band: bool,
    /// The cursor's visibility, shape, blink or drawn cell changed.
    pub cursor: bool,
    /// Window focus changed: the cursor and the tab bar's active marker and title.
    pub focus: bool,
    /// The active pane's selection changed.
    pub selection: bool,
    /// A pane's scrollbar opacity bucket changed.
    pub scrollbar: bool,
}

/// Fixed-size identity of copy-mode state without retained quick-select text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyModeIdentity {
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    mode: u8,
    read_only: bool,
    quick_select: Option<u64>,
}

impl From<&CopyModeState> for CopyModeIdentity {
    fn from(state: &CopyModeState) -> Self {
        let CopyModeState { cursor, anchor, mode, quick_select, read_only } = state;
        let quick_select =
            quick_select.as_ref().map(|QuickSelectState { hints, scrollback_evicted }| {
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                scrollback_evicted.hash(&mut hash);
                hints.len().hash(&mut hash);
                for QuickSelectHint { hint, row, col_start, col_end, text } in hints {
                    (hint, row, col_start, col_end, text).hash(&mut hash);
                }
                hash.finish()
            });
        Self {
            cursor: *cursor,
            anchor: *anchor,
            mode: match mode {
                CopyMode::Cursor => 0,
                CopyMode::Select => 1,
            },
            read_only: *read_only,
            quick_select,
        }
    }
}

/// Window-level pixel identity with borrowed payloads reduced to stable digests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WindowIdentity {
    pub selection: Option<Selection>,
    pub copy_mode: Option<CopyModeIdentity>,
    pub quick_select_hint_count: u32,
    pub cursor_visible: bool,
    pub tab: u64,
    pub search_hash: u64,
    pub palette_hash: u64,
    pub ime_hash: u64,
    pub notification_hash: u64,
    pub width: u32,
    pub height: u32,
    pub tab_hash: u64,
    /// Pixel hash of the status line strip; 0 while it is hidden.
    pub status_hash: u64,
    pub viewport_top_abs: Option<u64>,
    pub cursor_shape: u8,
    pub cursor_blink: bool,
    pub window_focused: bool,
    pub pane_focus_flash_bucket: u8,
    pub hover_tab: u32,
    pub close_override: u8,
    pub broadcast_participants_hash: u64,
    pub inline_media_hash: u64,
    pub hovered_url_cells: Option<HoveredUrlCells>,
    pub process_privileged: bool,
    pub subpixel_aa: SubpixelAaMode,
    pub background: [u64; 4],
    pub style_rev: u64,
    pub renderer_hash: u64,
    pub overlay_active: bool,
    pub cursor_cell: Option<CursorCell>,
}

/// Structural pane identity preserves order, revision, requested viewport, and resolved projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneIdentity {
    pub id: u64,
    pub revision: u64,
    /// The grid's dirty generation: dirt marked after a presented frame changes the key, so a pane
    /// with unacknowledged dirt never takes the unchanged-key shortcut.
    pub dirty_generation: u64,
    pub rect: PixelRect,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_len: u64,
    pub viewport_top_abs: Option<u64>,
    pub view_top_abs: u64,
    pub is_active: bool,
    pub is_alt: bool,
    pub scrollbar_bucket: u16,
}

/// Retained pixel identity after transient placement and dirty-row metadata are released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameKey {
    pub window: WindowIdentity,
    pub panes: Vec<PaneIdentity>,
    metrics: [u32; 3],
    padding: [u32; 4],
    scrollbar_mode: ScrollbarMode,
    degraded: bool,
}

/// Renderer policy and physical metrics captured before stateful frame assembly.
#[derive(Debug, Clone)]
pub(crate) struct FrameFacts {
    pub window: WindowIdentity,
    pub cell_w: f32,
    pub cell_h: f32,
    pub padding: [f32; 4],
    pub vertical_ink_pad: f32,
    pub scrollbar_mode: ScrollbarMode,
    pub degraded: bool,
    /// Top edge of the drawn tab bar in surface pixels; `None` while the bar is hidden.
    pub tab_bar_top: Option<f32>,
    /// Top edge and height of the status line band in surface pixels; `None` while it is hidden.
    /// Strip ink is clipped to this band, so its damage needs no ink padding.
    pub status_band: Option<(f32, f32)>,
    /// Display scale factor, which sizes the scrollbar.
    pub scale: f32,
    /// What the last presented frame's cursor recolors rewrote.
    pub previous_recolor: RecolorRecord,
}

/// One pane's state after its whole content was replaced in place, as the planner reads it.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementShape {
    /// Grid columns.
    pub cols: u16,
    /// Grid rows on screen.
    pub rows: u16,
    /// History rows above the live screen.
    pub scrollback_len: u64,
    /// Absolute top row of a scrolled-back view, or `None` when following the live screen.
    pub viewport_top_abs: Option<u64>,
    /// Live rows the replacement marked dirty.
    pub dirty_rows: Vec<usize>,
}

/// Test hook, not a supported API: whether the real planner draws nothing for `shape`.
///
/// Plans the pane once at revision 1 and again at revision 2 after the replacement. `retained`
/// passes the first plan's key as the previous frame, as a renderer that kept its retained
/// frame does; without it the second plan is a first frame. Lets a crate without a GPU check
/// that an in-place replacement it performs is never planned as `Noop`.
#[doc(hidden)]
#[must_use]
pub fn __replacement_plans_noop(shape: &ReplacementShape, retained: bool) -> bool {
    let cell_h = 20.0;
    let rows_px = u32::from(shape.rows) * 20 + 4;
    let facts = FrameFacts {
        window: WindowIdentity {
            width: u32::from(shape.cols) * 10 + 4,
            height: rows_px,
            ..Default::default()
        },
        cell_w: 10.0,
        cell_h,
        padding: [2.0; 4],
        vertical_ink_pad: 0.0,
        scrollbar_mode: ScrollbarMode::Auto,
        degraded: false,
        tab_bar_top: None,
        status_band: None,
        scale: 1.0,
        previous_recolor: RecolorRecord::default(),
    };
    let input = |revision, dirty_rows: Vec<usize>| PaneMetadata {
        id: 1,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: u32::from(shape.cols) * 10 + 4, h: rows_px },
        cols: shape.cols,
        rows: shape.rows,
        scrollback_len: shape.scrollback_len,
        viewport_top_abs: shape.viewport_top_abs,
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink: Vec::new(),
    };
    let before = FramePlan::build(facts.clone(), [input(1, Vec::new())], None);
    let previous = retained.then_some(&before.key);
    let after = FramePlan::build(facts, [input(2, shape.dirty_rows.clone())], previous);
    after.mode == RenderMode::Noop
}

/// Visible-pane metadata contains row indices but no cells or hidden-history payloads.
#[derive(Debug, Clone)]
pub(crate) struct PaneMetadata {
    pub id: u64,
    pub revision: u64,
    /// The grid's dirty generation when the plan was built.
    pub dirty_generation: u64,
    pub rect: PixelRect,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_len: u64,
    pub viewport_top_abs: Option<u64>,
    pub is_active: bool,
    pub is_alt: bool,
    pub scrollbar_alpha: f32,
    pub dirty_rows: Vec<usize>,
    /// Per viewport slot, the committed ink of the row it presented, when that record still
    /// describes the slot's current content; `None` when missing or stale.
    pub row_ink: Vec<Option<PixelRect>>,
}

/// Resolved physical geometry and row slots consumed with the caller's original grid borrow.
#[derive(Debug)]
pub(crate) struct PlannedPane {
    pub id: u64,
    pub expected_revision: u64,
    pub full_rect: PixelRect,
    pub full_clip: Option<PixelRect>,
    pub layout: PaneRect,
    pub chrome: PaneRect,
    pub content_clip: PaneRect,
    pub view_top_abs: u64,
    pub scrollback_len: u64,
    pub cols: u16,
    pub row_count: u16,
    pub background_cols: u16,
    pub background_rows: u16,
    pub is_active: bool,
    pub is_alt: bool,
    pub scrollbar_alpha: f32,
    /// The grid's dirty rows as live-buffer indices, unmapped and unfiltered; only cache invalidation reads them.
    pub dirty_live_rows: Vec<usize>,
    /// Viewport slots that draw a dirty live row; damage and row emission read them.
    pub dirty_slots: Vec<u16>,
    /// Per viewport slot, whether this frame assembles the row.
    pub emit_rows: Vec<bool>,
    /// Per viewport slot, the valid committed ink the planner was given.
    pub row_ink: Vec<Option<PixelRect>>,
}

/// One deterministic decision bundle consumed by both production presenters.
#[derive(Debug)]
pub(crate) struct FramePlan {
    pub key: FrameKey,
    pub panes: Vec<PlannedPane>,
    pub active_index: usize,
    pub active_view_top_abs: u64,
    pub mode: RenderMode,
    pub damage: PixelRect,
    pub damaged_rows: usize,
    pub first_frame: bool,
    pub unchanged: bool,
    /// The damage classes this frame's key changed.
    pub change: ChangeClass,
    /// The whole surface, which bounds every damage rectangle.
    pub surface: PixelRect,
    /// The rectangles `damage` is the union of, for measuring its waste.
    pub damage_parts: Vec<PixelRect>,
}

impl FramePlan {
    /// Compose all frame decisions from bounded metadata before stateful assembly begins.
    pub(crate) fn build(
        facts: FrameFacts,
        inputs: impl IntoIterator<Item = PaneMetadata>,
        previous: Option<&FrameKey>,
    ) -> Self {
        let surface =
            PixelRect { x: 0, y: 0, w: facts.window.width.max(1), h: facts.window.height.max(1) };
        let mut identities = Vec::new();
        let mut panes = Vec::new();
        let mut dirt = DamageParts::default();
        let mut damaged_rows = 0;
        let geometry = DamageGeometry {
            cell_w: facts.cell_w,
            cell_h: facts.cell_h,
            vertical_ink_pad: facts.vertical_ink_pad,
            surface,
        };
        let tab_bar_top = facts.tab_bar_top;
        let status_band = facts.status_band;
        let scale = facts.scale;
        let previous_recolor = facts.previous_recolor;
        for input in inputs {
            let geometry = sonicterm_render_model::pane_content_geometry(
                input.rect,
                facts.padding,
                facts.cell_h,
                input.rows,
            );
            let origin_x = geometry.grid.x;
            let origin_y = geometry.grid.y;
            let content_w = geometry.grid.w;
            let content_h = geometry.grid.h;
            let chrome = PaneRect::new(
                geometry.content.x,
                geometry.content.y,
                geometry.content.w.max(facts.cell_w),
                geometry.content.h.max(facts.cell_h),
            );
            let layout = PaneRect::new(
                origin_x,
                origin_y,
                content_w.max(facts.cell_w),
                content_h.max(facts.cell_h),
            );
            let clip_x = origin_x.max(0.0);
            let clip_y = origin_y.max(0.0);
            let content_clip = PaneRect::new(
                clip_x,
                clip_y,
                ((origin_x + content_w).min(surface.w as f32) - clip_x).max(0.0),
                ((origin_y + content_h).min(surface.h as f32) - clip_y).max(0.0),
            );
            let view_top_abs = resolved_view_top(input.viewport_top_abs, input.scrollback_len);
            let scrollbar_bucket = effective_scrollbar_bucket(
                facts.scrollbar_mode,
                input.scrollback_len,
                input.rows,
                input.scrollbar_alpha,
            );
            identities.push(PaneIdentity {
                id: input.id,
                revision: input.revision,
                dirty_generation: input.dirty_generation,
                rect: input.rect,
                cols: input.cols,
                rows: input.rows,
                scrollback_len: input.scrollback_len,
                viewport_top_abs: input.viewport_top_abs,
                view_top_abs,
                is_active: input.is_active,
                is_alt: input.is_alt,
                scrollbar_bucket,
            });
            let dirty_slots: Vec<u16> = input
                .dirty_rows
                .iter()
                .filter_map(|&row| {
                    live_row_slot(input.is_alt, input.scrollback_len, view_top_abs, input.rows, row)
                })
                .collect();
            // One part per dirty slot (one for an alternate pane, which damages whole), so the
            // waste counter sees the gaps between dirty rows. Each group is a borrowed slice, so
            // the loop allocates nothing per dirty row.
            let mut add_slot_group = |slots: &[u16]| {
                if let Some(rect) = pane_damage_rect_with_ink_pad(
                    input.is_alt,
                    slots.iter().map(|&slot| usize::from(slot)),
                    input.rect,
                    origin_x,
                    origin_y,
                    input.cols,
                    facts.cell_w,
                    facts.cell_h,
                    facts.vertical_ink_pad,
                    surface.w,
                    surface.h,
                ) {
                    dirt.add_clipped(rect, surface);
                }
            };
            if input.is_alt {
                add_slot_group(&dirty_slots);
            } else {
                // When: `is_alt` is false, each dirty row is its own ink-padded strip.
                for slot in &dirty_slots {
                    add_slot_group(std::slice::from_ref(slot));
                }
            }
            damaged_rows += dirty_slots.len();
            panes.push(PlannedPane {
                id: input.id,
                expected_revision: input.revision,
                full_rect: input.rect,
                full_clip: input.rect.intersect(surface),
                layout,
                chrome,
                content_clip,
                view_top_abs,
                scrollback_len: input.scrollback_len,
                cols: input.cols,
                row_count: input.rows,
                background_cols: ((layout.w / facts.cell_w).floor() as i32)
                    .clamp(0, i32::from(input.cols)) as u16,
                background_rows: ((layout.h / facts.cell_h).floor() as i32)
                    .clamp(0, i32::from(input.rows)) as u16,
                is_active: input.is_active,
                is_alt: input.is_alt,
                scrollbar_alpha: input.scrollbar_alpha,
                dirty_slots,
                dirty_live_rows: input.dirty_rows,
                // Filled once the mode is decided.
                emit_rows: Vec::new(),
                row_ink: input.row_ink,
            });
        }
        let active_index = panes.iter().position(|pane| pane.is_active).unwrap_or(0);
        let active_live_top = panes.get(active_index).map_or(0, |pane| pane.scrollback_len);
        let active_view_top_abs =
            facts.window.viewport_top_abs.unwrap_or(active_live_top).min(active_live_top);
        let key = FrameKey {
            window: facts.window,
            panes: identities,
            metrics: [
                facts.cell_w.to_bits(),
                facts.cell_h.to_bits(),
                facts.vertical_ink_pad.to_bits(),
            ],
            padding: facts.padding.map(f32::to_bits),
            scrollbar_mode: facts.scrollbar_mode,
            degraded: facts.degraded,
        };
        let first_frame = previous.is_none();
        let unchanged = previous.is_some_and(|previous| previous == &key);
        let change = previous
            .map_or(ChangeClass { full: true, ..ChangeClass::default() }, |previous| {
                classify_frame(previous, &key)
            });
        // Any class change repaints the whole surface on the degraded path, as every window
        // change did before classes existed; the hardware path damages only the class areas.
        let window_changed = change.full || change.any_class();
        if change.full {
            dirt.add_clipped(surface, surface);
        } else if let Some(previous) = previous {
            // When: previous exists without a full-class change, hover transitions replace only old and new glyph-row ink.
            if previous.window.hovered_url_cells != key.window.hovered_url_cells {
                for hovered in [previous.window.hovered_url_cells, key.window.hovered_url_cells]
                    .into_iter()
                    .flatten()
                {
                    if let Some(pane) = panes.iter().find(|pane| pane.id == hovered.pane_id) {
                        let rows = hovered
                            .spans()
                            .iter()
                            .filter(|span| span.row < pane.row_count)
                            .map(|span| usize::from(span.row));
                        if let Some(rect) = dirty_rows_damage_rect_with_ink_pad(
                            rows,
                            pane.full_rect,
                            pane.layout.x,
                            pane.layout.y,
                            pane.cols,
                            facts.cell_w,
                            facts.cell_h,
                            facts.vertical_ink_pad,
                            surface.w,
                            surface.h,
                        ) {
                            dirt.add_clipped(rect, surface);
                        }
                    }
                }
            }
            add_class_damage(
                &mut dirt,
                ClassDamageInputs {
                    change,
                    previous,
                    key: &key,
                    panes: &panes,
                    active_index,
                    active_view_top_abs,
                    tab_bar_top,
                    status_band,
                    scale,
                    scrollbar_mode: facts.scrollbar_mode,
                    previous_recolor,
                    geometry,
                },
            );
        }
        // A revision-only change whose dirt is all scrolled out of view draws nothing new. An
        // active overlay is excluded: a preedit follows the live cursor, which the key omits.
        let offscreen_only = !unchanged
            && !window_changed
            && !key.window.overlay_active
            && dirt.rect().is_none()
            && previous.is_some_and(|previous| {
                previous.window.hovered_url_cells == key.window.hovered_url_cells
                    && previous.panes.iter().zip(&key.panes).zip(&panes).all(
                        |((before, after), planned)| {
                            before.revision == after.revision
                                || (!planned.dirty_live_rows.is_empty()
                                    && planned.dirty_slots.is_empty())
                        },
                    )
            });
        // A7: a changed hardware key whose composed damage (dirt, hover and every class) is empty
        // and whose panes hold no live dirt draws nothing new and has nothing to acknowledge. An
        // active overlay is excluded for the same reason as above.
        let quiet = !unchanged
            && !first_frame
            && !facts.degraded
            && !change.full
            && !key.window.overlay_active
            && dirt.rect().is_none()
            && panes.iter().all(|pane| pane.dirty_live_rows.is_empty());
        let decided = if unchanged || offscreen_only || quiet {
            RenderMode::Noop
        } else {
            // When: `unchanged` is false, compose redraw policy using the same key and damage planned for execution.
            decide_render_mode(
                facts.degraded,
                RenderSignals {
                    first_frame,
                    overlay_active_or_toggled: window_changed || key.window.overlay_active,
                    dirty_damage: dirt.rect(),
                    ..Default::default()
                },
            )
        };
        let composed = dirt.rect();
        // A changed hardware frame narrows to the rows its damage can reach only when nothing
        // forces a whole repaint and every clean visible row has a record bounding its ink.
        let partial = decided == RenderMode::Full
            && !facts.degraded
            && !first_frame
            && !change.full
            && !key.window.overlay_active
            && previous.is_some_and(|previous| !previous.window.overlay_active)
            && composed.is_some_and(|damage| damage != surface)
            && panes.iter().all(records_complete);
        let mode = if partial { RenderMode::Partial } else { decided };
        // A Full plan is one full frame for the counting renderer building it.
        crate::frame_stats::note_full_frame(mode == RenderMode::Full);
        let damage = if first_frame || (facts.degraded && mode == RenderMode::Full) {
            surface
        } else {
            // When: `first_frame` is false outside a degraded full repaint, replace only the composed `dirt`.
            composed.unwrap_or(surface)
        };
        let damage = if offscreen_only || quiet {
            // An offscreen-only or A7 Noop changes no pixel; any unacknowledged dirt waits for a later frame.
            PixelRect { x: 0, y: 0, w: 0, h: 0 }
        } else {
            // When: neither `offscreen_only` nor `quiet` holds, keep the damage composed from first-frame, degraded and dirty-row policy.
            damage
        };
        // The cursor cell is where the block cursor recolors glyphs; the previous recolor bounds are
        // the glyphs it recolored last frame. A row whose record meets either is assembled, so this
        // frame's recolor record is complete and widening by it rarely needs the fallback.
        let cursor_reach =
            key.window.cursor_cell.and_then(|cell| cursor_cell_rect(&panes, cell, geometry));
        let active_clip = panes.get(active_index).and_then(|pane| pane.full_clip);
        let recolor_reach = resolve_recolor(previous_recolor.bounds, active_clip, surface);
        for pane in &mut panes {
            let rows = usize::from(pane.row_count);
            pane.emit_rows = match mode {
                RenderMode::Full => vec![true; rows],
                RenderMode::Noop => vec![false; rows],
                RenderMode::Partial => {
                    partial_emit_rows(pane, damage, [cursor_reach, recolor_reach], geometry)
                }
            };
        }
        let damage_parts = if Some(damage) == composed {
            dirt.parts
        } else {
            // When: `damage` is not the `composed` union (first frame, degraded, fallback or empty),
            // the damage itself is its only part, so it is measured as wasting nothing.
            vec![damage]
        };
        Self {
            key,
            panes,
            active_index,
            active_view_top_abs,
            mode,
            damage,
            damaged_rows,
            first_frame,
            unchanged,
            change,
            surface,
            damage_parts,
        }
    }
}

/// The drawn cursor cell in surface pixels, where the block cursor recolors glyphs; `None` when
/// its pane is not planned.
fn cursor_cell_rect(
    panes: &[PlannedPane],
    cell: CursorCell,
    geometry: DamageGeometry,
) -> Option<PixelRect> {
    let pane = panes.iter().find(|pane| pane.id == cell.pane_id)?;
    let left = pane.layout.x + f32::from(cell.col) * geometry.cell_w;
    let top = pane.layout.y + f32::from(cell.slot) * geometry.cell_h;
    let width = f32::from(cell.span.max(1)) * geometry.cell_w;
    Some(crate::cursor::outward_rect((left, top, width, geometry.cell_h)))
}

pub(crate) fn effective_scrollbar_bucket(
    mode: ScrollbarMode,
    scrollback_len: u64,
    rows: u16,
    alpha: f32,
) -> u16 {
    if matches!(mode, ScrollbarMode::Never)
        || scrollback_len == 0
        || rows == 0
        || alpha <= sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR
    {
        // When: no scrollbar pixels are eligible, `alpha` has no effect on frame identity.
        return 0;
    }
    (alpha.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}

/// Classify a changed key against the previous one: window fields, frame-wide policy and every
/// pane pair. While an overlay is active on either side, any change to the key is `full`:
/// overlays draw over everything in batch order, and a preedit follows the live cursor, which
/// the key omits, so even a revision, dirty-generation or hover change can move overlay pixels.
fn classify_frame(previous: &FrameKey, key: &FrameKey) -> ChangeClass {
    let mut change = key.window.classify(&previous.window);
    if previous.metrics != key.metrics
        || previous.padding != key.padding
        || previous.degraded != key.degraded
        || previous.scrollbar_mode != key.scrollbar_mode
        || previous.panes.len() != key.panes.len()
    {
        change.full = true;
    }
    for (before, after) in previous.panes.iter().zip(&key.panes) {
        let pane_change = after.classify(before);
        change.full |= pane_change.full;
        change.scrollbar |= pane_change.scrollbar;
    }
    if previous != key && (previous.window.overlay_active || key.window.overlay_active) {
        // Either key's overlay may have moved with the live cursor, so the whole surface repaints.
        change.full = true;
    }
    change
}

/// Cell metrics and surface bounds every class-damage rectangle is computed with.
#[derive(Debug, Clone, Copy)]
struct DamageGeometry {
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface: PixelRect,
}

/// Everything class damage reads, borrowed from the plan under construction.
struct ClassDamageInputs<'plan> {
    change: ChangeClass,
    previous: &'plan FrameKey,
    key: &'plan FrameKey,
    panes: &'plan [PlannedPane],
    active_index: usize,
    active_view_top_abs: u64,
    tab_bar_top: Option<f32>,
    status_band: Option<(f32, f32)>,
    scale: f32,
    scrollbar_mode: ScrollbarMode,
    previous_recolor: RecolorRecord,
    geometry: DamageGeometry,
}

/// A frame's damage as one union rectangle plus the parts it unions, for the waste counter.
#[derive(Debug, Default)]
struct DamageParts {
    union: DamageRect,
    parts: Vec<PixelRect>,
}

impl DamageParts {
    /// Add `rect` clipped to `bounds`; a rectangle outside `bounds` adds nothing.
    fn add_clipped(&mut self, rect: PixelRect, bounds: PixelRect) {
        if let Some(clipped) = rect.intersect(bounds) {
            self.union.add_clipped(clipped, bounds);
            self.parts.push(clipped);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderMode {
    Full,
    Partial,
    Noop,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RenderSignals {
    pub first_frame: bool,
    pub resize: bool,
    pub dpi_or_scale_change: bool,
    pub font_or_atlas_rebuild: bool,
    pub theme_or_config_reload: bool,
    pub surface_reconfigure: bool,
    pub occlusion_restore: bool,
    pub viewport_scroll: bool,
    pub selection_change: bool,
    pub tab_switch: bool,
    pub pane_topology_change: bool,
    pub scrollbar_change: bool,
    pub overlay_active_or_toggled: bool,
    pub degrade_state_changed: bool,
    pub dirty_damage: Option<PixelRect>,
}

pub(crate) fn decide_render_mode(degrade: bool, signals: RenderSignals) -> RenderMode {
    if !degrade {
        // When: `degrade` is false, every changed key uses full hardware assembly.
        return RenderMode::Full;
    }
    let force_full = signals.first_frame
        || signals.resize
        || signals.dpi_or_scale_change
        || signals.font_or_atlas_rebuild
        || signals.theme_or_config_reload
        || signals.surface_reconfigure
        || signals.occlusion_restore
        || signals.viewport_scroll
        || signals.selection_change
        || signals.tab_switch
        || signals.pane_topology_change
        || signals.scrollbar_change
        || signals.overlay_active_or_toggled
        || signals.degrade_state_changed;
    if force_full || signals.dirty_damage.is_some() {
        RenderMode::Full
    } else {
        // When: `force_full` and `dirty_damage` are absent, no pixels need new software assembly.
        RenderMode::Noop
    }
}

#[cfg(test)]
#[path = "frame_plan_tests.rs"]
mod frame_plan_tests;

#[path = "frame_plan_damage.rs"]
mod damage;
#[path = "frame_plan_identity.rs"]
mod identity;
#[cfg(test)]
pub(crate) use damage::pane_scrollbar_identity;
use damage::{add_class_damage, resolve_recolor, slot_damage};
pub(crate) use damage::{
    dirty_rows_damage_rect_with_ink_pad, pane_damage_rect_with_ink_pad, pane_scrollbar_geometry,
};
#[path = "frame_plan_rows.rs"]
mod rows;
pub(crate) use rows::{live_row_slot, resolved_view_top};
use rows::{partial_emit_rows, records_complete};

#[cfg(test)]
pub(crate) use rows::live_row_to_slot;
