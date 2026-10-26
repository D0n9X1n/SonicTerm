//! Borrowed geometry and UI inputs shared by ordered CPU-only assembly phases.

use super::*;

/// One planned pane borrowed only while the source holds its grid.
pub(super) struct AssemblyPaneView<'frame> {
    pub(super) grid: &'frame Grid,
    pub(super) planned: &'frame PlannedPane,
    pub(super) pane_id: u64,
    pub(super) origin_x: f32,
    pub(super) origin_y: f32,
    pub(super) rect_w: f32,
    pub(super) rect_h: f32,
    pub(super) scrollbar_alpha: f32,
    pub(super) inline_images: &'frame [sonicterm_render_model::InlineImage],
}

/// Immutable inputs copied into each phase without cloning frame or terminal state.
#[derive(Clone, Copy)]
pub(super) struct AssemblyGeometry<'frame> {
    pub(super) fg_default: ChromeColor,
    pub(super) pane_views: &'frame [AssemblyPaneView<'frame>],
    pub(super) grid: &'frame Grid,
    pub(super) plan: &'frame FramePlan,
    pub(super) active_origin_x: f32,
    pub(super) active_origin_y: f32,
    pub(super) active_pane_x: f32,
    pub(super) active_pane_y: f32,
    pub(super) active_pane_width_px: f32,
    pub(super) active_pane_height_px: f32,
    pub(super) cell_width_px: f32,
    pub(super) cell_height_px: f32,
    pub(super) baseline_y_in_cell: f32,
    pub(super) raster_px: f32,
    pub(super) software_presenter: bool,
    pub(super) surface_width_px: f32,
    pub(super) surface_height_px: f32,
}

/// Immutable inputs copied into each phase without cloning frame or terminal state.
#[derive(Clone, Copy)]
pub(super) struct AssemblyUi<'frame> {
    pub(super) theme: &'frame Theme,
    pub(super) cursor_visible: bool,
    pub(super) selection: Option<&'frame Selection>,
    pub(super) copy_mode: Option<&'frame CopyModeState>,
    pub(super) tabs: &'frame TabBar,
    pub(super) process_privileged: bool,
    pub(super) search: Option<&'frame SearchState>,
    pub(super) ime: Option<&'frame ImeState>,
    pub(super) notification: Option<&'frame NotificationBubble>,
    pub(super) hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    pub(super) link_preview: Option<&'frame sonicterm_render_model::inputs::LinkPreview>,
    pub(super) read_only_mode: bool,
    pub(super) quick_select_hint_count: u32,
    pub(super) hover_tab_idx: u32,
    pub(super) tab_now: Instant,
}

/// Search placement carried forward into notifications, palette fields and terminal IME.
pub(super) struct AssemblyFields {
    pub(super) read_only_badge: Option<(f32, f32, f32, f32)>,
    pub(super) search_bar_layout: Option<SearchBarLayout>,
    pub(super) field_environment: u64,
    pub(super) field_candidates: PresentedFields,
    pub(super) field_selection_bg: [f32; 4],
    pub(super) field_selection_fg: [f32; 4],
    pub(super) search_ime_anchor: Option<(f32, f32, f32)>,
}
