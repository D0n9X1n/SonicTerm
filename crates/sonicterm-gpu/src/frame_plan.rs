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
    pub broadcast_receivers_hash: u64,
    pub inline_media_hash: u64,
    pub hovered_url_cells: Option<HoveredUrlCells>,
    pub process_privileged: bool,
    pub subpixel_aa: SubpixelAaMode,
    pub background: [u64; 4],
    pub style_rev: u64,
    pub renderer_hash: u64,
    pub overlay_active: bool,
}

/// Structural pane identity preserves order, revision, requested viewport, and resolved projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneIdentity {
    pub id: u64,
    pub revision: u64,
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
    fn same_projection(&self, other: &Self) -> bool {
        // Grid revisions alone use dirty-row damage; every other pane identity change invalidates its projection.
        Self { revision: other.revision, ..*self } == *other
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
}

/// Visible-pane metadata contains row indices but no cells or hidden-history payloads.
#[derive(Debug, Clone)]
pub(crate) struct PaneMetadata {
    pub id: u64,
    pub revision: u64,
    pub rect: PixelRect,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_len: u64,
    pub viewport_top_abs: Option<u64>,
    pub is_active: bool,
    pub is_alt: bool,
    pub scrollbar_alpha: f32,
    pub dirty_rows: Vec<usize>,
}

/// Resolved physical geometry and row slots consumed with the caller's original grid borrow.
#[derive(Debug)]
pub(crate) struct PlannedPane {
    pub id: u64,
    pub expected_revision: u64,
    pub full_rect: PixelRect,
    pub full_clip: Option<PixelRect>,
    pub layout: PaneRect,
    pub content_clip: PaneRect,
    pub view_top_abs: u64,
    pub scrollback_len: u64,
    pub cols: u16,
    pub row_count: u16,
    pub background_cols: u16,
    pub background_rows: u16,
    pub is_active: bool,
    pub scrollbar_alpha: f32,
    pub dirty_rows: Vec<usize>,
}

impl PlannedPane {
    /// Iterate resolved viewport slots without retaining copied rows or hidden-history entries.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = (u16, u64)> + '_ {
        (0..self.row_count).map(|slot| (slot, self.view_top_abs.saturating_add(u64::from(slot))))
    }
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
        let mut dirt = DamageRect::empty();
        let mut damaged_rows = 0;
        let [left, right, top, bottom] = facts.padding;
        for input in inputs {
            let origin_x = input.rect.x as f32 + left;
            let origin_y = input.rect.y as f32 + top;
            let content_w = (input.rect.w as f32 - left - right).max(0.0);
            let content_h = (input.rect.h as f32 - top - bottom).max(0.0);
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
            if let Some(rect) = pane_damage_rect_with_ink_pad(
                input.is_alt,
                input.dirty_rows.iter().copied(),
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
            damaged_rows += input.dirty_rows.len();
            panes.push(PlannedPane {
                id: input.id,
                expected_revision: input.revision,
                full_rect: input.rect,
                full_clip: input.rect.intersect(surface),
                layout,
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
                scrollbar_alpha: input.scrollbar_alpha,
                dirty_rows: input.dirty_rows,
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
        let chrome_changed = previous.is_none_or(|previous| {
            previous.window != key.window
                || previous.metrics != key.metrics
                || previous.padding != key.padding
                || previous.degraded != key.degraded
                || previous.scrollbar_mode != key.scrollbar_mode
                || previous.panes.len() != key.panes.len()
                || previous
                    .panes
                    .iter()
                    .zip(&key.panes)
                    .any(|(before, after)| !before.same_projection(after))
        });
        if chrome_changed {
            dirt.add_clipped(surface, surface);
        }
        let mode = if unchanged {
            RenderMode::Noop
        } else {
            // When: `unchanged` is false, compose redraw policy using the same key and damage planned for execution.
            decide_render_mode(
                facts.degraded,
                RenderSignals {
                    first_frame,
                    overlay_active_or_toggled: chrome_changed || key.window.overlay_active,
                    dirty_damage: dirt.rect(),
                    ..Default::default()
                },
            )
        };
        let damage = if first_frame || (facts.degraded && mode == RenderMode::Full) {
            surface
        } else {
            // When: `first_frame` is false outside a degraded full repaint, replace only the composed `dirt`.
            dirt.rect().unwrap_or(surface)
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
        }
    }

    /// Authorize acknowledgement only for the exact planned pane and revision after a full presentation.
    pub(crate) fn acknowledges(&self, index: usize, id: u64, revision: u64) -> bool {
        self.mode == RenderMode::Full
            && self
                .panes
                .get(index)
                .is_some_and(|pane| pane.id == id && pane.expected_revision == revision)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderMode {
    Full,
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
