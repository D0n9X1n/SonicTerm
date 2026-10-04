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
    /// The cursor's visibility, shape, blink or drawn cell changed.
    pub cursor: bool,
    /// Window focus changed: the cursor and the tab bar's active marker and title.
    pub focus: bool,
    /// The active pane's selection changed.
    pub selection: bool,
    /// A pane's scrollbar opacity bucket changed.
    pub scrollbar: bool,
}

impl ChangeClass {
    /// True when any narrow class, but not necessarily `full`, is set.
    pub(crate) fn any_class(self) -> bool {
        self.tab_band || self.cursor || self.focus || self.selection || self.scrollbar
    }
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
        let quick_select = quick_select.as_ref().map(|QuickSelectState { hints }| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
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

impl WindowIdentity {
    /// Classify every field that differs from `previous`.
    ///
    /// Both identities are destructured without a rest pattern, so a new field fails to compile
    /// here until it is given a class. `hovered_url_cells` has its own row path in the planner.
    pub(crate) fn classify(&self, previous: &Self) -> ChangeClass {
        let Self {
            selection,
            copy_mode,
            quick_select_hint_count,
            cursor_visible,
            tab,
            search_hash,
            palette_hash,
            ime_hash,
            notification_hash,
            width,
            height,
            tab_hash,
            viewport_top_abs,
            cursor_shape,
            cursor_blink,
            window_focused,
            pane_focus_flash_bucket,
            hover_tab,
            close_override,
            broadcast_participants_hash,
            inline_media_hash,
            hovered_url_cells: _,
            process_privileged,
            subpixel_aa,
            background,
            style_rev,
            renderer_hash,
            overlay_active,
            cursor_cell,
        } = self;
        let Self {
            selection: previous_selection,
            copy_mode: previous_copy_mode,
            quick_select_hint_count: previous_quick_select_hint_count,
            cursor_visible: previous_cursor_visible,
            tab: previous_tab,
            search_hash: previous_search_hash,
            palette_hash: previous_palette_hash,
            ime_hash: previous_ime_hash,
            notification_hash: previous_notification_hash,
            width: previous_width,
            height: previous_height,
            tab_hash: previous_tab_hash,
            viewport_top_abs: previous_viewport_top_abs,
            cursor_shape: previous_cursor_shape,
            cursor_blink: previous_cursor_blink,
            window_focused: previous_window_focused,
            pane_focus_flash_bucket: previous_pane_focus_flash_bucket,
            hover_tab: previous_hover_tab,
            close_override: previous_close_override,
            broadcast_participants_hash: previous_broadcast_participants_hash,
            inline_media_hash: previous_inline_media_hash,
            hovered_url_cells: _,
            process_privileged: previous_process_privileged,
            subpixel_aa: previous_subpixel_aa,
            background: previous_background,
            style_rev: previous_style_rev,
            renderer_hash: previous_renderer_hash,
            overlay_active: previous_overlay_active,
            cursor_cell: previous_cursor_cell,
        } = previous;
        ChangeClass {
            full: copy_mode != previous_copy_mode
                || quick_select_hint_count != previous_quick_select_hint_count
                || tab != previous_tab
                || search_hash != previous_search_hash
                || palette_hash != previous_palette_hash
                || ime_hash != previous_ime_hash
                || notification_hash != previous_notification_hash
                || width != previous_width
                || height != previous_height
                || viewport_top_abs != previous_viewport_top_abs
                || pane_focus_flash_bucket != previous_pane_focus_flash_bucket
                || broadcast_participants_hash != previous_broadcast_participants_hash
                || inline_media_hash != previous_inline_media_hash
                || subpixel_aa != previous_subpixel_aa
                || background != previous_background
                || style_rev != previous_style_rev
                || renderer_hash != previous_renderer_hash
                || overlay_active != previous_overlay_active,
            tab_band: tab_hash != previous_tab_hash
                || hover_tab != previous_hover_tab
                || close_override != previous_close_override
                || process_privileged != previous_process_privileged,
            cursor: cursor_visible != previous_cursor_visible
                || cursor_shape != previous_cursor_shape
                || cursor_blink != previous_cursor_blink
                || cursor_cell != previous_cursor_cell,
            focus: window_focused != previous_window_focused,
            selection: selection != previous_selection,
            scrollbar: false,
        }
    }
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

impl PaneIdentity {
    /// Classify every field that differs from `previous`. Revision and dirty generation are dirt,
    /// damaged through the plan's dirty slots, so they set no class.
    pub(crate) fn classify(&self, previous: &Self) -> ChangeClass {
        let Self {
            id,
            revision: _,
            dirty_generation: _,
            rect,
            cols,
            rows,
            scrollback_len,
            viewport_top_abs,
            view_top_abs,
            is_active,
            is_alt,
            scrollbar_bucket,
        } = self;
        let Self {
            id: previous_id,
            revision: _,
            dirty_generation: _,
            rect: previous_rect,
            cols: previous_cols,
            rows: previous_rows,
            scrollback_len: previous_scrollback_len,
            viewport_top_abs: previous_viewport_top_abs,
            view_top_abs: previous_view_top_abs,
            is_active: previous_is_active,
            is_alt: previous_is_alt,
            scrollbar_bucket: previous_scrollbar_bucket,
        } = previous;
        ChangeClass {
            full: id != previous_id
                || rect != previous_rect
                || cols != previous_cols
                || rows != previous_rows
                || scrollback_len != previous_scrollback_len
                || viewport_top_abs != previous_viewport_top_abs
                || view_top_abs != previous_view_top_abs
                || is_active != previous_is_active
                || is_alt != previous_is_alt,
            scrollbar: scrollbar_bucket != previous_scrollbar_bucket,
            tab_band: false,
            cursor: false,
            focus: false,
            selection: false,
        }
    }
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
    /// Display scale factor, which sizes the scrollbar.
    pub scale: f32,
    /// What the last presented frame's cursor recolors rewrote.
    pub previous_recolor: RecolorRecord,
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

impl PlannedPane {
    /// Iterate resolved viewport slots without retaining copied rows or hidden-history entries.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = (u16, u64)> + '_ {
        (0..self.row_count).map(|slot| (slot, self.view_top_abs.saturating_add(u64::from(slot))))
    }
}

/// Map a grid dirty row, a live-buffer index, to the viewport slot that draws it.
///
/// Live row `live_row` is absolute row `scrollback_len + live_row`; a view whose top is
/// `view_top_abs` draws it at slot `scrollback_len + live_row - view_top_abs` when that is
/// below `rows`, and draws it nowhere otherwise. The view top is clamped to the live top, as
/// [`FramePlan::build`] clamps it, so an unscrolled view maps every live row to itself.
pub(crate) fn live_row_to_slot(
    scrollback_len: u64,
    view_top_abs: u64,
    rows: u16,
    live_row: usize,
) -> Option<u16> {
    let view_top_abs = view_top_abs.min(scrollback_len);
    let row_abs = scrollback_len.checked_add(u64::try_from(live_row).ok()?)?;
    let slot = row_abs.checked_sub(view_top_abs)?;
    u16::try_from(slot).ok().filter(|slot| *slot < rows)
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
            let view_top_abs =
                input.viewport_top_abs.unwrap_or(input.scrollback_len).min(input.scrollback_len);
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
                    live_row_slot(
                        input.is_alt,
                        input.scrollback_len,
                        view_top_abs,
                        input.rows,
                        row,
                    )
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

    /// Widen a presenting plan's damage by the cursor recolors: whenever the record assembly
    /// produced (`current`) differs from the last presented one (`previous`), both bounds are
    /// damaged, so a replaced, removed or new recolored glyph is repainted; with the cursor or
    /// focus class set, the current bounds are damaged even when the record is unchanged.
    /// `Unbounded` damages the active pane. A partial plan's widened damage can reach a row it did
    /// not emit; [`Self::partial_reaches_unemitted_ink`] reports that, and assembly falls back.
    pub(crate) fn widen_for_recolor(&mut self, previous: RecolorRecord, current: RecolorRecord) {
        if self.mode == RenderMode::Noop {
            // When: `mode` is Noop, the plan presents nothing and no damage is read.
            return;
        }
        let changed = previous != current;
        let class = self.change.cursor || self.change.focus;
        let mut records = Vec::with_capacity(2);
        if changed {
            records.push(previous.bounds);
        }
        if changed || class {
            records.push(current.bounds);
        }
        let active_clip = self.panes.get(self.active_index).and_then(|pane| pane.full_clip);
        for bounds in records {
            let Some(rect) = resolve_recolor(bounds, active_clip, self.surface) else {
                // When: the bounds are empty, nothing was recolored on that side.
                continue;
            };
            self.add_damage(rect);
        }
    }

    /// Widen a full plan's damage by the tab-title ink the last presented frame and this frame
    /// drew. A title glyph can reach above the padded tab band, so whenever the tab-band or
    /// focus class is set, or the ink changed, both sides' ink is repainted. `Unbounded` ink
    /// damages the whole surface. As with the recolor, a partial plan checks the widened damage
    /// against the rows it did not emit.
    pub(crate) fn widen_for_tab_ink(&mut self, previous: RecolorBounds, current: RecolorBounds) {
        if self.mode == RenderMode::Noop {
            // When: `mode` is Noop, the plan presents nothing and no damage is read.
            return;
        }
        if previous == current && !self.change.tab_band && !self.change.focus {
            // When: the ink is unchanged and no tab-band or focus class is set, it is on screen.
            return;
        }
        for bounds in [previous, current] {
            if let Some(rect) = resolve_recolor(bounds, Some(self.surface), self.surface) {
                self.add_damage(rect);
            }
        }
    }

    /// Union `rect`, clipped to the surface, into the damage and record it as a part.
    fn add_damage(&mut self, rect: PixelRect) {
        if let Some(clipped) = rect.intersect(self.surface) {
            self.damage = if self.damage.is_empty() {
                clipped
            } else {
                // When: damage is non-empty, the single rectangle grows to the union.
                self.damage.union(clipped)
            };
            self.damage_parts.push(clipped);
        }
    }

    /// The rows a presented frame acknowledges for the exact planned pane and revision: a `Full`
    /// plan every row, a `Partial` plan each dirty live row whose slot it emitted, a `Noop` none. A
    /// dirty live row below a scrolled view has no slot, so a partial frame keeps its bit.
    pub(crate) fn acknowledged_rows(
        &self,
        index: usize,
        id: u64,
        revision: u64,
    ) -> Option<sonicterm_render_model::AckRows> {
        let pane = self
            .panes
            .get(index)
            .filter(|pane| pane.id == id && pane.expected_revision == revision)?;
        match self.mode {
            RenderMode::Noop => None,
            RenderMode::Full => Some(sonicterm_render_model::AckRows::All),
            RenderMode::Partial => Some(sonicterm_render_model::AckRows::Rows(
                pane.dirty_live_rows
                    .iter()
                    .copied()
                    .filter(|&row| {
                        live_row_slot(
                            pane.is_alt,
                            pane.scrollback_len,
                            pane.view_top_abs,
                            pane.row_count,
                            row,
                        )
                        .is_some_and(|slot| {
                            pane.emit_rows.get(usize::from(slot)).copied().unwrap_or(false)
                        })
                    })
                    .collect(),
            )),
        }
    }

    /// Turn a partial plan into the `Full` plan assembly falls back to: every row emitted and
    /// every pane acknowledged whole. Its damage stays the composed rectangle, as a hardware
    /// `Full` keeps it, and the reassembled frame counts as one full frame.
    pub(crate) fn force_full(&mut self) {
        if self.mode != RenderMode::Partial {
            // When: `mode` is not Partial, every row is already emitted or none is presented.
            return;
        }
        self.mode = RenderMode::Full;
        for pane in &mut self.panes {
            pane.emit_rows.fill(true);
        }
        crate::frame_stats::note_full_frame(true);
        crate::frame_stats::note_partial_fallback();
    }

    /// Whether a partial plan's final damage, after the recolor and tab-ink widening, reaches the
    /// committed ink of a row it did not emit. The scissor would then reset pixels that row drew
    /// without redrawing them, so assembly must fall back to `Full`. A missing record counts as
    /// reaching, conservatively.
    pub(crate) fn partial_reaches_unemitted_ink(&self) -> bool {
        if self.mode != RenderMode::Partial {
            // When: `mode` is not Partial, every drawn row was emitted.
            return false;
        }
        self.panes.iter().filter(|pane| pane.full_clip.is_some()).any(|pane| {
            pane.emit_rows.iter().enumerate().any(|(slot, emitted)| {
                !*emitted
                    && pane
                        .row_ink
                        .get(slot)
                        .copied()
                        .flatten()
                        .is_none_or(|record| meets(record, self.damage))
            })
        })
    }
}

/// The viewport slot that draws live row `live_row`: on the alternate screen, which keeps no
/// scrollback, the row itself; otherwise through [`live_row_to_slot`]. `None` when no slot does.
pub(crate) fn live_row_slot(
    is_alt: bool,
    scrollback_len: u64,
    view_top_abs: u64,
    rows: u16,
    live_row: usize,
) -> Option<u16> {
    if is_alt {
        u16::try_from(live_row).ok().filter(|slot| *slot < rows)
    } else {
        // When: `is_alt` is false, a primary view may be scrolled back, so the live row maps through its view top.
        live_row_to_slot(scrollback_len, view_top_abs, rows, live_row)
    }
}

/// Whether two rectangles share a positive pixel area.
fn meets(left: PixelRect, right: PixelRect) -> bool {
    left.intersect(right).is_some()
}

/// Whether every row of `pane` a partial frame could leave unemitted has a valid record. Dirty
/// slots are always emitted, and a pane with no pixels on the surface draws no row.
fn records_complete(pane: &PlannedPane) -> bool {
    pane.full_clip.is_none()
        || (0..pane.row_count).all(|slot| {
            pane.row_ink.get(usize::from(slot)).is_some_and(Option::is_some)
                || pane.dirty_slots.contains(&slot)
        })
}

/// The rows a partial plan assembles for `pane`: every dirty slot, every slot whose padded strip
/// meets `damage`, and every slot whose valid record meets `damage` or a `reach` rectangle (the
/// drawn cursor cell and the last frame's recolor bounds). A record is not clipped to its pane, so
/// a neighbour's overhanging glyph is assembled. A pane off the surface assembles nothing.
fn partial_emit_rows(
    pane: &PlannedPane,
    damage: PixelRect,
    reach: [Option<PixelRect>; 2],
    geometry: DamageGeometry,
) -> Vec<bool> {
    let mut emit = vec![false; usize::from(pane.row_count)];
    if pane.full_clip.is_none() {
        // When: `full_clip` is None, neither row loop draws this pane, so `emit` stays empty of rows.
        return emit;
    }
    for slot in &pane.dirty_slots {
        if let Some(row) = emit.get_mut(usize::from(*slot)) {
            *row = true;
        }
    }
    for slot in 0..pane.row_count {
        let index = usize::from(slot);
        if emit[index] {
            // When: `emit[index]` is set, the dirty slot is assembled whatever its record says.
            continue;
        }
        let strip_meets =
            slot_damage(pane, slot, geometry).is_some_and(|strip| meets(strip, damage));
        let record_meets = pane.row_ink.get(index).copied().flatten().is_some_and(|record| {
            meets(record, damage)
                || reach.iter().flatten().any(|target| meets(record, *target))
        });
        emit[index] = strip_meets || record_meets;
    }
    emit
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
    scale: f32,
    scrollbar_mode: ScrollbarMode,
    previous_recolor: RecolorRecord,
    geometry: DamageGeometry,
}

/// Add the damage of every narrow class set in `inputs.change` to `dirt`.
fn add_class_damage(dirt: &mut DamageParts, inputs: ClassDamageInputs<'_>) {
    let ClassDamageInputs {
        change,
        previous,
        key,
        panes,
        active_index,
        active_view_top_abs,
        tab_bar_top,
        scale,
        scrollbar_mode,
        previous_recolor,
        geometry,
    } = inputs;
    let surface = geometry.surface;
    if change.tab_band || change.focus {
        if let Some(top) = tab_bar_top {
            // The bar is pinned to the surface bottom; ink above its top edge is padded in.
            let band_top = (top - geometry.vertical_ink_pad.max(0.0)).floor() as i32;
            let band_h =
                (i64::from(surface.bottom()) - i64::from(band_top)).clamp(0, i64::from(u32::MAX));
            dirt.add_clipped(
                PixelRect { x: surface.x, y: band_top, w: surface.w, h: band_h as u32 },
                surface,
            );
        }
    }
    if change.cursor || change.focus {
        for cell in [previous.window.cursor_cell, key.window.cursor_cell].into_iter().flatten() {
            match panes.iter().find(|pane| pane.id == cell.pane_id) {
                Some(pane) => {
                    if let Some(rect) = slot_damage(pane, cell.slot, geometry) {
                        dirt.add_clipped(rect, surface);
                    }
                }
                // The cursor's pane is not planned, so its pixels cannot be located.
                None => dirt.add_clipped(surface, surface),
            }
        }
        let active_clip = panes.get(active_index).and_then(|pane| pane.full_clip);
        if let Some(rect) = resolve_recolor(previous_recolor.bounds, active_clip, surface) {
            dirt.add_clipped(rect, surface);
        }
    }
    if change.selection {
        if let (Some(pane), Some(identity)) = (panes.get(active_index), key.panes.get(active_index))
        {
            for slot in changed_selection_slots(
                previous.window.selection.as_ref(),
                key.window.selection.as_ref(),
                active_view_top_abs,
                identity.rows,
                identity.cols,
            ) {
                if let Some(rect) = slot_damage(pane, slot, geometry) {
                    dirt.add_clipped(rect, surface);
                }
            }
        }
    }
    if change.scrollbar {
        // When: `change.scrollbar` is set, some pane's opacity bucket moved, so its drawn track is repainted.
        for ((before, after), pane) in previous.panes.iter().zip(&key.panes).zip(panes) {
            if before.scrollbar_bucket == after.scrollbar_bucket || pane.full_clip.is_none() {
                // When: the bucket is unchanged or the pane is off the surface, no track is redrawn.
                continue;
            }
            // A bucket-only change keeps every geometry input, so old and new tracks coincide.
            if let Some(track) = pane_scrollbar_geometry(
                pane.chrome,
                pane.row_count,
                pane.scrollback_len + u64::from(pane.row_count),
                pane.view_top_abs,
                scrollbar_mode,
                scale,
            ) {
                let track = track.track_rect;
                let rect = crate::cursor::outward_rect((track.x, track.y, track.w, track.h));
                dirt.add_clipped(rect, surface);
            }
        }
    }
}

/// The damage one viewport slot of `pane` occupies: the ink-padded row, or the whole clipped
/// pane on the alternate screen.
fn slot_damage(pane: &PlannedPane, slot: u16, geometry: DamageGeometry) -> Option<PixelRect> {
    pane_damage_rect_with_ink_pad(
        pane.is_alt,
        [usize::from(slot)],
        pane.full_rect,
        pane.layout.x,
        pane.layout.y,
        pane.cols,
        geometry.cell_w,
        geometry.cell_h,
        geometry.vertical_ink_pad,
        geometry.surface.w,
        geometry.surface.h,
    )
}

/// The viewport slots whose selection quads differ between `before` and `after`, computed with
/// the draw's own `selection_quad_rects` in unit cells so each quad's y is its slot.
fn changed_selection_slots(
    before: Option<&Selection>,
    after: Option<&Selection>,
    view_top_abs: u64,
    rows: u16,
    cols: u16,
) -> Vec<u16> {
    let quads = |selection: Option<&Selection>| -> Vec<(u16, u32, u32)> {
        selection
            .map(|selection| {
                crate::core::selection_quad_rects(
                    selection,
                    view_top_abs,
                    rows,
                    cols,
                    0.0,
                    0.0,
                    1.0,
                    1.0,
                    &[],
                )
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(left, top, width, _)| (top as u16, left.to_bits(), width.to_bits()))
            .collect()
    };
    let (old_quads, new_quads) = (quads(before), quads(after));
    let mut slots: Vec<u16> = old_quads
        .iter()
        .filter(|quad| !new_quads.contains(quad))
        .chain(new_quads.iter().filter(|quad| !old_quads.contains(quad)))
        .map(|quad| quad.0)
        .collect();
    slots.sort_unstable();
    slots.dedup();
    slots
}

/// Where recolor `bounds` draw: nothing when empty, the rectangle itself, or, when unbounded,
/// the active pane's clip (the whole surface without one).
fn resolve_recolor(
    bounds: RecolorBounds,
    active_clip: Option<PixelRect>,
    surface: PixelRect,
) -> Option<PixelRect> {
    match bounds {
        RecolorBounds::Empty => None,
        RecolorBounds::Rect(rect) => Some(rect),
        RecolorBounds::Unbounded => Some(active_clip.unwrap_or(surface)),
    }
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

    /// The union of every part added, if any.
    fn rect(&self) -> Option<PixelRect> {
        self.union.rect()
    }
}

/// The scrollbar geometry a pane draws: the track at the right edge of its padded `chrome`,
/// `max(8 * scale, 1)` pixels wide and clamped to the chrome. Drawing and damage both read it.
pub(crate) fn pane_scrollbar_geometry(
    chrome: PaneRect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: ScrollbarMode,
    scale: f32,
) -> Option<sonicterm_render_model::boundary::ui::scrollbar::ScrollbarGeometry> {
    use sonicterm_render_model::boundary::ui::scrollbar;
    // Authored at 8 logical px and scaled with DPI so the bar keeps a constant physical size, min 1px.
    let width_px = (8.0 * scale).max(1.0);
    let rect = scrollbar::Rect::new(chrome.x, chrome.y, chrome.w, chrome.h);
    scrollbar::compute(viewport_rows, total_rows, view_top, rect, mode, width_px)
}

#[cfg(test)]
pub(crate) fn pane_scrollbar_identity<I>(mode: ScrollbarMode, panes: I) -> Vec<(u64, u16)>
where
    I: IntoIterator<Item = (u64, usize, u16, f32)>,
{
    let mut identity: Vec<_> = panes
        .into_iter()
        .map(|(id, scrollback, rows, alpha)| {
            (id, effective_scrollbar_bucket(mode, scrollback as u64, rows, alpha))
        })
        .collect();
    identity.sort_unstable_by_key(|(id, _)| *id);
    identity
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn dirty_rows_damage_rect_with_ink_pad<I>(
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    if cols == 0 || cell_w <= 0.0 || cell_h <= 0.0 || surface_w == 0 || surface_h == 0 {
        // When: cell metrics or `surface_w`/`surface_h` are degenerate, there are no drawable row pixels.
        return None;
    }
    let pane_bounds = pane_rect.intersect(PixelRect { x: 0, y: 0, w: surface_w, h: surface_h })?;
    // Include pane padding because negative glyph bearings can leave ink beyond the first cell.
    let left = (origin_x.floor() as i32).min(pane_bounds.x);
    let right = ((origin_x + f32::from(cols) * cell_w).ceil() as i32).max(pane_bounds.right());
    let row_w = (right - left).max(1) as u32;
    let mut damage = DamageRect::empty();
    for row in dirty_rows {
        let ink_pad = vertical_ink_pad.max(0.0);
        // Floor the start and ceil the next row edge so fractional metrics leave no unpainted seam.
        let top = (origin_y + row as f32 * cell_h - ink_pad).floor() as i32;
        let next_top = (origin_y + (row as f32 + 1.0) * cell_h + ink_pad).ceil() as i32;
        damage.add_clipped(
            PixelRect { x: left, y: top, w: row_w, h: (next_top - top).max(1) as u32 },
            pane_bounds,
        );
    }
    damage.rect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn pane_damage_rect_with_ink_pad<I>(
    is_alt: bool,
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    if is_alt {
        // When: `is_alt` is dirty, moved terminal content requires replacement of the whole clipped pane.
        return dirty_rows.into_iter().next().and_then(|_| {
            pane_rect.intersect(PixelRect { x: 0, y: 0, w: surface_w, h: surface_h })
        });
    }
    dirty_rows_damage_rect_with_ink_pad(
        dirty_rows,
        pane_rect,
        origin_x,
        origin_y,
        cols,
        cell_w,
        cell_h,
        vertical_ink_pad,
        surface_w,
        surface_h,
    )
}

/// Whether a plan in `mode` emits every visible row into its batches. Exhaustive, so a mode
/// that emits fewer rows must state its own ink-coverage rule before it compiles.
pub(crate) fn emits_every_visible_row(mode: RenderMode) -> bool {
    match mode {
        RenderMode::Full => true,
        RenderMode::Partial | RenderMode::Noop => false,
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
