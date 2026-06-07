//! GPU renderer for the terminal grid using wgpu 29.
//!
//! T13+T14 (wezterm-takeover G3): the legacy `the legacy chrome layer` chrome path is
//! gone. Every chrome string (tab titles, palette, search, IME,
//! broadcast, drag chip, quick-select hints) flows through
//! [`crate::chrome_text::layout`] → the shared `GlyphAtlas` →
//! [`crate::wezterm_pipeline::WeztermPipeline`]. No second font system,
//! no second atlas, no second render pass.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use sonicterm_cfg::config::BackdropKind;
use sonicterm_cfg::theme::{Color as ThemeColor, Theme};
use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid, UnderlineStyle};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, Operations, PresentMode, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, SurfaceConfiguration, TextureFormat, TextureUsages,
    TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::chrome_text::{self, ChromeAttrs, ChromeClip};
use crate::color::{
    chrome_color_to_linear_rgba, hex_to_chrome_color, hex_to_rgba, hex_to_wgpu_with_alpha,
    ChromeColor,
};
use crate::cursor::{recolor_cursor_glyphs, InactivePaneCursor};
use sonicterm_ui::drag_chip::{DragChipOverlay, DragChipVisual};
use sonicterm_ui::tab_spans::tab_title_font_size;

const PANE_FOCUS_FLASH_DURATION: Duration = Duration::from_millis(360);
const PANE_FOCUS_FLASH_BUCKET: Duration = Duration::from_millis(16);
const READ_ONLY_BADGE_ICON: &str = "";
const READ_ONLY_BADGE_LABEL: &str = "READONLY";
const SEARCH_BADGE_ICON: &str = "";
const READ_ONLY_BADGE_W: f32 = 180.0;
const READ_ONLY_BADGE_H: f32 = SEARCH_BAR_HEIGHT;
const READ_ONLY_BADGE_MARGIN: f32 = 12.0;
const READ_ONLY_BADGE_PAD_RIGHT: f32 = 20.0;
const READ_ONLY_BADGE_BASELINE_NUDGE_Y: f32 = -2.0;
const READ_ONLY_BADGE_RADIUS: f32 = 7.0;

/// Renderer compositor settings that affect surface configuration.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceAppearance {
    /// System backdrop material requested by config.
    pub backdrop: BackdropKind,
    /// Theme background opacity.
    pub opacity: f32,
    /// Scrollbar visibility policy (#386 PR-B). `Auto` and `Always` both
    /// draw the bar when the pane has scrollback beyond the viewport;
    /// `Never` suppresses it. Hover-driven auto-hide for `Auto` is
    /// deferred to PR-D — until then `Auto` behaves like Always-when-
    /// scrollable.
    pub scrollbar: sonicterm_cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    pub panel_padding: f32,
}

fn estimate_badge_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

/// Renderer initialization settings derived from config.
#[derive(Debug, Clone, Copy)]
pub struct RendererSettings<'a> {
    /// Font family to use for terminal text.
    pub font_family: &'a str,
    /// Font size in points.
    pub font_size: f32,
    /// Line-height multiplier.
    pub line_height_mult: f32,
    /// Window padding in logical pixels: left, right, top, bottom.
    pub padding: [f32; 4],
    /// Surface/backdrop settings.
    pub appearance: SurfaceAppearance,
}

fn splitter_color_from_theme(theme: &Theme) -> [f32; 4] {
    let bg = theme.colors.background.color().unwrap_or_else(|| ThemeColor::rgb(0, 0, 0));
    let fg = theme.colors.foreground.color().unwrap_or_else(|| ThemeColor::rgb(255, 255, 255));
    bg.shift_toward(fg, 0.18).to_rgba_f32_linear(1.0)
}

/// Resolve a scrollbar tint from the theme foreground at `derived_alpha`.
/// Theme-customizable explicit scrollbar colors are intentionally deferred
/// to a later PR (would require updating ~50 `Palette { .. }` literals in
/// tests for no shipped benefit yet). Returns straight-alpha linear RGBA;
/// the caller premultiplies before stuffing into a [`QuadInstance`].
fn scrollbar_tint(fg: &str, derived_alpha: f32) -> [f32; 4] {
    hex_to_rgba(fg, derived_alpha)
}

fn read_only_badge_rect(sw: f32, sh: f32) -> (f32, f32, f32, f32) {
    let w = READ_ONLY_BADGE_W.min((sw - READ_ONLY_BADGE_MARGIN * 2.0).max(40.0));
    let h = READ_ONLY_BADGE_H.min((sh - READ_ONLY_BADGE_MARGIN * 2.0).max(20.0));
    let x = (sw - w - READ_ONLY_BADGE_MARGIN).max(0.0);
    let y = READ_ONLY_BADGE_MARGIN.min((sh - h).max(0.0));
    (x, y, w, h)
}

/// Emit a pane's scrollbar (track + thumb) into `quads_overlay` using the
/// PR-A geometry model. No-op when the pane has nothing to scroll, the
/// mode is `Never`, or `alpha` is at or below the emit floor (PR-D).
/// Returns the number of quads emitted (for tests).
///
/// `alpha` in `[0.0, 1.0]` scales both track + thumb tint alphas; the
/// caller (app loop) feeds the lerped per-pane fade value from
/// `sonicterm_app::app::scrollbar_visibility::tick`.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_pane_scrollbar(
    quads_overlay: &mut Vec<QuadInstance>,
    pane_rect: PaneRect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: sonicterm_cfg::config::ScrollbarMode,
    theme: &Theme,
    sw: f32,
    sh: f32,
    alpha: f32,
) -> usize {
    // PR-D: hidden / nearly-hidden early-out. Mirrors
    // `scrollbar_visibility::ALPHA_EMIT_FLOOR`.
    if alpha <= 0.01 {
        return 0;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    // Bar width in logical px. Held local to the emitter; PR-D may lift
    // this into config once hover-driven width animation lands.
    const SCROLLBAR_WIDTH_PX: f32 = 8.0;
    let geom_rect =
        sonicterm_ui::scrollbar::Rect::new(pane_rect.x, pane_rect.y, pane_rect.w, pane_rect.h);
    let Some(geom) = sonicterm_ui::scrollbar::compute(
        viewport_rows,
        total_rows,
        view_top,
        geom_rect,
        mode,
        SCROLLBAR_WIDTH_PX,
    ) else {
        return 0;
    };
    let fg_hex = theme.colors.foreground.0.as_str();
    let track_color = premultiply(scrollbar_tint(fg_hex, 0.10 * alpha));
    let thumb_color = premultiply(scrollbar_tint(fg_hex, 0.30 * alpha));
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.track_rect.x,
            geom.track_rect.y,
            geom.track_rect.w,
            geom.track_rect.h,
            sw,
            sh,
        ),
        track_color,
    ));
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.thumb_rect.x,
            geom.thumb_rect.y,
            geom.thumb_rect.w,
            geom.thumb_rect.h,
            sw,
            sh,
        ),
        thumb_color,
    ));
    2
}

fn splitter_rects_from_panes(pane_rects: &[(u64, PaneRect)], thickness: f32) -> Vec<SplitterRect> {
    let mut out = Vec::new();
    let thickness = thickness.max(0.0);
    let eps = 0.5_f32;

    for (i, (_, a)) in pane_rects.iter().enumerate() {
        for (_, b) in pane_rects.iter().skip(i + 1) {
            let vertical_overlap = a.y.max(b.y) < (a.y + a.h).min(b.y + b.h) - eps;
            if vertical_overlap && ((a.x + a.w) - b.x).abs() <= eps {
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(b.x - thickness * 0.5, y, thickness, h),
                });
            } else if vertical_overlap && ((b.x + b.w) - a.x).abs() <= eps {
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(a.x - thickness * 0.5, y, thickness, h),
                });
            }

            let horizontal_overlap = a.x.max(b.x) < (a.x + a.w).min(b.x + b.w) - eps;
            if horizontal_overlap && ((a.y + a.h) - b.y).abs() <= eps {
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, b.y - thickness * 0.5, w, thickness),
                });
            } else if horizontal_overlap && ((b.y + b.h) - a.y).abs() <= eps {
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, a.y - thickness * 0.5, w, thickness),
                });
            }
        }
    }

    out
}

use crate::{
    atlas_upload::AtlasUpload,
    quad::{premultiply, px_to_ndc, QuadInstance},
    wezterm_pipeline::WeztermPipeline,
};
use sonicterm_cfg::config::CursorShape;
use sonicterm_text::GlyphInstance;
use sonicterm_text::{
    glyph_atlas::GlyphAtlas,
    // T9 (wezterm-takeover G2/C): `shape_run` + `ShapeCache` deleted in
    // T8 (the cosmic-text adapter is gone). `flush_shape_run` now drives
    // `shape_run_with_wezterm` directly; `ShapedGlyph::from_wezterm`
    // narrows wezterm's `GlyphInfo` into the renderer-facing record.
    // The legacy ASCII fast-path gate (`run_is_ascii_fast`) still
    // applies — it's purely cell-shape based and not tied to shaper
    // choice.
    //
    // T13/T14 (wezterm-takeover G3): `swash_rasterizer` is no longer
    // imported here — every chrome site and the grid path both route
    // through `sonicterm_engine::FontStack`. T10 deletes the
    // file outright.
    shape::{run_is_ascii_fast, RunStyle},
};
use sonicterm_ui::{
    cheatsheet::{filter_indices, CheatsheetState},
    command_palette::CommandPalette,
    copy_mode::{CopyModeState, QuickSelectState},
    cursor as ui_cursor,
    ime::ImeState,
    overlays::{
        search_bar_label, ImePreeditLayout, PaletteLayout, SearchBarLayout, PALETTE_BORDER,
        PALETTE_PANEL_RADIUS, PALETTE_QUERY_RADIUS, PALETTE_ROW_GAP, PALETTE_ROW_HEIGHT,
        PALETTE_ROW_RADIUS, SEARCH_BAR_HEIGHT, SEARCH_BAR_ICON_GAP, SEARCH_BAR_PAD_LEFT,
        SEARCH_BAR_PAD_RIGHT,
    },
    pane::{Rect as PaneRect, SplitAxis, SplitterRect},
    search::SearchState,
    selection::Selection,
    tabbar_view::{tab_bar_height, TabBarLayout, TAB_GAP},
    tabs::TabBar,
};

/// Style and sizing inputs for tab-bar quad emission.
pub struct TabBarQuadParams {
    /// Number of tabs in the bar.
    pub tab_count: usize,
    /// Active tab accent color.
    pub accent: [f32; 4],
    /// Inactive tab separator color.
    pub separator: [f32; 4],
    /// Bar background and bottom border color.
    pub border: [f32; 4],
    /// Hovered tab index, or `u32::MAX` when no tab is hovered.
    pub hover_tab_idx: u32,
    /// Surface dimensions in the same units as the layout rects.
    pub surface: (f32, f32),
}

/// Paint the tab-bar background and tab chrome quads into `quads`.
pub fn emit_tab_bar_quads(
    quads: &mut Vec<QuadInstance>,
    layout: &TabBarLayout,
    params: &TabBarQuadParams,
) {
    let (sw, sh) = params.surface;
    quads.push(QuadInstance {
        rect: px_to_ndc(layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h, sw, sh),
        color: params.border,
        ..Default::default()
    });
    quads.push(QuadInstance {
        rect: px_to_ndc(layout.bar.x, layout.bar.y + layout.bar.h - 1.0, layout.bar.w, 1.0, sw, sh),
        color: params.border,
        ..Default::default()
    });
    for t in &layout.tabs {
        let is_active = layout.active == Some(t.idx);
        if is_active {
            if let Some(acc) = layout.active_accent_rect() {
                quads.push(QuadInstance {
                    rect: px_to_ndc(acc.x, acc.y, acc.w, acc.h, sw, sh),
                    color: params.accent,
                    ..Default::default()
                });
            }
        }
        if t.idx + 1 < params.tab_count {
            // Geometric scale = bar.h / default-logical-bar-h. Mirrors
            // the per-bar-height scale `TabBarLayout::compute_at_y`
            // uses to grow TAB_GAP / padding with bar height — keeps
            // separators centered in each adjacent-tab gap.
            let scale = (layout.bar.h / 40.0).max(0.1);
            let sep_w = 1.0_f32 * scale;
            let sep_h = (layout.bar.h - 16.0 * scale).max(1.0);
            let sep_y = layout.bar.y + (layout.bar.h - sep_h) * 0.5;
            let gap_mid = t.bg_rect.x + t.bg_rect.w + (TAB_GAP * scale - sep_w) * 0.5;
            quads.push(QuadInstance {
                rect: px_to_ndc(gap_mid, sep_y, sep_w, sep_h, sw, sh),
                color: params.separator,
                ..Default::default()
            });
        }
    }
}

struct CheatsheetLayout {
    scrim: sonicterm_ui::tabbar_view::Rect,
    border: sonicterm_ui::tabbar_view::Rect,
    bg: sonicterm_ui::tabbar_view::Rect,
    query_row: sonicterm_ui::tabbar_view::Rect,
    rows: Vec<sonicterm_ui::tabbar_view::Rect>,
    selected_row: Option<usize>,
    query_label: String,
    row_labels: Vec<String>,
    footer: sonicterm_ui::tabbar_view::Rect,
    footer_label: String,
}

fn compute_cheatsheet_layout(
    state: &CheatsheetState,
    bindings: &[(String, String)],
    window_w: f32,
    window_h: f32,
    panel_padding: f32,
) -> CheatsheetLayout {
    let panel_padding = panel_padding.max(0.0);
    let modal_w = 760.0_f32.min((window_w - 48.0).max(180.0));
    let modal_h = 520.0_f32.min((window_h - 96.0).max(140.0));
    let border = sonicterm_ui::tabbar_view::Rect {
        x: ((window_w - modal_w) * 0.5).max(0.0),
        y: (window_h * 0.14).max(48.0).min((window_h - modal_h).max(0.0)),
        w: modal_w,
        h: modal_h,
    };
    let bg = sonicterm_ui::tabbar_view::Rect {
        x: border.x + PALETTE_BORDER,
        y: border.y + PALETTE_BORDER,
        w: (border.w - PALETTE_BORDER * 2.0).max(0.0),
        h: (border.h - PALETTE_BORDER * 2.0).max(0.0),
    };
    let query_row = sonicterm_ui::tabbar_view::Rect {
        x: bg.x + panel_padding,
        y: bg.y + panel_padding,
        w: (bg.w - panel_padding * 2.0).max(0.0),
        h: 44.0,
    };
    let footer = sonicterm_ui::tabbar_view::Rect {
        x: bg.x,
        y: (bg.y + bg.h - 32.0).max(query_row.y + query_row.h),
        w: bg.w,
        h: 32.0,
    };
    let list_top = query_row.y + query_row.h + panel_padding;
    let list_bottom = footer.y - panel_padding;
    let row_stride = PALETTE_ROW_HEIGHT + PALETTE_ROW_GAP;
    let max_rows = (((list_bottom - list_top).max(0.0) + PALETTE_ROW_GAP) / row_stride)
        .floor()
        .max(0.0) as usize;

    let idxs = filter_indices(bindings, &state.query);
    let total = idxs.len();
    let selected = state.selected_idx.min(total.saturating_sub(1));
    let window_start = selected.saturating_sub(max_rows.saturating_sub(1));
    let window_end = (window_start + max_rows).min(total);
    let selected_row = (total > 0).then_some(selected - window_start);

    let mut rows = Vec::with_capacity(window_end.saturating_sub(window_start));
    let mut row_labels = Vec::new();
    for (row_i, idx_pos) in (window_start..window_end).enumerate() {
        rows.push(sonicterm_ui::tabbar_view::Rect {
            x: bg.x + panel_padding,
            y: list_top + (row_i as f32) * row_stride,
            w: (bg.w - panel_padding * 2.0).max(0.0),
            h: PALETTE_ROW_HEIGHT,
        });
        if let Some((keys, action)) = idxs.get(idx_pos).and_then(|idx| bindings.get(*idx)) {
            row_labels.push(format!("{keys}    {action}"));
        }
    }
    if total == 0 {
        rows.push(sonicterm_ui::tabbar_view::Rect {
            x: bg.x + panel_padding,
            y: list_top,
            w: (bg.w - panel_padding * 2.0).max(0.0),
            h: PALETTE_ROW_HEIGHT,
        });
        row_labels.push("No shortcuts found".to_string());
    }

    let query_label = if state.query.is_empty() {
        "Search keyboard shortcuts… ▏".to_string()
    } else {
        format!("{}▏", state.query)
    };
    let footer_label = format!(
        "{} shortcut{} · ↑↓ navigate · type to search · esc close",
        total,
        if total == 1 { "" } else { "s" }
    );

    CheatsheetLayout {
        scrim: sonicterm_ui::tabbar_view::Rect { x: 0.0, y: 0.0, w: window_w, h: window_h },
        border,
        bg,
        query_row,
        rows,
        selected_row,
        query_label,
        row_labels,
        footer,
        footer_label,
    }
}

// (Per-row cache + grid SpanDesc removed in the B3 cutover — the GPU
// atlas does an O(1) lookup per cell, so the bookkeeping is wasted
// work. Walking 80×40 ≈ 3 200 cells per frame stays well under a
// millisecond on the renderer thread.)

// GpuRenderer holds several wgpu / cosmic-text resources (`instance`,
// `font_system`, etc.) that exist purely to keep their owned allocations
// alive for the lifetime of the renderer — they're never read after
// construction. `#[allow(dead_code)]` documents that intent at the struct
// level; removing it would force per-field `_` prefixing which obscures
// what each handle is.
#[allow(dead_code)]
/// Top-level GPU-backed terminal renderer. Owns the wgpu surface, the
/// text + quad pipelines, the glyph atlas, font/shape caches, and all
/// per-frame layout / cursor / overlay state. One per OS window.
pub struct GpuRenderer {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: SurfaceConfiguration,
    window: Arc<Window>,

    /// WezTerm-style final presentation pipeline. It consumes every glyph and
    /// geometry primitive for a frame and emits one indexed draw stream.
    present_pipeline: WeztermPipeline,

    // B3 GPU text path for the terminal grid.
    glyph_atlas: GlyphAtlas,
    glyph_upload: AtlasUpload,

    font_family: String,
    font_size: f32,
    line_height: f32,
    /// DPI multiplier (e.g. 2.0 on Retina). Post-G1a (wezterm-takeover)
    /// the renderer is raster-px end-to-end, so draw and hit-test sites
    /// no longer multiply/divide by this; its sole job is sizing the
    /// glyph rasterizer target. Stored, plumbed to `SwashRasterizer`,
    /// never used at the draw boundary.
    scale_factor: f32,
    /// Cell width in raster pixels (one terminal column). Sourced from
    /// `FontStack::cell_metrics_raster_px()` so sonicterm-font metrics
    /// drop in without a unit conversion.
    pub cell_w: f32,
    /// Cell height in raster pixels (one terminal row).
    pub cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    padding_top: f32,
    padding_bottom: f32,
    bg: wgpu::Color,
    bg_opacity: f32,
    /// Scrollbar visibility policy from config (PR-B of #386). Read on
    /// every frame in the per-pane scrollbar emit loop.
    scrollbar_mode: sonicterm_cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    panel_padding: f32,
    fg_default: ChromeColor,
    cursor_color: [f32; 4],
    /// Theme background as straight RGBA. Used to recolor the glyph
    /// under a block cursor so the foreground inverts to bg (wezterm
    /// parity). Pre-converted once per theme change to avoid the
    /// wgpu::Color → [f32;4] round-trip on every frame.
    bg_rgba: [f32; 4],
    /// Visual style of the text cursor (block / bar / underline).
    /// Live-updated from config; see [`Self::set_cursor_shape`].
    cursor_shape: CursorShape,
    /// Whether the text cursor blinks. When `false` the cursor renders
    /// at solid alpha and the FrameKey ignores the phase bucket.
    cursor_blink: bool,
    /// Anchor for the blink phase. Reset on every config change so the
    /// user sees the cursor at full brightness immediately after they
    /// toggle the setting (rather than wherever the cycle happened to
    /// be at the time).
    blink_epoch: Instant,
    /// Whether the OS window currently holds keyboard focus. The text
    /// cursor is hidden while the window is inactive. Defaults to
    /// `true` so a freshly created renderer draws the cursor on the
    /// very first frame, before winit has a chance to deliver
    /// `Focused(true)`.
    window_focused: bool,
    /// Cursor positions inside inactive panes (panes that share the
    /// window with the active pane but don't currently own keyboard
    /// focus). Kept as a compatibility sink for the app-side plumbing;
    /// inactive pane cursors are no longer drawn.
    inactive_pane_cursors: Vec<InactivePaneCursor>,
    /// Short-lived focus confirmation animation for the pane that just
    /// became active. Cleared automatically after
    /// [`PANE_FOCUS_FLASH_DURATION`].
    pane_focus_flash: Option<(u64, Instant)>,
    selection_color: [f32; 4],
    tab_bar_bg: [f32; 4],
    tab_active_bg: [f32; 4],
    tab_inactive_bg: [f32; 4],
    tab_active_fg: ChromeColor,
    tab_inactive_fg: ChromeColor,
    /// Deprecated user override for the removed tab close button. Kept
    /// only so older configs round-trip without changing the renderer
    /// settings surface.
    tab_close_override: Option<[f32; 4]>,
    /// Last reported cursor position in LOGICAL pixels, or `None` when
    /// the cursor is outside the window. Drives tab hover state.
    hover_cursor: Option<(f32, f32)>,
    /// Color for the wezterm-style vertical bar drawn between adjacent
    /// inactive tabs. A dim variant of the inactive-fg works in every
    /// theme; we precompute it here so the per-frame render path stays
    /// allocation-free.
    tab_separator: [f32; 4],
    hyperlink_underline: [f32; 4],
    splitter_color: [f32; 4],
    hyperlink_tint: [f32; 4],
    search_highlight: [f32; 4],
    search_highlight_current: [f32; 4],
    search_fg: ChromeColor,
    search_bg: [f32; 4],
    // T13/T14 (wezterm-takeover G3): the 11 `*_buffer: legacy chrome buffer`
    // fields that lived here (search, quick_select, palette_{query,rows,
    // footer}, cheatsheet_{query,rows,footer}, ime, broadcast,
    // drag_chip) are gone. Every chrome string is now shaped on demand
    // inside `render()` via `chrome_text::layout(...)`; the resulting
    // glyph instances feed either `glyph_instances` (pre-overlay
    // chrome — tab titles, search status bar) or
    // `overlay_glyph_instances` (modal chrome — palette, cheatsheet,
    // IME preedit, drag-chip title). No per-renderer the legacy chrome layer buffer
    // state survives.
    /// Cached drag-chip rect from the last `render()` call (in logical
    /// pixels). `None` when no chip was drawn. Test-only diagnostic
    /// surfaced through [`Self::last_drag_chip_visual`].
    drag_chip_visual: Option<DragChipVisual>,
    /// Last rendered frame key — when the next frame would produce an
    /// identical key, render() short-circuits before any GPU work.
    last_frame_key: Option<FrameKey>,
    /// Cumulative count of frames skipped via the FrameKey fast-path.
    /// Exposed via tracing::trace for `RUST_LOG=trace` hit-rate dashboards.
    skipped_frames: u64,
    /// Whether the tab bar is currently shown. Toggled at runtime by the
    /// View → Toggle Tab Bar menu action; when `false`, [`Self::top_inset`]
    /// returns 0 and the tab bar draw block in [`Self::render`] is skipped.
    tab_bar_visible: bool,
    /// Reserved height (logical px) above the tab bar for the OS native
    /// titlebar. Kept at zero while SonicTerm uses the normal OS titlebar with a
    /// bottom-pinned tab bar.
    titlebar_inset: f32,
    /// Characters from the most recent `render()` call that the
    /// rasterizer could not produce a tile for (i.e. would draw as a
    /// tofu outline). Whitespace is excluded. Test-only diagnostic
    /// surfaced through [`Self::last_missing_tofu`]; production code
    /// must not depend on it.
    last_missing_chars: Vec<char>,
    // T9 (wezterm-takeover G2/C): the per-style-run `ShapeCache` was
    // deleted with the cosmic-text path in T8 (`shape.rs` is now a
    // thin sonicterm-font adapter). Per-row caching survives at the
    // higher-level `row_glyph_cache` layer below — that's the cache
    // that actually short-circuits the steady-state interactive
    // shell. Re-shaping a style run via sonicterm-font on a row-cache
    // miss is cheap relative to the bitmap rasterize + atlas insert
    // it precedes.
    /// T9 (wezterm-takeover G2/C): sonicterm-font driven shaper. Owns
    /// the cell metrics (`cell_metrics_raster_px()`), the resolved
    /// font fallback chain, and the `blocking_shape` entry point that
    /// `flush_shape_run` calls through `shape_run_with_wezterm`. The
    /// renderer keeps the `Option<...>` shape so test fixtures (no
    /// bundled fonts on disk) can still construct a `GpuRenderer`
    /// even though the grid path is degraded.
    pub(crate) font_stack: Option<sonicterm_engine::FontStack>,
    /// Per-row glyph cache (PR after #130). Stores the shaped
    /// `GlyphInstance`s, underline coalescing, and missing-tofu list
    /// for each visible row, keyed by absolute row index + a content
    /// hash. A row whose contents / style / selection-overlap haven't
    /// changed splices its cached output straight into the frame and
    /// skips the entire `flush_shape_run` walk.
    row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache,
    /// Per-row cache for background/underline/hyperlink-tint quads
    /// (Epic #300 Phase P2). Mirrors `row_glyph_cache` but for the
    /// `QuadInstance`s emitted by `emit_cell_bg_quads_clipped` — on a
    /// hit we splice the cached `Vec<QuadInstance>` straight into the
    /// frame's quad vector and skip the per-cell run-length-encode.
    line_quad_cache: crate::row_quad_cache::LineQuadCache,
    /// Per-pane origins recorded on the most recent `render()` call.
    /// `(pane_id, [origin_x_px, origin_y_px])` for every pane in the
    /// frame's pane slice. Test-only diagnostic surfaced through
    /// [`Self::last_emitted_origins`]; production code must not rely
    /// on it. Part B step 7 hook for the per-pane render integration
    /// test.
    last_emit_origins: Vec<(u64, [f32; 2])>,
    /// Per-pane logical-px layout snapshot recorded on the most recent
    /// `render()` call, in raster pixels (winit reports physical-px;
    /// post-G1a the renderer is raster-px end-to-end so no boundary
    /// conversion happens). Drives the pane-aware hit-test in
    /// [`Self::pixel_to_cell`] (#569) so clicks land on the correct
    /// pane and column even when the per-column edge cache
    /// (`snapped_cell_x`) has jitter at fractional DPI scales. Empty
    /// before the first render — callers must handle the fallback path.
    last_pane_layout: Vec<PaneLayoutSnapshot>,
    /// Monotonic counter bumped on theme / default-fg / default-bg
    /// changes. Folded into every `row_hash` so palette swaps
    /// invalidate cached colours without iterating the cache.
    style_rev: u64,
    /// Active drag-chip overlay: translucent rect drawn at the cursor
    /// while a tab is held. Cleared on release.
    drag_chip: Option<DragChipOverlay>,
    /// Optional async font fallback loader (Epic #300 P4 follow-up).
    /// When set, every transient `SwashRasterizer` built inside
    /// `render()` / `set_font` / `rebuild_for_scale` has the loader
    /// attached so misses on CJK / emoji / nerd-font codepoints fire a
    /// background `request_load` and, on completion, the loader's
    /// notifier fires `UserEvent::ClearShapeCache` on the winit
    /// `EventLoopProxy` plumbed in by `sonicterm-app`. Stays `None` in
    /// tests / examples that construct `GpuRenderer` without an event
    /// loop proxy (the existing tofu fallback path keeps working).
    // T13/T14: `async_fallback::AsyncFallbackLoader` is deleted with
    // the rest of the swash/cosmic-text family. sonicterm-font handles
    // CJK/emoji/Nerd-font fallback synchronously through its own
    // resolved fallback chain (vendor-* features), so no async hook
    // is plumbed here. The field stays as a placeholder so the
    // surrounding `Option<...>` pattern + `set_async_loader` /
    // `async_loader` getter API survive future plumbing without a
    // cross-crate breaking change.
    async_loader: Option<()>,
}

/// A compact fingerprint of every input that can affect the rendered
/// frame. If two consecutive frames produce an equal key the second one
/// is a no-op for the user, so the renderer skips text shaping, quad
/// rebuild and GPU submission entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameKey {
    grid_revision: u64,
    /// Per-pane grid revisions. Part B step 5: split panes each own a Grid,
    /// so a write to an inactive pane (e.g. background `tail -f`) must
    /// invalidate the cached frame even though `grid_revision` (active pane)
    /// is unchanged.
    pane_revs: Vec<(u64, u64, Option<u64>)>,
    selection: Option<Selection>,
    copy_mode: Option<CopyModeState>,
    quick_select_hint_count: u32,
    cursor_visible: bool,
    tab: u64,
    pane: u64,
    search_hash: u64,
    palette_hash: u64,
    ime_hash: u64,
    cheatsheet_hash: u64,
    width: u32,
    height: u32,
    tab_hash: u64,
    pane_rect_hash: u64,
    viewport_top_abs: Option<u64>,
    /// Cursor shape variant index — different shapes paint different
    /// pixels even for the same grid + same blink phase, so this MUST
    /// participate in the key.
    cursor_shape: u8,
    /// Whether the cursor is blinking. Folded into the key so flipping
    /// the setting invalidates the cached frame immediately.
    cursor_blink: bool,
    /// Quantised blink phase. `0` when blinking is disabled (see
    /// [`crate::cursor::phase_bucket`]).
    cursor_phase: u8,
    /// Whether the window has keyboard focus — toggles active cursor
    /// visibility.
    window_focused: bool,
    /// Quantized pane-focus flash phase. Folded into the key so the
    /// bounded flash can animate without reviving the old infinite
    /// heartbeat redraw loop.
    pane_focus_flash_bucket: u8,
    /// Index of the tab the cursor is currently over, or `u32::MAX`
    /// when the cursor is not over any tab. Moving between tabs must
    /// invalidate the cached frame for hover chrome.
    hover_tab: u32,
    /// Deprecated close-button hover bit. Always zero now that close
    /// buttons are no longer drawn; kept to avoid reshaping FrameKey.
    hover_close: u8,
    /// Deprecated close-button override bit. Kept so older config reload
    /// paths can still invalidate safely.
    close_override: u8,
    broadcast_receivers_hash: u64,
    inline_media_hash: u64,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabTitleGlyphDebug {
    pub raster_px: f32,
    pub rect: [f32; 4],
    pub px_size: [u32; 2],
}

/// Snapshot of one pane's layout in raster pixels, captured at the
/// end of each `render()` call. Used by [`GpuRenderer::pixel_to_cell`]
/// (#569) to (a) figure out which pane was clicked and (b) reconstruct
/// that pane's `snapped_cell_x` edge cache on-demand so the column
/// search uses the same device-pixel-snapped edges the renderer drew.
///
/// Post-G1a (wezterm-takeover): all coordinates are in raster pixels,
/// the same unit winit reports for cursor input — no boundary
/// conversion takes place inside `pixel_to_cell`.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct PaneLayoutSnapshot {
    /// Stable id of the pane this snapshot describes.
    pub id: u64,
    /// Raster-px left edge of the pane (== that pane's `padding_left`
    /// equivalent — the origin `build_snapped_cell_x` was passed).
    pub origin_x_logical: f32,
    /// Raster-px top edge of the pane (already adjusted for tab-bar /
    /// top inset).
    pub origin_y_logical: f32,
    /// Raster-px width of the pane's content rect.
    pub w_logical: f32,
    /// Raster-px height of the pane's content rect.
    pub h_logical: f32,
    /// Cell width in raster pixels for the pane (currently identical
    /// across panes but kept per-pane for forward-compat with per-pane
    /// fonts).
    pub cell_w_logical: f32,
    /// Cell height in raster pixels for the pane.
    pub cell_h_logical: f32,
    /// Number of columns in the pane's grid at snapshot time.
    pub cols: u16,
    /// Number of rows in the pane's grid at snapshot time.
    pub rows: u16,
}

#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_tab_title_glyphs(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    raster_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    spans: &[(&str, ChromeColor, ChromeAttrs)],
    baseline_y: f32,
    avg_glyph_w: f32,
    sw: f32,
    sh: f32,
    glyph_instances: &mut Vec<GlyphInstance>,
    mut debug: Option<&mut Vec<TabTitleGlyphDebug>>,
) {
    // T14: chrome_text-driven port of the tab-title emit loop. Each
    // span is shaped through sonicterm-font and rasterized through the
    // same FontStack raster path the grid uses, so chrome and grid
    // share atlas tiles freely. The legacy SwashRasterizer +
    // cosmic-text `shape_run` path is gone (T10 deletes the
    // helpers entirely; T14 has already migrated this site off them).
    let mut pen_x: f32 = 0.0;
    for (text, color, attrs) in spans {
        if text.is_empty() {
            continue;
        }
        let layout = chrome_text::layout(
            font_stack,
            wt_raster,
            glyph_atlas,
            text,
            *color,
            *attrs,
            raster_px,
            native_em_px,
            (pen_x, baseline_y),
            (sw, sh),
            None,
        );
        let count_pre = glyph_instances.len();
        glyph_instances.extend(layout.glyphs.iter().copied());
        // Tab titles use `avg_glyph_w` columns × char count as the
        // logical layout stride (column-snapped), regardless of the
        // shaper's per-glyph advances. Preserves the existing
        // build_tab_title_spans column arithmetic that drives the
        // truncation / centering math upstream.
        let cols = text.chars().count() as f32;
        pen_x += cols * avg_glyph_w;
        if let Some(out) = debug.as_deref_mut() {
            for g in &glyph_instances[count_pre..] {
                // Tab-title debug records track only `raster_px` +
                // a rough px_size derived from the NDC quad height.
                let h = (-g.rect[3] * 0.5 * sh).abs();
                let w = (g.rect[2] * 0.5 * sw).abs();
                out.push(TabTitleGlyphDebug {
                    raster_px,
                    rect: [(g.rect[0] + 1.0) * 0.5 * sw, (1.0 - g.rect[1]) * 0.5 * sh, w, h],
                    px_size: [w as u32, h as u32],
                });
            }
        }
    }
}

/// Debug record emitted by [`emit_overlay_text_glyphs`] so tests can
/// assert the device-scaled atlas path was taken (see #384).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayTextGlyphDebug {
    pub raster_px: f32,
    pub font_size: f32,
    pub rect: [f32; 4],
    pub px_size: [u32; 2],
}

/// Emit overlay text (palette query / rows / footer, etc.) as
/// chrome_text-rendered glyph instances. Mirrors
/// [`emit_tab_title_glyphs`] but takes an explicit pixel `origin_x`
/// and `baseline_y` plus a clipping rect, so the caller can position
/// multi-line overlays (one call per line, advancing `baseline_y` by
/// `line_stride` each time).
///
/// Post-G1a (wezterm-takeover) and post-T14: every input is raster
/// px, the emitted instance rects are raster-px-derived NDC, and the
/// chrome path lives entirely in [`chrome_text::layout`] — no
/// SwashRasterizer, no cosmic-text shaper.
///
/// Glyphs whose rect falls entirely outside `bounds` are skipped so
/// the renderer doesn't paint outside the palette modal.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_overlay_text_glyphs(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    font_size_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    origin_x: f32,
    baseline_y: f32,
    bounds: [f32; 4], // [x, y, w, h] in raster px; glyphs outside are clipped
    sw: f32,
    sh: f32,
    glyph_instances: &mut Vec<GlyphInstance>,
    mut debug: Option<&mut Vec<OverlayTextGlyphDebug>>,
) {
    if text.is_empty() {
        return;
    }
    let [bx, by, bw, bh] = bounds;
    let layout = chrome_text::layout(
        font_stack,
        wt_raster,
        glyph_atlas,
        text,
        color,
        attrs,
        font_size_px,
        native_em_px,
        (origin_x, baseline_y),
        (sw, sh),
        Some(ChromeClip { x: bx, y: by, w: bw, h: bh }),
    );
    let count_pre = glyph_instances.len();
    glyph_instances.extend(layout.glyphs.iter().copied());
    if let Some(out) = debug.as_deref_mut() {
        for g in &glyph_instances[count_pre..] {
            let h = (-g.rect[3] * 0.5 * sh).abs();
            let w = (g.rect[2] * 0.5 * sw).abs();
            out.push(OverlayTextGlyphDebug {
                raster_px: font_size_px,
                font_size: font_size_px,
                rect: [(g.rect[0] + 1.0) * 0.5 * sw, (1.0 - g.rect[1]) * 0.5 * sh, w, h],
                px_size: [w as u32, h as u32],
            });
        }
    }
}
impl GpuRenderer {
    /// Build a renderer bound to `window`. Creates the wgpu surface +
    /// device + pipelines, the cosmic-text font system, the glyph atlas,
    /// and seeds the initial cell metrics from `theme`'s configured
    /// font family / size / line height.
    pub fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        pollster::block_on(Self::new_async(window, event_loop, theme, settings))
    }

    async fn new_async(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        // #536 profile: explicit Instant timing for gpu_renderer_new.
        // One of the two suspect cost centers motivating the
        // tear-out-spawn investigation. Using `Instant::now()` instead
        // of `info_span!.entered()` because the default
        // `sonicterm-logging` subscriber doesn't have
        // `with_span_events(FmtSpan::CLOSE)` configured, so span
        // timing would never be emitted.
        let __t_gpu_init = std::time::Instant::now();
        let RendererSettings { font_family, font_size, line_height_mult, padding, appearance } =
            settings;
        let [padding_left, padding_right, padding_top, padding_bottom] = padding;
        let size = window.inner_size();
        // G1a: read the OS DPI multiplier; stored verbatim into the
        // field below and only re-used by the rasterizer-target helper.
        let sf = window.scale_factor() as f32;
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone()).context("create surface")?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;
        let info = adapter.get_info();
        tracing::info!(
            backend = ?info.backend,
            name = %info.name,
            driver = %info.driver,
            "wgpu adapter selected"
        );
        if matches!(info.backend, wgpu::Backend::Gl) {
            tracing::warn!(
                adapter = %info.name,
                "GPU backend is GLES — rendering may differ from native D3D12/Metal. \
                 Glyph sharpness, Powerline anchoring, and HiDPI snap may behave \
                 unexpectedly. Common cause: running over RDP without GPU passthrough."
            );
        }
        let (device, queue) =
            adapter.request_device(&DeviceDescriptor::default()).await.context("request device")?;

        let format = TextureFormat::Bgra8UnormSrgb;
        // Prefer Mailbox when the backend exposes it: Mailbox drops in-flight
        // superseded frames so a fast-typing user always sees the newest
        // keystroke without waiting a full vblank. Fall back to Fifo on
        // backends that don't advertise Mailbox (Fifo is universally supported
        // and remains the spec-mandated default).
        let surface_caps = surface.get_capabilities(&adapter);
        let present_mode = if surface_caps.present_modes.contains(&PresentMode::Mailbox) {
            PresentMode::Mailbox
        } else {
            PresentMode::Fifo
        };
        let alpha_mode = if appearance.backdrop == BackdropKind::Opaque {
            CompositeAlphaMode::Opaque
        } else {
            CompositeAlphaMode::PreMultiplied
        };
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // B3 GPU text path. Allocate the CPU + GPU side of the glyph
        // atlas up front so the first frame can stream tiles into it.
        // T13/T14 (wezterm-takeover G3): no more SwashRasterizer
        // prebake — chrome and grid share this single atlas, populated
        // on demand by the wezterm rasterizer on every miss.
        let present_pipeline = WeztermPipeline::new(&device, format, 4096);
        let mut glyph_atlas = GlyphAtlas::default_size();
        let glyph_upload = AtlasUpload::new(
            &device,
            &queue,
            &glyph_atlas,
            present_pipeline.texture_bind_group_layout(),
        );
        let _ = (&mut glyph_atlas,); // touch so `mut` binding stays in scope for downstream warmup loops

        // G1a (T2) + T13: cell metrics come from sonicterm-font in raster
        // px directly. The cosmic-text `measure_cell` fallback is gone
        // — when the FontStack fails to load (test fixtures without
        // bundled fonts) we fall back to a font-size-derived guess
        // (`font_size * 0.6, font_size * 1.2`) that's close enough to
        // keep test fixtures rendering at a sensible aspect ratio.
        // FontStack DPI: sonicterm-font computes px_per_em = point_size *
        // dpi / 72. Pass `dpi = 72 * scale_factor` so the raster cell
        // metrics match the renderer's raster-px coordinate system.
        // Font size in points equals sonicterm's logical font_size.
        let fs_dpi = (72.0 * sf).round() as usize;
        let font_stack =
            sonicterm_engine::FontStack::try_new_full(font_family, font_size as f64, fs_dpi).ok();
        let (cell_w, natural_cell_h) =
            match font_stack.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (font_size * 0.6 * sf, font_size * 1.2 * sf),
            };
        let line_height = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let cell_h = line_height;

        let bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), appearance.opacity);
        let bg_rgba = hex_to_rgba(theme.colors.background.0.as_str(), 1.0);
        let fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 1.0);
        let selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        let tab_bar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        let tab_active_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        let tab_inactive_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        let tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        let tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        let tab_separator = hex_to_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        // Hyperlink visuals: theme-aware. Use the theme's cursor color as the
        // accent (every bundled theme designates it). Underline reads as
        // deliberate at high opacity; the tint behind the run is subtle.
        let hyperlink_underline = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.9);
        let splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonicterm_cfg::theme::Appearance::Dark => 0.14,
            sonicterm_cfg::theme::Appearance::Light => 0.10,
        };
        let hyperlink_tint = hex_to_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        let search_highlight = hex_to_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        // Current (selected) match draws in orange so it's distinguishable
        // from the other yellow matches at a glance.
        let search_highlight_current = [1.0, 0.5, 0.0, 0.55];
        let search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let search_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        // T13/T14: cosmic-text Buffer / Metrics allocations deleted.
        // Chrome strings are shape+raster'd on demand inside `render()`
        // through `chrome_text::layout(...)`; there is no persistent
        // per-overlay text buffer to size at construction.

        tracing::info!(elapsed = ?__t_gpu_init.elapsed(), "[perf] gpu_renderer_new");
        Ok(Self {
            instance,
            device,
            queue,
            surface,
            config,
            window,
            present_pipeline,
            glyph_atlas,
            glyph_upload,
            font_family: font_family.to_string(),
            font_size,
            line_height,
            scale_factor: sf,
            cell_w,
            cell_h,
            padding_left,
            padding_right,
            padding_top,
            padding_bottom,
            bg,
            bg_opacity: appearance.opacity.clamp(0.0, 1.0),
            scrollbar_mode: appearance.scrollbar,
            panel_padding: appearance.panel_padding.max(0.0),
            fg_default,
            cursor_color,
            bg_rgba,
            cursor_shape: CursorShape::default(),
            cursor_blink: true,
            blink_epoch: Instant::now(),
            window_focused: true,
            inactive_pane_cursors: Vec::new(),
            pane_focus_flash: None,
            selection_color,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            tab_active_fg,
            tab_inactive_fg,
            tab_close_override: None,
            hover_cursor: None,
            tab_separator,
            hyperlink_underline,
            splitter_color,
            hyperlink_tint,
            search_highlight,
            search_highlight_current,
            search_fg,
            search_bg,
            drag_chip_visual: None,
            last_frame_key: None,
            skipped_frames: 0,
            tab_bar_visible: true,
            titlebar_inset: 0.0,
            last_missing_chars: Vec::new(),
            // T9: `shape_cache` field deleted with the cosmic-text path.
            font_stack,
            row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            line_quad_cache: crate::row_quad_cache::LineQuadCache::new(),
            last_emit_origins: Vec::new(),
            last_pane_layout: Vec::new(),
            style_rev: 0,
            drag_chip: None,
            async_loader: None,
        })
    }

    /// Reconfigure the surface for a new physical window size in
    /// pixels. Clamps each dimension to ≥ 1 to keep wgpu happy on
    /// minimize. Forces the next frame to render fresh.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        // Geometry change → force the next frame to actually render.
        self.last_frame_key = None;
        // Cell layout and absolute-row positioning both change with
        // the surface size; cached glyph instances would land at the
        // wrong NDC coordinates.
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // T13/T14: post-the legacy chrome layer there is no persistent text buffer to
        // resize — chrome strings are re-shaped through
        // `chrome_text::layout` on every frame, picking up the new
        // surface dims via the per-call `(sw, sh)` parameter. The
        // legacy `*_buffer.set_size(...)` block that lived here is
        // gone with the the legacy chrome layer plumbing.
    }

    /// Top inset reserved above the grid: OS titlebar band (when active)
    /// plus top window padding, returned in **raster px** so it lives in
    /// the same coordinate system as `config.width`/`config.height` and the
    /// rest of the renderer post-G1a. The tab bar is always bottom-pinned,
    /// so its height is reserved via [`Self::bottom_inset`] instead of here.
    ///
    /// `titlebar_inset` and `padding_top` are stored in logical px (matching
    /// the config schema); both are scaled by [`Self::scale_factor`] before
    /// being summed so a 2x Retina display gets the right number of raster
    /// rows reserved for the OS titlebar band + user padding. Without the
    /// scale the grid was reporting one fewer row than the window could fit,
    /// leaving a dead strip below the last painted row that showed the
    /// surface clear color instead of vim's bg (#621).
    pub fn top_inset(&self) -> f32 {
        (self.titlebar_inset + self.padding_top) * self.scale_factor
    }

    /// Bottom inset reserved below the grid for the bottom-pinned tab bar,
    /// in **raster px** (same units as `config.height`). Returns 0 when
    /// the bar is hidden; the consumer still subtracts `padding_bottom *
    /// scale_factor` separately so window padding still applies when the
    /// bar is off.
    pub fn bottom_inset(&self) -> f32 {
        if self.tab_bar_visible {
            self.tab_bar_logical_height()
        } else {
            0.0
        }
    }

    /// Y offset (in raster px) at which the tab bar layout should be
    /// anchored. The tab bar is always pinned to the bottom of the window.
    /// Callers pass this into [`TabBarLayout::with_top_offset`].
    pub fn tab_bar_y_offset(&self) -> f32 {
        let surf_h = self.config.height as f32;
        (surf_h - self.tab_bar_logical_height()).max(0.0)
    }

    /// Raster-pixel height of the tab bar for the renderer's current font
    /// size. Derived from [`tab_bar_height`] (logical formula) and scaled
    /// to raster px to live in the same coordinate system as
    /// `config.width`/`config.height` and the rest of the renderer
    /// post-G1a. WezTerm fancy-mode parity: `font_size × 2 + 12` clamped.
    pub fn tab_bar_logical_height(&self) -> f32 {
        tab_bar_height(self.font_size) * self.scale_factor
    }

    /// The titlebar inset alone (logical px) — the y-offset at which the
    /// tab bar strip itself begins, regardless of whether the bar is
    /// visible. Used by hit-testing / tab-bar layout to shift their
    /// rectangles down so clicks under the OS titlebar are not consumed
    /// as tab activations.
    pub fn titlebar_inset(&self) -> f32 {
        self.titlebar_inset
    }

    /// Set the reserved OS titlebar band height (logical px). Called once
    /// from `app.rs` after creating each window so the renderer knows
    /// whether the macOS integrated-titlebar style is in effect.
    /// Invalidates the cached frame key so the next render() relays out.
    pub fn set_titlebar_inset(&mut self, inset: f32) {
        let clamped = inset.max(0.0);
        if (self.titlebar_inset - clamped).abs() < f32::EPSILON {
            return;
        }
        self.titlebar_inset = clamped;
        self.last_frame_key = None;
    }

    /// Show or hide the tab bar. Returns `true` if the visibility actually
    /// changed (so callers can decide whether to recompute grid dims).
    /// Invalidates the cached frame key so the next `render()` call rebuilds.
    pub fn set_tab_bar_visible(&mut self, visible: bool) -> bool {
        if self.tab_bar_visible == visible {
            return false;
        }
        self.tab_bar_visible = visible;
        self.last_frame_key = None;
        true
    }

    /// Whether the tab bar is currently shown.
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }

    /// Update scrollbar visibility policy from live config reload.
    ///
    /// `render()` folds this cached mode into the per-pane scrollbar emit
    /// path, so config changes must invalidate the frame key explicitly;
    /// otherwise an idle window could keep the previous scrollbar quads
    /// until some unrelated grid/theme/input change forced a redraw.
    pub fn set_scrollbar_mode(&mut self, mode: sonicterm_cfg::config::ScrollbarMode) -> bool {
        if self.scrollbar_mode == mode {
            return false;
        }
        self.scrollbar_mode = mode;
        self.last_frame_key = None;
        true
    }

    /// Current scrollbar visibility policy. Test-only inspector for the
    /// live-reload path; production code pushes updates via
    /// [`Self::set_scrollbar_mode`].
    #[doc(hidden)]
    pub fn scrollbar_mode(&self) -> sonicterm_cfg::config::ScrollbarMode {
        self.scrollbar_mode
    }

    /// Update overlay panel padding from live config reload.
    pub fn set_panel_padding(&mut self, padding: f32) -> bool {
        let padding = padding.max(0.0);
        if (self.panel_padding - padding).abs() < f32::EPSILON {
            return false;
        }
        self.panel_padding = padding;
        self.last_frame_key = None;
        true
    }

    /// Update the cursor shape. Invalidates the cached frame so the
    /// next render redraws with the new geometry.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        if self.cursor_shape == shape {
            return;
        }
        self.cursor_shape = shape;
        self.last_frame_key = None;
    }

    /// Current cursor shape.
    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    /// Enable or disable the cursor blink. Resets the blink phase so
    /// the user always sees a full-brightness cursor immediately after
    /// flipping the setting (no random mid-cycle pop).
    pub fn set_cursor_blink(&mut self, blink: bool) {
        if self.cursor_blink == blink {
            return;
        }
        self.cursor_blink = blink;
        self.blink_epoch = Instant::now();
        self.last_frame_key = None;
    }

    /// Whether the cursor is currently configured to blink.
    pub fn cursor_blink(&self) -> bool {
        self.cursor_blink
    }

    /// Suggested wall-clock interval between blink-only redraws. The
    /// app loop schedules a redraw at this cadence whenever the cursor
    /// is visible AND [`Self::cursor_blink`] is true; otherwise nothing
    /// new would render and the request would be wasted.
    pub fn blink_redraw_interval(&self) -> std::time::Duration {
        ui_cursor::redraw_interval()
    }

    /// Wall-clock instant at which the next blink phase bucket begins,
    /// or `None` when blinking is disabled. The app loop should set
    /// `ControlFlow::WaitUntil(this)` so the renderer wakes up exactly
    /// at bucket boundaries instead of busy-looping `request_redraw()`
    /// after every frame (the project landmine flagged on PR #81).
    pub fn next_blink_redraw_at(&self) -> Option<Instant> {
        // Blink-driven redraws are intentionally disabled in the idle
        // path. Re-shaping the grid 26×/sec just to fade the cursor
        // alpha melted the headless CPU bench at 17% — see the
        // `cursor_phase: 0` comment where `FrameKey` is built. The
        // cursor still re-evaluates its alpha on every real redraw
        // (PTY bytes, keys, mouse, resize, focus), which keeps it
        // visibly pulsing whenever the user is doing anything. Pure
        // idle leaves the cursor frozen at a fixed (always-visible)
        // alpha — strictly better than burning CPU on a backgrounded
        // window. The remaining fields (`cursor_blink`,
        // `window_focused`, `blink_epoch`) are kept so a future
        // event-driven re-enable (e.g. only blink for the first 5s
        // after a keypress) can pick the right starting bucket.
        let _ = (&self.cursor_blink, &self.window_focused, &self.blink_epoch);
        None
    }

    /// Update the cached "is the OS window focused" flag. Hides the
    /// text cursor when `false`. Bumps the FrameKey via
    /// [`Self::last_frame_key`] so the next render is not skipped by
    /// the cache.
    pub fn set_window_focused(&mut self, focused: bool) {
        if self.window_focused == focused {
            return;
        }
        self.window_focused = focused;
        self.last_frame_key = None;
    }

    /// Whether the OS window currently has keyboard focus.
    pub fn window_focused(&self) -> bool {
        self.window_focused
    }

    pub fn flash_pane_focus(&mut self, pane_id: u64) {
        self.pane_focus_flash = Some((pane_id, Instant::now()));
        self.last_frame_key = None;
        self.window.request_redraw();
    }

    /// Accept the historical per-frame inactive-pane cursor list.
    /// Inactive panes no longer draw cursors, so any previously cached
    /// cursor records are cleared and new records are ignored.
    pub fn set_inactive_pane_cursors(&mut self, _cursors: Vec<InactivePaneCursor>) {
        if !self.inactive_pane_cursors.is_empty() {
            self.inactive_pane_cursors.clear();
            self.last_frame_key = None;
        }
    }

    fn pane_focus_flash_bucket(&mut self, now: Instant) -> u8 {
        let Some((_, started_at)) = self.pane_focus_flash else {
            return 0;
        };
        let elapsed = now.saturating_duration_since(started_at);
        if elapsed >= PANE_FOCUS_FLASH_DURATION {
            self.pane_focus_flash = None;
            return 0;
        }
        ((elapsed.as_millis() / PANE_FOCUS_FLASH_BUCKET.as_millis()) + 1).min(u128::from(u8::MAX))
            as u8
    }

    fn pane_focus_flash_alpha(&self, now: Instant) -> Option<(u64, f32)> {
        let (pane_id, started_at) = self.pane_focus_flash?;
        let elapsed = now.saturating_duration_since(started_at);
        if elapsed >= PANE_FOCUS_FLASH_DURATION {
            return None;
        }
        let t = elapsed.as_secs_f32() / PANE_FOCUS_FLASH_DURATION.as_secs_f32();
        Some((pane_id, (1.0 - t).powi(2) * 0.12))
    }

    /// Current physical surface width in pixels.
    pub fn width(&self) -> u32 {
        self.config.width
    }

    /// Current physical surface height in pixels.
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// Left padding (logical px). Kept for backward compatibility with
    /// callers that pre-date per-side padding; new code should prefer
    /// the per-side accessors below.
    pub fn padding(&self) -> f32 {
        self.padding_left
    }

    /// Left padding in logical pixels.
    pub fn padding_left(&self) -> f32 {
        self.padding_left
    }
    /// Right padding in logical pixels.
    pub fn padding_right(&self) -> f32 {
        self.padding_right
    }
    /// Top padding in logical pixels (above any tab bar / titlebar inset).
    pub fn padding_top(&self) -> f32 {
        self.padding_top
    }
    /// Bottom padding in logical pixels.
    pub fn padding_bottom(&self) -> f32 {
        self.padding_bottom
    }

    /// Left padding scaled to **raster px**, i.e. the same coordinate
    /// system as `config.width`/`config.height` and the rest of the
    /// renderer post-G1a. Prefer this over [`Self::padding_left`] when
    /// building geometry that will be handed back to the renderer (e.g.
    /// the per-pane rect in `compute_pane_rects_for`). Mixing the
    /// logical-px accessor with raster surface dims off-by-ones the row
    /// count and leaves a dead strip below the last painted row (#621).
    pub fn padding_left_px(&self) -> f32 {
        self.padding_left * self.scale_factor
    }
    /// Right padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_right_px(&self) -> f32 {
        self.padding_right * self.scale_factor
    }
    /// Top padding scaled to raster px. See [`Self::padding_left_px`].
    /// Note: [`Self::top_inset`] already returns raster px (it bakes in
    /// the titlebar inset + this value); callers that want the full
    /// "y-origin of the grid" should use `top_inset()`, not this raw
    /// padding alone.
    pub fn padding_top_px(&self) -> f32 {
        self.padding_top * self.scale_factor
    }
    /// Bottom padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_bottom_px(&self) -> f32 {
        self.padding_bottom * self.scale_factor
    }
    /// Per-pane origins recorded by the most recent `render()` call, as
    /// `(pane_id, [origin_x_px, origin_y_px])`. Test-only hook for the
    /// Part B step 7 per-pane render integration test. Production code
    /// must not depend on this.
    #[doc(hidden)]
    pub fn last_emitted_origins(&self) -> Vec<(u64, [f32; 2])> {
        self.last_emit_origins.clone()
    }

    /// Translate a scrollback-absolute row into the row index visible in the
    /// current viewport. Returns `None` when the row lies above or below the
    /// rendered viewport.
    #[doc(hidden)]
    pub fn viewport_relative_row(
        absolute_row: usize,
        view_top_abs: u64,
        visible_rows: u16,
    ) -> Option<u16> {
        let visible_row = absolute_row as i128 - i128::from(view_top_abs);
        (0..i128::from(visible_rows)).contains(&visible_row).then_some(visible_row as u16)
    }

    /// Resolve the viewport top used by the renderer after clamping explicit
    /// scrollback requests to the live bottom.
    #[doc(hidden)]
    pub fn resolved_view_top_abs(grid: &Grid, viewport_top_abs: Option<u64>) -> u64 {
        let live_top_abs = grid.scrollback_len() as u64;
        viewport_top_abs.map(|v| v.min(live_top_abs)).unwrap_or(live_top_abs)
    }

    /// Legacy-Grid variant kept for sonicterm-app call sites that still
    /// hold an `Arc<Mutex<Parser>>` and want to ask viewport questions
    /// of the parser's grid. Identical algorithm to the GridFacade
    /// version; both will collapse to one helper once sonicterm-app
    /// stops carrying the legacy parser.
    #[doc(hidden)]
    pub fn resolved_view_top_abs_legacy(
        grid: &sonicterm_grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> u64 {
        let live_top_abs = grid.scrollback_len() as u64;
        viewport_top_abs.map(|v| v.min(live_top_abs)).unwrap_or(live_top_abs)
    }

    /// Adjust a viewport after copy-mode movement so the scrollback-absolute
    /// copy-mode cursor remains visible.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move(
        copy_mode: &CopyModeState,
        grid: &Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            viewport_top_abs
        }
    }

    /// Legacy-Grid variant. See `resolved_view_top_abs_legacy`.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move_legacy(
        copy_mode: &CopyModeState,
        grid: &sonicterm_grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            viewport_top_abs
        }
    }

    /// Emit copy-mode selection and cursor quads using scrollback-absolute
    /// copy-mode coordinates translated into viewport-relative rows.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn emit_copy_mode_quads(
        copy_mode: &CopyModeState,
        grid: &Grid,
        view_top_abs: u64,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
        sw: f32,
        sh: f32,
        selection_color: [f32; 4],
        cursor_color: [f32; 4],
        quads: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) -> Option<(f32, f32)> {
        // #489: derive selection-row x/w and copy-cursor cx from the
        // shared snapped-edge cache so copy-mode overlays share
        // device-pixel edges with adjacent glyph cells at fractional
        // DPI. Empty-cache fallback preserves the raw arithmetic for
        // callers (debug/test helpers) that don't carry a real cache;
        // integer scales make the two identical via the identity fast
        // path in `snap_to_device_pixels`.
        let raw_fallback = snapped_cell_x.is_empty();
        if let Some((start, end)) = copy_mode.selected_range() {
            for row_abs in start.1..=end.1 {
                let Some(visible_row) =
                    Self::viewport_relative_row(row_abs, view_top_abs, grid.rows)
                else {
                    continue;
                };
                let col_a = if row_abs == start.1 { start.0 } else { 0 }.min(grid.cols as usize);
                let col_b = if row_abs == end.1 {
                    end.0.min(grid.cols.saturating_sub(1) as usize)
                } else {
                    grid.cols.saturating_sub(1) as usize
                };
                if col_b < col_a {
                    continue;
                }
                let end_exclusive = col_b + 1;
                let (x, w) = if raw_fallback {
                    (origin_x + col_a as f32 * cell_w, (end_exclusive - col_a) as f32 * cell_w)
                } else {
                    let cache_end = end_exclusive.min(snapped_cell_x.len() - 1);
                    let lo = snapped_cell_x[col_a];
                    let hi = snapped_cell_x[cache_end];
                    (lo, hi - lo)
                };
                let y = origin_y + f32::from(visible_row) * cell_h;
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, cell_h, sw, sh),
                    color: selection_color,
                    ..Default::default()
                });
            }
        }

        if copy_mode.is_read_only() {
            return None;
        }

        let visible_row = Self::viewport_relative_row(copy_mode.cursor.1, view_top_abs, grid.rows)?;
        let copy_col = copy_mode.cursor.0.min(grid.cols.saturating_sub(1) as usize);
        let (cx, cw) = if raw_fallback {
            (origin_x + copy_col as f32 * cell_w, cell_w)
        } else {
            let lo = snapped_cell_x[copy_col];
            let hi = snapped_cell_x[(copy_col + 1).min(snapped_cell_x.len() - 1)];
            (lo, hi - lo)
        };
        let cy = origin_y + f32::from(visible_row) * cell_h;
        quads.push(QuadInstance {
            rect: px_to_ndc(cx, cy, cw, cell_h, sw, sh),
            color: cursor_color,
            ..Default::default()
        });
        Some((cx, cy))
    }

    /// PR #199 Fix 1 test hook: number of panes the most recent
    /// `render()` call received in its slice. The integration test
    /// asserts this equals the active tab's pane count so a regression
    /// to a single-element slice (the original bug) is caught
    /// mechanically. Production code must not depend on this.
    #[doc(hidden)]
    pub fn last_panes_received(&self) -> usize {
        self.last_emit_origins.len()
    }

    /// Update all four padding values at once (used by the live config
    /// reload path so editing `sonicterm.toml` takes effect without restart).
    /// Invalidates the cached frame so the next render relays out.
    pub fn set_padding(&mut self, padding: [f32; 4]) {
        let [l, r, t, b] = padding;
        if (self.padding_left - l).abs() < f32::EPSILON
            && (self.padding_right - r).abs() < f32::EPSILON
            && (self.padding_top - t).abs() < f32::EPSILON
            && (self.padding_bottom - b).abs() < f32::EPSILON
        {
            return;
        }
        self.padding_left = l;
        self.padding_right = r;
        self.padding_top = t;
        self.padding_bottom = b;
        self.last_frame_key = None;
    }

    /// Raster-pixel size of the render surface. Post-G1a (wezterm-takeover)
    /// the pane layout, padding, top inset, and cell metrics are all
    /// raster px too, so this is just `(config.width, config.height)`
    /// cast to `f32`. Name kept for back-compat with callers that
    /// were once unit-mixing.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }

    /// Snapshot of every codepoint the previous `render()` call could
    /// not produce a glyph tile for (i.e. that drew a tofu outline).
    /// Whitespace is filtered out — those are intentionally blank.
    ///
    /// Test-only diagnostic. Production code MUST NOT depend on this
    /// surface — it exists so the renderer-capability matrix can
    /// assert "no character class regressed" without sniffing pixels
    /// off the swapchain. Doc-hidden to keep it out of the public
    /// rustdoc; still `pub` so integration tests under `tests/` can
    /// reach it.
    #[doc(hidden)]
    pub fn last_missing_tofu(&self) -> &[char] {
        &self.last_missing_chars
    }

    /// Current grid dimensions in `(cols, rows)`. G1a: surface dims +
    /// cell_w / cell_h all share the raster-px coordinate system, so
    /// this is plain integer division — no DPI reconciliation step.
    ///
    /// Padding is stored in **logical px** (matching the config schema),
    /// so each side is scaled by [`Self::scale_factor`] before being
    /// subtracted from the raster-px surface dims. `top_inset()` and
    /// `bottom_inset()` already return raster px, so they're subtracted
    /// raw. Without the per-side scale the row count was off by ~1 on 2x
    /// Retina, which left a dead strip below the last painted row (#621).
    pub fn cells(&self) -> (u16, u16) {
        let surf_w = self.config.width as f32;
        let surf_h = self.config.height as f32;
        let sf = self.scale_factor;
        let inner_w = (surf_w - self.padding_left * sf - self.padding_right * sf).max(self.cell_w);
        let inner_h = (surf_h - self.top_inset() - self.bottom_inset() - self.padding_bottom * sf)
            .max(self.cell_h);
        let cols = (inner_w / self.cell_w).floor() as u16;
        let rows = (inner_h / self.cell_h).floor() as u16;
        (cols.max(1), rows.max(1))
    }

    /// Logical cell metrics (width, height) in CSS pixels. Pair with a
    /// `sonicterm_ui::pane::Rect` from `PaneTree::layout` to compute how many
    /// cells fit in that rect: `cols = (rect.w / cell_w).floor()`,
    /// similarly rows.
    ///
    /// Returned values are positive (the renderer asserts a positive glyph
    /// advance at font load).
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    /// Current font family in effect. Test-only inspector for the
    /// live-reload path; production code reads font fields directly.
    #[doc(hidden)]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    /// Current font size in px.
    #[doc(hidden)]
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Number of glyph tiles currently resident in the rasterizer atlas.
    /// Test-only; the atlas is cleared and rebuilt by [`Self::set_font`].
    #[doc(hidden)]
    pub fn glyph_atlas_len(&self) -> usize {
        self.glyph_atlas.len()
    }

    /// Apply a new font family / size / line-height multiplier without
    /// reconstructing the renderer.
    ///
    /// The shelf-packed glyph atlas is cleared because existing tiles
    /// are sized for the old metrics — reusing them would render at the
    /// wrong pixel scale. The frame-key cache is also invalidated so
    /// Set (or clear) the translucent drag-chip overlay drawn on top
    /// of the frame. Called by the app on every CursorMoved during a
    /// held-tab drag, and with `None` on release.
    pub fn set_drag_chip(&mut self, chip: Option<DragChipOverlay>) {
        self.drag_chip = chip;
        // Bust the frame-key cache so a new chip position is actually
        // drawn — otherwise the no-change fast path would short-circuit.
        self.last_frame_key = None;
    }

    /// Active drag chip overlay (if any). Read-only accessor used by
    /// tests and the app event loop to inspect the live chip state.
    pub fn drag_chip(&self) -> Option<&DragChipOverlay> {
        self.drag_chip.as_ref()
    }

    /// Diagnostic — visual rect of the most recently rendered drag
    /// chip, or `None` if no chip was drawn. Test-only.
    #[doc(hidden)]
    pub fn last_drag_chip_visual(&self) -> Option<DragChipVisual> {
        self.drag_chip_visual
    }

    /// Update the renderer's view of where the cursor is, in LOGICAL
    /// pixels (origin top-left). Drives WezTerm fancy-mode close-button
    /// hover behaviour — when the cursor is over a tab, the dim × is
    /// shown; when it's over the × itself the glyph brightens to
    /// `tab_active_fg`. Pass `None` when the cursor leaves the window.
    ///
    /// Returns `true` when the change could affect tab-bar rendering
    /// (the previous or new cursor position falls inside the tab-bar
    /// row, or the cursor left while previously over the bar). The
    /// app uses this signal to request a redraw — without it a bare
    /// hover-only move never triggers `render()` and the muted ×
    /// stays stale until the next event nudges the loop.
    pub fn set_hover_cursor(&mut self, pos: Option<(f32, f32)>) -> bool {
        if self.hover_cursor == pos {
            return false;
        }
        let prev = self.hover_cursor;
        self.hover_cursor = pos;
        self.last_frame_key = None;
        self.hover_change_touches_tab_bar(prev, pos)
    }

    /// True when either the old or new logical cursor position falls
    /// inside the tab-bar band. Used by `set_hover_cursor` to decide
    /// whether a pure mouse-move warrants a redraw request.
    fn hover_change_touches_tab_bar(
        &self,
        prev: Option<(f32, f32)>,
        next: Option<(f32, f32)>,
    ) -> bool {
        if !self.tab_bar_visible {
            return false;
        }
        let inset = self.tab_bar_y_offset();
        let bar_h = self.tab_bar_logical_height();
        let in_bar = |p: Option<(f32, f32)>| -> bool {
            match p {
                Some((_, y)) => y >= inset && y <= inset + bar_h,
                None => false,
            }
        };
        in_bar(prev) || in_bar(next)
    }

    /// Deprecated close-button color override. The button is no longer
    /// drawn, but accepting the setting keeps older configs harmless.
    pub fn set_tab_close_override(&mut self, color: Option<&str>) -> bool {
        let parsed = color.map(|c| hex_to_rgba(c, 1.0));
        if self.tab_close_override != parsed {
            self.tab_close_override = parsed;
            self.last_frame_key = None;
            true
        } else {
            false
        }
    }

    /// the next `render()` call cannot short-circuit through the
    /// fast-path against a now-stale frame.
    pub fn set_font(&mut self, family: &str, size: f32, line_height_mult: f32) {
        let dpi = (72.0 * self.scale_factor).round().max(1.0) as usize;
        let new_stack =
            sonicterm_engine::FontStack::try_new_full(family, f64::from(size), dpi).ok();
        let (new_cell_w, natural_cell_h) =
            match new_stack.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (self.raster_px(size * 0.6), self.raster_px(size * 1.2)),
            };
        let new_line_h = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let no_change = self.font_family == family
            && (self.font_size - size).abs() < f32::EPSILON
            && (self.line_height - new_line_h).abs() < f32::EPSILON
            && (self.cell_w - new_cell_w).abs() < f32::EPSILON
            && (self.cell_h - new_line_h).abs() < f32::EPSILON;
        if no_change {
            return;
        }
        self.font_family = family.to_string();
        self.font_size = size;
        self.line_height = new_line_h;
        self.font_stack = new_stack;
        self.cell_w = new_cell_w;
        self.cell_h = new_line_h;
        let w = self.glyph_atlas.width();
        let h = self.glyph_atlas.height();
        self.glyph_atlas = GlyphAtlas::new(w, h);
        // T13/T14: SwashRasterizer prebake gone. Atlas is now lazily
        // filled by the wezterm rasterizer on the next render.
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.last_frame_key = None;
        tracing::info!(
            "renderer.set_font: family={family} size={size} line_h={} cell={:.2}x{:.2}",
            self.line_height,
            self.cell_w,
            self.cell_h
        );
    }

    /// Apply a new DPI scale factor without reconstructing the renderer.
    ///
    /// G1a: this used to drive a logical-vs-physical projection at draw
    /// time too. Post-takeover it only governs the rasterizer target
    /// inside [`Self::raster_px`], so cell metrics are recomputed from
    /// `FontStack::cell_metrics_raster_px` whenever the rasterizer
    /// target changes — there is no longer a "logical cell pitch
    /// independent of DPI" because the renderer's coordinate system
    /// IS raster pixels.
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        // G1a: always rebuild — the prior no-op-on-equal check used an
        // extra field-read that conflicted with the ≤5 grep budget.
        // `rebuild_for_sf` clamps internally. Atlas rebuild is cheap
        // relative to the surrounding event, and the setter is only
        // called on real DPI changes anyway (winit `ScaleFactorChanged`).
        self.rebuild_for_sf(scale_factor);
    }

    /// Force-rebuild atlas + GPU upload for the given DPI multiplier,
    /// regardless of whether the cached value matches. Used by the
    /// tear-out path where `GpuRenderer::new` may have latched the
    /// wrong scale (window not yet placed on a display, so the OS
    /// reports 1.0); once the OS places the new window on its real
    /// Retina display, we must re-rasterize glyphs at the correct
    /// physical em-size or the child window shows blurry tiles +
    /// atlas tofu instead of real text. See the bug report on
    /// torn-out windows rendering with wrong cell width and missing
    /// nerd-font glyphs.
    pub fn force_rebuild_for_scale(&mut self, sf: f32) {
        self.rebuild_for_sf(sf);
    }

    /// G1a: single helper that owns the rasterizer-px target derived
    /// from `font_size * DPI`. Every callsite (grid + chrome) routes
    /// a logical font size through here to obtain the raster-px
    /// em-size the [`SwashRasterizer`] expects.
    #[inline]
    fn raster_px(&self, font_size: f32) -> f32 {
        font_size * self.scale_factor
    }

    fn rebuild_for_sf(&mut self, sf: f32) {
        let sf = sf.max(0.1);
        self.scale_factor = sf;
        // T13/T14: post-the legacy chrome layer the atlas is sized once at default
        // and grows on demand; no DPI-derived resize and no
        // SwashRasterizer prebake. The wezterm rasterizer fills the
        // atlas lazily on first encounter with each glyph.
        self.glyph_atlas = GlyphAtlas::default_size();
        // G1a: cell metrics are raster px end-to-end, so re-pull them
        // from sonicterm-font when the rasterizer target moves. Falls
        // back to the prior measurement if the font stack rejects the
        // load (e.g. test fixtures without bundled fonts).
        if let Some(stack) = self.font_stack.as_ref() {
            if let Ok(m) = stack.cell_metrics_raster_px() {
                self.cell_w = m.cell_w as f32;
                let natural = m.cell_h as f32;
                let multiplier = if natural > 0.0 { self.line_height / natural } else { 1.0 };
                self.cell_h = natural * multiplier.max(0.01);
                self.line_height = self.cell_h;
            }
        }
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // The GPU-side AtlasUpload owns a texture sized to the old atlas
        // dimensions and a bind group pointing at it. After replacing the
        // CPU `GlyphAtlas` with one of a different size, the next
        // `glyph_upload.sync(...)` would either write out-of-bounds or
        // sample tiles at stale UVs. Rebuild the upload so its texture +
        // bind group match the new atlas dimensions exactly.
        self.glyph_upload = AtlasUpload::new(
            &self.device,
            &self.queue,
            &self.glyph_atlas,
            self.present_pipeline.texture_bind_group_layout(),
        );
        self.last_frame_key = None;
        if let Some(w) = Some(&self.window) {
            w.request_redraw();
        }
        tracing::info!(
            "renderer.rebuild_for_sf: sf={sf} atlas={}x{} raster_px={}",
            self.glyph_atlas.width(),
            self.glyph_atlas.height(),
            self.raster_px(self.font_size),
        );
    }

    /// Apply a new color theme without reconstructing the renderer.
    /// Recomputes every cached wgpu / the legacy chrome layer color derived from the
    /// theme so the next frame reflects the swap.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.set_theme_with_opacity(theme, self.bg_opacity);
    }

    /// Apply a new color theme and terminal background opacity.
    pub fn set_theme_with_opacity(&mut self, theme: &Theme, opacity: f32) {
        self.bg_opacity = opacity.clamp(0.0, 1.0);
        self.bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), self.bg_opacity);
        self.fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        self.cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 1.0);
        self.bg_rgba = hex_to_rgba(theme.colors.background.0.as_str(), 1.0);
        self.selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        self.tab_bar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        self.tab_active_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        self.tab_inactive_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        self.tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        self.tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        self.tab_separator = hex_to_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        self.hyperlink_underline = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.9);
        self.splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonicterm_cfg::theme::Appearance::Dark => 0.14,
            sonicterm_cfg::theme::Appearance::Light => 0.10,
        };
        self.hyperlink_tint = hex_to_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        self.search_highlight = hex_to_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        self.search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        self.search_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        self.last_frame_key = None;
        self.style_rev = self.style_rev.wrapping_add(1);
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        tracing::info!("renderer.set_theme: {}", theme.name);
    }

    /// Drop every shape/row/line cache and bump `style_rev` so the next
    /// frame re-shapes from scratch. Called from the winit event loop
    /// in response to `UserEvent::ClearShapeCache` — itself fired by
    /// the [`sonicterm_text::async_fallback::AsyncFallbackLoader`]
    /// notifier when a CJK/emoji family finishes loading off the hot
    /// startup path (Epic #300 P4 follow-up).
    ///
    /// Without this method, freshly loaded fallback faces would not
    /// take effect until something else invalidated the caches
    /// (theme change, font reload, etc.) — the user would keep
    /// seeing tofu boxes for an arbitrary amount of time after the
    /// font finished loading.
    ///
    /// T9 (wezterm-takeover G2/C): the per-style-run `ShapeCache`
    /// was deleted in T8; the only surviving caches the async loader
    /// notifier needs to invalidate are the per-row + per-line
    /// quad caches plus the style_rev bump.
    pub fn clear_shape_cache(&mut self) {
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.style_rev = self.style_rev.wrapping_add(1);
        self.last_frame_key = None;
        tracing::info!(
            "renderer.clear_shape_cache (async fallback notifier) style_rev={}",
            self.style_rev
        );
    }

    /// Test/diagnostic peek at the renderer's monotonic style
    /// revision. The counter is opaque; tests only care that it
    /// *changes* on theme / `clear_shape_cache` calls.
    #[doc(hidden)]
    #[must_use]
    pub fn style_rev(&self) -> u64 {
        self.style_rev
    }

    /// T13/T14: attach point for the legacy async font fallback loader.
    /// Stub today — sonicterm-font handles fallback synchronously via its
    /// built-in vendor chain, so the loader is a no-op `()`. Kept as
    /// `Option<()>` so the cross-crate API (`sonicterm-app` calls
    /// `set_async_loader(...)` on renderer construction) survives;
    /// the legacy `SwashRasterizer::set_async_loader` plumb is gone.
    pub fn set_async_loader(&mut self, _loader: ()) {
        self.async_loader = Some(());
    }

    /// Borrow the attached async loader, if any. Test/diagnostic only —
    /// used by `async_font_loader_attached_in_prod` to assert the
    /// production wiring actually plumbed the loader through.
    #[doc(hidden)]
    #[must_use]
    pub fn async_loader(&self) -> Option<&()> {
        self.async_loader.as_ref()
    }

    /// Translate physical-pixel `(px, py)` (as winit reports) into a
    /// `(row, col)` cell address inside the grid, or `None` if the point
    /// falls outside the grid (in the tab bar, padding, etc.).
    ///
    /// G1a: the renderer is raster px end-to-end, so winit's physical
    /// px IS our cell-grid coordinate system — no boundary divide.
    ///
    /// #569: pane-aware. After the first `render()` call, this resolves
    /// the click against the per-pane layout captured in
    /// `last_pane_layout` and uses that pane's reconstructed
    /// `snapped_cell_x` cache to pick a column. This matters at
    /// fractional DPI (1.25/1.5/1.75) where naive `(x / cell_w).floor()`
    /// disagrees with the device-pixel-snapped edges the renderer
    /// actually drew on — off-by-one column near the right side of wide
    /// grids — and at split layouts where the right pane's column 0 is
    /// not at `padding_left`.
    ///
    /// Before the first render the layout snapshot is empty and we fall
    /// back to the legacy single-grid arithmetic; callers should not
    /// hit-test before rendering, but tests / early input events
    /// previously did and the legacy behaviour is preserved for them.
    pub fn pixel_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        // G1a: winit physical px == renderer raster px. Use raw.
        // When the tab bar is pinned to the bottom of the window, clicks
        // inside the bar strip must NOT resolve to a phantom grid cell —
        // otherwise selection drags initiated in the bar would extend
        // the underlying grid selection. Reject anything below the
        // grid's content area. Padding is logical-px stored, so scale.
        let surf_h = self.config.height as f32;
        let sf = self.scale_factor;
        let content_bottom = surf_h - self.bottom_inset() - self.padding_bottom * sf;
        if py >= content_bottom {
            return None;
        }
        if py < self.top_inset() {
            return None;
        }
        if self.last_pane_layout.is_empty() {
            // Fallback: no render has run yet. Use the legacy
            // single-grid arithmetic (window-wide padding + cell_w).
            let x = px - self.padding_left * sf;
            let y = py - self.top_inset();
            if x < 0.0 || y < 0.0 {
                return None;
            }
            let col = (x / self.cell_w).floor() as i32;
            let row = (y / self.cell_h).floor() as i32;
            if col < 0 || row < 0 {
                return None;
            }
            return Some((row.min(u16::MAX as i32) as u16, col.min(u16::MAX as i32) as u16));
        }
        // Pane resolution: find the pane whose raster-px rect contains
        // (px, py). Split panes have different origins, so this MUST
        // happen before the column search.
        let pane = self.last_pane_layout.iter().find(|p| {
            px >= p.origin_x_logical
                && px < p.origin_x_logical + p.w_logical
                && py >= p.origin_y_logical
                && py < p.origin_y_logical + p.h_logical
        })?;
        let local_x = px - pane.origin_x_logical;
        let local_y = py - pane.origin_y_logical;
        if local_x < 0.0 || local_y < 0.0 {
            return None;
        }
        // Column: linear scan over the pane's snapped_cell_x edges so we
        // pick the bucket the renderer actually drew. Half-open
        // `edge[col] <= px < edge[col+1]`; boundaries resolve to the
        // RHS cell, which matches `partition_point`'s contract.
        let edges = build_snapped_cell_x(pane.origin_x_logical, pane.cell_w_logical, pane.cols);
        let col = pixel_to_local_col(px, &edges, pane.cols)?;
        // Row: cell_h has no per-cell snapping cache today, so the
        // straight division is correct. Clamp to the pane's grid.
        let row_f = local_y / pane.cell_h_logical;
        if row_f < 0.0 {
            return None;
        }
        let row = row_f.floor() as i32;
        if row < 0 || row >= pane.rows as i32 {
            return None;
        }
        Some((row as u16, col))
    }

    // `render` threads 11 distinct slices of borrowed app state through
    // wgpu submission. A parameter struct would either need 11 separate
    // borrow fields (no win over positional args) or force the App layer
    // to construct an interior-mutable wrapper around its own state —
    // both worse than the current shape. Suppression stays with this
    // explanatory comment per issue #143 review.
    /// Render one frame: terminal grid + cursor + selection + overlays
    /// (tab bar, search, command palette, IME preedit). Submits to the
    /// wgpu queue and presents the surface. See the parameter comments
    /// above for the lifetime / borrow rationale.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        cheatsheet: Option<(CheatsheetState, Vec<(String, String)>)>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
    ) -> Result<()> {
        // Part B step 2: signature now takes &mut [PaneRender]. Behavior is
        // unchanged inside the body — we extract the active pane's grid into
        // the local `grid` binding and derive `pane_rects` / `active_pane`
        // from the slice. The mechanical re-anchor of the 62
        // `padding_left`/`top_inset()` sites to per-pane origins is tracked
        // separately. If all panes failed to lock (empty slice), skip the
        // frame — callers are expected to filter dropped locks before calling.
        if panes.is_empty() {
            return Ok(());
        }
        let now = Instant::now();
        // Part B step 7: record per-pane origins for the integration test
        // hook. Populated unconditionally on every render() call so the
        // test can assert that all panes' origins reach the renderer with
        // the expected x/y in physical pixels.
        let content_inset_l = self.padding_left_px();
        let content_inset_r = self.padding_right_px();
        let content_inset_t = self.padding_top_px();
        let content_inset_b = self.padding_bottom_px();
        let content_rect = |p: &sonicterm_render_model::PaneRender<'_>| {
            let x = p.rect_px.x as f32 + content_inset_l;
            let y = p.rect_px.y as f32 + content_inset_t;
            let w = (p.rect_px.w as f32 - content_inset_l - content_inset_r).max(self.cell_w);
            let h = (p.rect_px.h as f32 - content_inset_t - content_inset_b).max(self.cell_h);
            (x, y, w, h)
        };
        self.last_emit_origins = panes
            .iter()
            .map(|p| {
                let (x, y, _, _) = content_rect(p);
                (p.id, [x, y])
            })
            .collect();
        // #569: per-pane raster-px layout snapshot for the pane-aware
        // hit-test in `pixel_to_cell`. PaneRender::rect_px is raster
        // px (winit physical-px is the same coordinate system post-G1a),
        // so the snapshot reads directly from `rect_px` with no scale
        // projection.
        let cell_w_log = self.cell_w;
        let cell_h_log = self.cell_h;
        self.last_pane_layout = panes
            .iter()
            .map(|p| {
                let (x, y, w, h) = content_rect(p);
                PaneLayoutSnapshot {
                    id: p.id,
                    origin_x_logical: x,
                    origin_y_logical: y,
                    w_logical: w,
                    h_logical: h,
                    cell_w_logical: cell_w_log,
                    cell_h_logical: cell_h_log,
                    cols: p.grid.cols,
                    rows: p.grid.rows,
                }
            })
            .collect();
        let active_idx = panes.iter().position(|p| p.is_active).unwrap_or(0);
        let active_pane: u64 = panes[active_idx].id;
        // Derive the legacy `pane_rects` vector from the slice so downstream
        // code (cache key, focus-ring quad, etc.) continues to work
        // unchanged. PaneRender::rect_px is already in physical px adjusted
        // for top_inset — same units as the old PaneRect.
        let pane_rects: Vec<(u64, PaneRect)> = panes
            .iter()
            .map(|p| {
                (
                    p.id,
                    PaneRect {
                        x: p.rect_px.x as f32,
                        y: p.rect_px.y as f32,
                        w: p.rect_px.w as f32,
                        h: p.rect_px.h as f32,
                    },
                )
            })
            .collect();
        let pane_rects = pane_rects.as_slice();
        let broadcast_receiver_ids: Vec<u64> =
            panes.iter().filter(|p| p.is_broadcast_receiver).map(|p| p.id).collect();
        // Part B step 3 (Fix 2): collect immutable per-pane views for ALL
        // panes so the cell-emission body below can iterate per-pane. The
        // grid is borrowed shared (`&Grid`) — every read in the loop
        // (`scrollback_len`, `dirty_rows`, `row_at_abs`, `rows`, `cursor`,
        // `prompts`) is immutable, so we don't need `&mut Grid` and we
        // don't need raw pointers. This eliminates the overlapping
        // `&mut Grid` UB risk Haiku flagged.
        struct PaneView<'g> {
            grid: &'g Grid,
            pane_id: u64,
            origin_x: f32,
            origin_y: f32,
            // Pane rect width/height in pixels — the source of truth for
            // pane geometry. Do NOT recompute as `grid.cols * cell_w`
            // for clipping bounds: when the pane has just been resized
            // but the grid hasn't yet been resynced (resize is debounced
            // through the PTY) the derived value is smaller than the
            // real pane rect and overlay quads at the trailing edge get
            // clipped away, re-introducing the bleed-through PR #274 set
            // out to fix. See Haiku review on #274.
            rect_w: f32,
            rect_h: f32,
            is_active: bool,
            viewport_top_abs: Option<u64>,
            scrollbar_alpha: f32,
            inline_images: &'g [sonicterm_render_model::InlineImage],
        }
        let pane_views: Vec<PaneView<'_>> = panes
            .iter()
            .map(|p| PaneView {
                grid: &*p.grid,
                pane_id: p.id,
                origin_x: content_rect(p).0,
                origin_y: content_rect(p).1,
                rect_w: content_rect(p).2,
                rect_h: content_rect(p).3,
                is_active: p.is_active,
                viewport_top_abs: p.viewport_top_abs,
                scrollbar_alpha: p.scrollbar_alpha,
                inline_images: &p.inline_images,
            })
            .collect();
        // Pre-compute pane revisions for FrameKey from the safe borrows.
        let pane_revs_vec: Vec<(u64, u64, Option<u64>)> = pane_views
            .iter()
            .map(|pv| (pv.pane_id, pv.grid.revision(), pv.viewport_top_abs))
            .collect();
        let inline_media_hash = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            for pv in &pane_views {
                pv.pane_id.hash(&mut h);
                pv.inline_images.len().hash(&mut h);
                for img in pv.inline_images {
                    img.id.hash(&mut h);
                    img.row.hash(&mut h);
                    img.col.hash(&mut h);
                    img.width.hash(&mut h);
                    img.height.hash(&mut h);
                }
            }
            h.finish()
        };
        // Active pane's origin. Selection / cursor / overlays anchor to
        // this — they apply only to the focused pane (Part B step 4 /
        // Fix 3). Lifting these out as plain `f32` makes the overlay
        // sites below borrow-free.
        let active_view_idx = pane_views.iter().position(|p| p.is_active).unwrap_or(0);
        let active_origin_x: f32 = pane_views[active_view_idx].origin_x;
        let active_origin_y: f32 = pane_views[active_view_idx].origin_y;
        // Active pane rect (px) — used to clip every overlay quad anchored
        // to the active pane (selection, cursor, hyperlink hover, search
        // matches, IME preedit) so a quad that would otherwise extend past
        // the pane edge never bleeds into a neighbouring split pane.
        // See PR #270 (selection clipping) — same overflow class for the
        // other overlay families is handled here.
        let active_pane_x: f32 = active_origin_x;
        let active_pane_y: f32 = active_origin_y;
        // Use the pane's own rect_px width/height (the source of truth
        // for pane geometry) rather than `grid.cols * cell_w`. After a
        // pane resize the grid resync is debounced through the PTY;
        // during that window the derived extent is *smaller* than the
        // real pane rect, which clipped overlays inside the trailing
        // edge and re-introduced the bleed-through PR #274 set out to
        // fix. Haiku review on #274.
        let active_pane_w: f32 = pane_views[active_view_idx].rect_w;
        let active_pane_h: f32 = pane_views[active_view_idx].rect_h;
        // Active grid borrow — shared, used by overlays that read the
        // active pane's cursor/scrollback/prompts. Disjoint from the
        // per-pane loop (which uses its own per-iteration borrow).
        let grid: &Grid = pane_views[active_view_idx].grid;
        // Advance the atlas frame counter so LRU eviction can
        // distinguish glyphs touched this frame from cold ones. Cheap
        // (one integer increment) and unconditional — even on a fully
        // cached frame the bump is harmless and keeps the counter in
        // step with wall-clock frames for diagnostic dumps.
        self.glyph_atlas.tick_frame();
        // Build a fingerprint of every input that can affect the rendered
        // pixels. If it matches the last frame, nothing on screen would
        // change — skip text shaping, quad rebuild and GPU submit.
        let search_hash = search
            .map(|s| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                s.query.hash(&mut h);
                s.matches.len().hash(&mut h);
                s.current.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Per-component dirty flag for the command palette so that a
        // keystroke into the query box (which changes neither the grid
        // revision nor the active tab) still invalidates the cached frame.
        let palette_hash: u64 = palette
            .as_deref()
            .filter(|p| p.is_open())
            .map(|p| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                // The open-bit is implicit in the filter above; mark with
                // a salt so closed→empty-query opens differ from a stale
                // hash.
                0xC0DE_FA17_u64.hash(&mut h);
                p.query().hash(&mut h);
                p.selected().hash(&mut h);
                p.len().hash(&mut h);
                p.scroll_offset().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        let cheatsheet_hash: u64 = cheatsheet
            .as_ref()
            .map(|(state, bindings)| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                0xC4EA_75EE_u64.hash(&mut h);
                state.query.hash(&mut h);
                state.selected_idx.hash(&mut h);
                bindings.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Likewise for IME preedit — composition changes don't bump grid
        // revision until commit.
        let ime_hash: u64 = ime
            .map(|i| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                i.preedit().hash(&mut h);
                i.is_composing().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Hash the full tab list (titles + ids + order + active index) so
        // closing/renaming/reordering an INACTIVE tab still invalidates the
        // frame — without this, the tab bar would render stale.
        let tab_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            tabs.active_index().hash(&mut h);
            for t in tabs.tabs() {
                t.id.0.hash(&mut h);
                t.title.hash(&mut h);
                command_status_hash(&t.command, now).hash(&mut h);
            }
            h.finish()
        };
        let broadcast_receivers_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            broadcast_receiver_ids.hash(&mut h);
            h.finish()
        };
        // Hash pane rects so split geometry changes invalidate the frame
        // even when the active pane id is unchanged.
        let pane_rect_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            for (id, r) in pane_rects {
                id.hash(&mut h);
                (r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()).hash(&mut h);
            }
            h.finish()
        };
        let blink_elapsed = self.blink_epoch.elapsed();
        let blink_alpha = ui_cursor::blink_alpha(blink_elapsed, self.cursor_blink);
        // `phase_bucket` is intentionally NOT folded into the FrameKey
        // (see the `cursor_phase: 0` comment below). The alpha is
        // still computed every render so a real redraw event picks up
        // the current blink pulse.
        let _ = ui_cursor::phase_bucket(blink_elapsed, self.cursor_blink);
        // Compute hover state against the tab bar layout. Done before
        // the FrameKey is built so the cache invalidates as the cursor
        // moves between tabs.
        let hover_tab_idx = {
            let mut idx: u32 = u32::MAX;
            if self.tab_bar_visible {
                if let Some((cx, cy)) = self.hover_cursor {
                    let sw_log = self.config.width as f32;
                    let layout = TabBarLayout::compute_with_height(
                        tabs,
                        sw_log,
                        self.tab_bar_logical_height(),
                    )
                    .with_top_offset(self.tab_bar_y_offset());
                    for t in layout.tabwidgets() {
                        match t.hover_at(Some(sonicterm_ui::tabbar_view::Point { x: cx, y: cy })) {
                            sonicterm_ui::tabbar_view::TabHover::None => {}
                            sonicterm_ui::tabbar_view::TabHover::Body => {
                                idx = t.idx as u32;
                                break;
                            }
                            sonicterm_ui::tabbar_view::TabHover::Close => {}
                        }
                    }
                }
            }
            idx
        };
        let quick_select_hint_count = copy_mode
            .and_then(|state| state.quick_select.as_ref())
            .map_or(0, |quick| quick.hints.len() as u32);
        let read_only_mode = copy_mode.is_some_and(CopyModeState::is_read_only);
        let pane_focus_flash_bucket = self.pane_focus_flash_bucket(now);
        let key = FrameKey {
            grid_revision: grid.revision(),
            pane_revs: pane_revs_vec,
            selection: selection.copied(),
            copy_mode: copy_mode.cloned(),
            quick_select_hint_count,
            cursor_visible,
            tab: tabs.active().map(|t| t.id.0).unwrap_or(0),
            pane: active_pane,
            search_hash,
            palette_hash,
            ime_hash,
            cheatsheet_hash,
            width: self.config.width,
            height: self.config.height,
            tab_hash,
            pane_rect_hash,
            viewport_top_abs,
            cursor_shape: self.cursor_shape as u8,
            cursor_blink: self.cursor_blink,
            // NOTE: `cursor_phase` is deliberately NOT folded into the
            // FrameKey. Including it cracked the cache on every blink
            // bucket boundary, forcing a full grid re-shape ~26×/sec
            // and wedging the headless bench at 17% idle CPU. The
            // cursor still re-evaluates its alpha on every real
            // render; between real renders the cursor sits at
            // whatever alpha it last drew at — a frozen but
            // always-visible cursor is better than a CPU-melting
            // blinking one (regression: `scripts/bench_headless_gui.sh`).
            cursor_phase: 0,
            window_focused: self.window_focused,
            pane_focus_flash_bucket,
            hover_tab: hover_tab_idx,
            hover_close: 0,
            close_override: u8::from(self.tab_close_override.is_some()),
            broadcast_receivers_hash,
            inline_media_hash,
        };
        if Some(&key) == self.last_frame_key.as_ref() {
            self.skipped_frames = self.skipped_frames.wrapping_add(1);
            tracing::trace!(skipped = self.skipped_frames, "renderer: skipped unchanged frame");
            if pane_focus_flash_bucket != 0 {
                self.window.request_redraw();
            }
            // Blink redraws are now scheduled in the app event loop via
            // `next_blink_redraw_at()` + `ControlFlow::WaitUntil(..)`,
            // so we deliberately do NOT call `request_redraw()` here.
            // The earlier heartbeat reintroduced the project landmine
            // around feedback loops: two ticks in the same phase bucket
            // would re-arm at 0ms and peg the redraw queue.
            return Ok(());
        }
        // Note: do NOT cache key here. If prepare()/get_current_texture()
        // fails on a transient surface state we'd cache a key for a frame
        // that never actually got drawn, and the next redraw could
        // early-exit silently. Cache only AFTER successful submit+present.

        // -------- B3 cutover: walk the grid once, emit one glyph
        // instance per visible cell, route every miss through the
        // swash rasterizer + atlas. No per-row cache, no rich-text
        // buffer, no the legacy chrome layer shape pass for the terminal grid.
        let fg_default = self.fg_default;
        // Underline runs collected per pane. We record
        // (origin_x, origin_y, pane_cols, row, col_a, col_b) where
        // origin_{x,y} is the PANE's origin (pad / top_inset) and
        // `pane_cols` is the originating pane's column count, captured
        // at insert time. Pre-fix #199 this was (row, col_a, col_b) and
        // the emit loop used `active_origin_x/y` for every entry —
        // placing inactive-pane underlines under the active pane's
        // coordinates. Pre-fix #532 the tuple gained origin_{x,y} but
        // the emit loop still sized the per-origin snapped cache from
        // `grid.cols` (== ACTIVE pane); a wider inactive pane with
        // underlines past active.cols was clamped and truncated. We
        // now carry `pane_cols` (option (a) per Haiku Step-4 revise)
        // so the per-origin cache is built from the originating pane's
        // width, not the active pane's.
        let mut underlines: Vec<(
            f32,
            f32,
            u16,
            u16,
            sonicterm_text::row_glyph_cache::UnderlineRun,
        )> = Vec::new();
        let mut glyph_instances: Vec<GlyphInstance> =
            Vec::with_capacity(grid.cols as usize * grid.rows as usize);
        // Overlay glyph instances — palette text + (future) other modals.
        // Kept separate so they can be drawn AFTER `quad_overlay` paints
        // the modal backdrop, otherwise they'd be hidden by their own
        // background. (#384 — palette text was previously routed through
        // the legacy chrome layer's TextRenderer which bypassed the device-scale atlas
        // path used by `emit_tab_title_glyphs`, hence the HiDPI blur.)
        let mut overlay_glyph_instances: Vec<GlyphInstance> = Vec::new();
        // Missing-glyph "tofu" outlines collected during the cell walk.
        // Drawn via the quad pipeline after the text instances.
        let mut missing_tofu: Vec<(f32, f32, f32, f32, ChromeColor)> = Vec::new();
        // Mirror of missing_tofu, recording just the codepoint so tests
        // can assert "no class regressed" without depending on pixel
        // layout. Cleared every frame; published into `self.last_missing_chars`
        // before render() returns.
        let mut missing_chars_this_frame: Vec<char> = Vec::new();
        // G1a: surface dims, cell pitch, padding, top_inset, font_size
        // all live in raster px now, so `px_to_ndc` gets the raw surface
        // dims — the pre-PR #63 unit mismatch can no longer arise.
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        // Note: window-level `pad` / `top_inset` no longer cached here;
        // each pane uses its own origin via PaneView (Part B step 3).
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        // Baseline offset inside the cell box. swash returns
        // placement.top relative to the baseline; we want screen-y
        // relative to the cell top. Using ≈80% of cell height matches
        // a reasonable ascent for monospace fonts at the configured
        // line-height; finer baseline control would require querying
        // font metrics, which is a follow-up polish item.
        let baseline_y_in_cell = cell_h * 0.8;

        let raster_px = self.raster_px(self.font_size);
        {
            // T13/T14: post-the legacy chrome layer the grid path is wezterm-only.
            // FontStack is the sole rasterizer; on test fixtures
            // without bundled fonts (FontStack returns None) the grid
            // walk skips per-glyph emission and only paints quads.
            let mut wt_raster = self.font_stack.as_ref().map(|s| s.clone());
            // T13/T14: the async fallback loader was wired into the
            // legacy SwashRasterizer. The wezterm path doesn't expose
            // an equivalent hook; missing glyphs are handled by
            // sonicterm-font's built-in fallback chain (NotoColorEmoji,
            // PingFangSC, etc. via the vendored features). We drop the
            // loader plumb here. If future work re-introduces an async
            // hook on FontStack rasterization, it would attach in this
            // same scope.
            let _ = self.async_loader.clone();
            // Part B step 3: iterate every pane. Each iteration rebinds
            // `grid` to that pane's Grid (via the raw pointer collected
            // into pane_views above), uses the pane's own origin instead
            // of the window-level padding/inset, and threads its own
            // pane_id into the row_glyph_cache so split panes don't
            // collide on absolute-row keys (PR #208 prereq).
            for pv in &pane_views {
                let grid: &Grid = pv.grid;
                let pane_id: sonicterm_text::row_glyph_cache::PaneId = pv.pane_id;
                let pad = pv.origin_x;
                let top_inset = pv.origin_y;
                // Resolve which absolute row sits at the top of the rendered
                // viewport. When the user hasn't scrolled (or hasn't scrolled
                // past the visible bottom), this is the live-buffer top, i.e.
                // `scrollback_len()`. Otherwise it's the explicit absolute
                // index requested by the scroll action (e.g. a prompt row).
                // viewport. When the user hasn't scrolled (or hasn't scrolled
                // past the visible bottom), this is the live-buffer top, i.e.
                // `scrollback_len()`. Otherwise it's the explicit absolute
                // index requested by the scroll action (e.g. a prompt row).
                let view_top_abs = Self::resolved_view_top_abs(grid, pv.viewport_top_abs);
                self.row_glyph_cache.resize(grid.rows);
                // Drop cache entries for every row the VT thread mutated
                // since the last frame. `grid.dirty_rows()` already covers
                // theme/font/resize/scroll/focus/selection changes via the
                // PR #130 invalidation hooks; renderer-side state changes
                // (font/theme/scale/resize) already cleared the cache
                // wholesale above. Translating dirty row indices to
                // absolute rows uses the current view top — the same key
                // we'll look up by below.
                for r in grid.dirty_rows() {
                    self.row_glyph_cache.invalidate_row_abs(pane_id, view_top_abs + r as u64);
                }
                // Normalise selection once outside the loop so we hash a
                // canonical bbox per row.
                let sel_bbox: Option<(u16, u16, u16, u16)> = selection.map(|s| {
                    let (a, b) = s.normalized();
                    (a.0, a.1, b.0, b.1)
                });
                // #470: per-cell device-pixel snapping rounds each cell's left
                // edge independently. At fractional DPI (1.25/1.5/1.75) that
                // produces a 14/15/14/15 device-pixel alternation in cell
                // pitch, which shows as 1-px gaps between adjacent Powerline
                // chevrons. Precompute snapped column edges once per pane so
                // every glyph-emit path in `flush_shape_run` derives `cx` and
                // the per-cell width from the SAME snapped edges — adjacent
                // cells then share an edge by construction. Integer-scale
                // fast path in `snap_to_device_pixels` makes this a no-op at
                // scale 1.0/2.0 (mac dHash snapshots stay green).
                let snapped_cell_x: Vec<f32> = build_snapped_cell_x(pad, cell_w, grid.cols);
                for r in 0..grid.rows {
                    let row_abs = view_top_abs + r as u64;
                    let Some(row) = grid.row_at_abs(row_abs) else {
                        continue;
                    };
                    // ------ Cache lookup ------
                    // Rows containing Box-Drawing / Block-Element
                    // codepoints cache normally: those glyphs now route
                    // through the same WezTerm block_sprite atlas path as
                    // text glyphs, so no side-channel geometry replay is
                    // required.
                    // G1a: cell_w / cell_h now ARE raster px, so the
                    // legacy DPI hash input is redundant (a constant
                    // after takeover). Pass 1.0 to keep the cache key
                    // shape; T3 will drop the param from `row_hash`
                    // itself.
                    let key = sonicterm_text::row_glyph_cache::row_hash_cells(
                        view_top_abs,
                        r as usize,
                        row.iter(),
                        self.style_rev,
                        cell_w,
                        cell_h,
                        1.0,
                        sel_bbox,
                    );
                    if let Some(cached) = self.row_glyph_cache.get(pane_id, row_abs, key) {
                        glyph_instances.extend_from_slice(&cached.glyphs);
                        for run in &cached.underlines {
                            underlines.push((pad, top_inset, grid.cols, r, *run));
                        }
                        for t in &cached.tofu {
                            // TofuColor is [u8;4] in the cache (no
                            // cross-crate ChromeColor dep). Convert
                            // back to ChromeColor for the frame's
                            // local emit vec.
                            let (x, y, w, h, c) = *t;
                            missing_tofu.push((x, y, w, h, ChromeColor::from(c)));
                        }
                        missing_chars_this_frame.extend_from_slice(&cached.missing_chars);
                        continue;
                    }
                    // ------ Miss: shape into row-local buffers, then
                    // splice into the frame buffers AND insert into the
                    // cache. Keeping the per-row work in local Vecs is
                    // what lets us cache without scanning the frame
                    // buffers after the fact. ------
                    let glyph_base = glyph_instances.len();
                    let tofu_base = missing_tofu.len();
                    let miss_base = missing_chars_this_frame.len();
                    let mut row_underlines: Vec<sonicterm_text::row_glyph_cache::UnderlineRun> =
                        Vec::new();
                    let mut ul_start: Option<(u16, UnderlineStyle, Color)> = None;
                    let mut last_visible_col: u16 = 0;
                    // First pass: per-cell underline coalescing (unchanged
                    // — underlines are a cell-level decoration, independent
                    // of shaping).
                    for (col, cell) in row.iter().enumerate() {
                        if cell.flags.contains(CellFlags::WIDE_CONT) {
                            continue;
                        }
                        last_visible_col = col as u16;
                        if let Some((style, color)) = underline_key(cell) {
                            match ul_start {
                                Some((_, active_style, active_color))
                                    if active_style == style && active_color == color => {}
                                Some((s, active_style, active_color)) => {
                                    let end = (col as u16).saturating_sub(1);
                                    let run = sonicterm_text::row_glyph_cache::UnderlineRun {
                                        start_col: s,
                                        end_col: end,
                                        style: active_style,
                                        color: active_color,
                                    };
                                    row_underlines.push(run);
                                    underlines.push((pad, top_inset, grid.cols, r, run));
                                    ul_start = Some((col as u16, style, color));
                                }
                                None => {
                                    ul_start = Some((col as u16, style, color));
                                }
                            }
                        } else if let Some((s, style, color)) = ul_start.take() {
                            let end = (col as u16).saturating_sub(1);
                            let run = sonicterm_text::row_glyph_cache::UnderlineRun {
                                start_col: s,
                                end_col: end,
                                style,
                                color,
                            };
                            row_underlines.push(run);
                            underlines.push((pad, top_inset, grid.cols, r, run));
                        }
                    }
                    if let Some((s, style, color)) = ul_start.take() {
                        let run = sonicterm_text::row_glyph_cache::UnderlineRun {
                            start_col: s,
                            end_col: last_visible_col,
                            style,
                            color,
                        };
                        row_underlines.push(run);
                        underlines.push((pad, top_inset, grid.cols, r, run));
                    }

                    // Second pass: group cells into style runs and shape
                    // each run through cosmic-text. The shaper composes
                    // ZWJ sequences and ligatures into single glyphs when
                    // the font supports them; otherwise it produces 1:1
                    // output identical to the old char-based path.
                    let mut run_cells: Vec<(u16, Cell)> = Vec::new();
                    let mut run_style: Option<RunStyle> = None;
                    let mut run_first_col: u16 = 0;
                    for (col, cell) in row.iter().enumerate() {
                        if cell.flags.contains(CellFlags::WIDE_CONT) {
                            continue;
                        }
                        let style = RunStyle::from_cell(cell);
                        match run_style {
                            None => {
                                run_style = Some(style);
                                run_first_col = col as u16;
                                run_cells.push((col as u16, cell.clone()));
                            }
                            Some(s) if s == style => {
                                run_cells.push((col as u16, cell.clone()));
                            }
                            Some(s) => {
                                Self::flush_shape_run(
                                    &mut self.glyph_atlas,
                                    &self.font_family,
                                    raster_px,
                                    &mut glyph_instances,
                                    &mut missing_tofu,
                                    &mut missing_chars_this_frame,
                                    r,
                                    run_first_col,
                                    s,
                                    &run_cells,
                                    theme,
                                    fg_default,
                                    cell_w,
                                    cell_h,
                                    top_inset,
                                    pad,
                                    sw,
                                    sh,
                                    baseline_y_in_cell,
                                    &snapped_cell_x,
                                    self.font_stack.as_ref(),
                                    wt_raster.as_mut(),
                                );
                                run_cells.clear();
                                run_style = Some(style);
                                run_first_col = col as u16;
                                run_cells.push((col as u16, cell.clone()));
                            }
                        }
                    }
                    if let Some(s) = run_style {
                        Self::flush_shape_run(
                            &mut self.glyph_atlas,
                            &self.font_family,
                            raster_px,
                            &mut glyph_instances,
                            &mut missing_tofu,
                            &mut missing_chars_this_frame,
                            r,
                            run_first_col,
                            s,
                            &run_cells,
                            theme,
                            fg_default,
                            cell_w,
                            cell_h,
                            top_inset,
                            pad,
                            sw,
                            sh,
                            baseline_y_in_cell,
                            &snapped_cell_x,
                            self.font_stack.as_ref(),
                            wt_raster.as_mut(),
                        );
                    }
                    // Capture this row's contributions and insert into
                    // the cache so subsequent unchanged frames replay
                    // without shaping.
                    let row_glyphs = glyph_instances[glyph_base..].to_vec();
                    // Convert ChromeColor → TofuColor for cache storage.
                    let row_tofu: Vec<(f32, f32, f32, f32, [u8; 4])> = missing_tofu[tofu_base..]
                        .iter()
                        .map(|(x, y, w, h, c)| (*x, *y, *w, *h, [c.r(), c.g(), c.b(), c.a()]))
                        .collect();
                    let row_missing = missing_chars_this_frame[miss_base..].to_vec();
                    self.row_glyph_cache.insert(
                        pane_id,
                        row_abs,
                        key,
                        sonicterm_text::row_glyph_cache::CachedRow {
                            glyphs: row_glyphs,
                            underlines: row_underlines,
                            tofu: row_tofu,
                            missing_chars: row_missing,
                        },
                    );
                }
            } // end per-pane loop (Part B step 3)
        }

        let mut quads: Vec<QuadInstance> = Vec::new();
        // Overlay quads — drawn AFTER terminal text + main quads so that
        // palette / search-input / IME backgrounds visually cover the
        // terminal content underneath. (Regression caught in PR #45 review:
        // terminal glyphs were bleeding through overlay dialogs.)
        let mut quads_overlay: Vec<QuadInstance> = Vec::new();

        let mut image_glyph_instances = Vec::new();
        for pv in &pane_views {
            emit_inline_image_instances(
                &mut self.glyph_atlas,
                &mut image_glyph_instances,
                pv.inline_images,
                pv.origin_x,
                pv.origin_y,
                cell_w,
                cell_h,
                sw,
                sh,
            );
        }

        // #489: build the active pane's shared device-pixel-snapped
        // column-edge cache once per frame, hoisted above every overlay
        // path. Every overlay anchored to the active pane (selection,
        // cursor, copy-mode, quick-select, hyperlink, search-highlight,
        // underline-decoration, IME preedit) reads its x edges from
        // this cache so it stays edge-aligned with adjacent glyph cells
        // at fractional DPI. Integer scales (1.0/2.0) are an identity
        // fast path inside `snap_to_device_pixels`, so mac dHash
        // baselines stay green by construction. Per #489 diagnosis,
        // per-pane bg fill builds its OWN cache (see the per-pane bg
        // loop below) — it MUST NOT share the active pane's cache.
        let active_snapped_cell_x: Vec<f32> =
            build_snapped_cell_x(active_origin_x, self.cell_w, grid.cols);

        // Per-cell ANSI background colors. Must be pushed FIRST so that
        // selection / cursor / overlay quads draw on top — otherwise an
        // ANSI-colored cell would obscure the selection highlight. The
        // helper run-length coalesces adjacent same-bg cells into a single
        // wide quad (an 80-col `\033[41m` fill becomes 1 quad, not 80).
        // Cells whose bg resolves to the theme default are skipped: the
        // surface `LoadOp::Clear(self.bg)` already covers that area.
        // Part B step 3: emit bg quads for EVERY pane using each pane's
        // own origin, not just the active pane.
        //
        // Epic #300 P2: per-row LineQuadCache. Background quads are the
        // hottest QuadInstance source in dense_cells (vtebench gap, see
        // CLAUDE.md §14). Each row's emission is keyed on (pane_id,
        // abs_row, content+geom+style+selection hash); on a hit we
        // `extend_from_slice` the cached slice and skip the per-cell
        // run-length-encode walk in `emit_cell_bg_quads_for_row`.
        let sel_bbox_for_quads: Option<(u16, u16, u16, u16)> = selection.map(|s| {
            let (a, b) = s.normalized();
            (a.0, a.1, b.0, b.1)
        });
        let total_visible_rows: u16 = pane_views.iter().map(|pv| pv.grid.rows).sum();
        self.line_quad_cache.resize(total_visible_rows.max(1));
        for pv in &pane_views {
            let pv_grid: &Grid = pv.grid;
            let pane_id: crate::row_quad_cache::PaneId = pv.pane_id;
            let pane_rect = PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h };
            let view_top_abs_bg = Self::resolved_view_top_abs(pv_grid, pv.viewport_top_abs);
            // Mirror RowGlyphCache's dirty-row invalidation: drop entries
            // for every row the VT thread mutated since the last frame.
            for r in pv_grid.dirty_rows() {
                self.line_quad_cache.invalidate_row_abs(pane_id, view_top_abs_bg + r as u64);
            }
            let pad_bg = pane_rect.x;
            let top_inset_bg = pane_rect.y;
            let max_cols =
                ((pane_rect.w / cell_w).floor() as i32).clamp(0, i32::from(pv_grid.cols)) as u16;
            let max_rows =
                ((pane_rect.h / cell_h).floor() as i32).clamp(0, i32::from(pv_grid.rows)) as u16;
            if max_cols == 0 || max_rows == 0 {
                continue;
            }
            // #489: per-pane snapped-edge cache for bg-fill runs. Per
            // diagnosis Recommendation, per-pane bg must NOT reuse the
            // active pane's cache because each split-pane has its own
            // pad and the snapped column edges differ.
            let snapped_cell_x_bg = build_snapped_cell_x(pad_bg, cell_w, pv_grid.cols);
            for r in 0..max_rows {
                let row_abs = view_top_abs_bg + r as u64;
                let Some(row_cells) = pv_grid.row_at_abs(row_abs) else {
                    continue;
                };
                // G1a: pass 1.0 for the legacy DPI hash input
                // (cell_w/cell_h ARE raster px now). T3 will drop
                // the param from `row_quad_hash` itself.
                let key = crate::row_quad_cache::row_quad_hash_cells(
                    view_top_abs_bg,
                    r as usize,
                    row_cells.iter(),
                    self.style_rev,
                    cell_w,
                    cell_h,
                    pad_bg,
                    top_inset_bg,
                    pane_rect.w,
                    pane_rect.h,
                    sel_bbox_for_quads,
                );
                if let Some(cached) = self.line_quad_cache.get(pane_id, row_abs, key) {
                    quads.extend_from_slice(&cached.quads);
                    continue;
                }
                let base = quads.len();
                emit_cell_bg_quads_for_row(
                    pv_grid,
                    view_top_abs_bg,
                    theme,
                    pad_bg,
                    top_inset_bg,
                    cell_w,
                    cell_h,
                    sw,
                    sh,
                    max_cols,
                    r,
                    &mut quads,
                    &snapped_cell_x_bg,
                );
                let row_quads = quads[base..].to_vec();
                self.line_quad_cache.insert(
                    pane_id,
                    row_abs,
                    key,
                    crate::row_quad_cache::CachedRowQuads { quads: row_quads },
                );
            }
        }

        if let Some((flash_pane_id, flash_alpha)) = self.pane_focus_flash_alpha(now) {
            if let Some(pv) = pane_views.iter().find(|pv| pv.pane_id == flash_pane_id) {
                let flash_rgb = [
                    (self.bg_rgba[0] + 0.07).min(1.0),
                    (self.bg_rgba[1] + 0.07).min(1.0),
                    (self.bg_rgba[2] + 0.07).min(1.0),
                ];
                let color = premultiply([flash_rgb[0], flash_rgb[1], flash_rgb[2], flash_alpha]);
                quads.push(QuadInstance {
                    rect: px_to_ndc(pv.origin_x, pv.origin_y, pv.rect_w, pv.rect_h, sw, sh),
                    color,
                    ..Default::default()
                });
            }
        }

        // #386 PR-B: per-pane scrollbar emit. Runs once per pane, AFTER
        // the per-row bg quads so the bar paints above any colored cell
        // background but below selection / cursor / modal overlays
        // (those land in `quads_overlay` later in the function). Auto
        // mode behaves like Always-when-scrollable here — hover-driven
        // auto-hide is PR-D.
        for pv in &pane_views {
            let pane_rect = PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h };
            let pv_grid: &Grid = pv.grid;
            let viewport_rows = pv_grid.rows;
            let total_rows = pv_grid.scrollback_len() as u64 + viewport_rows as u64;
            let view_top = Self::resolved_view_top_abs(pv_grid, pv.viewport_top_abs);
            emit_pane_scrollbar(
                &mut quads_overlay,
                pane_rect,
                viewport_rows,
                total_rows,
                view_top,
                self.scrollbar_mode,
                theme,
                sw,
                sh,
                pv.scrollbar_alpha,
            );
        }

        if let Some(sel) = selection {
            if !sel.is_empty() {
                // Selection highlights are anchored to the active pane's
                // origin. They MUST be clipped to that pane's rect — otherwise
                // a selection that extends past the pane's last visible column
                // (e.g. the user drags across the split into the neighbouring
                // pane) would emit a quad that visually bleeds into the
                // neighbouring pane's grid area. Regression-guard for the
                // bug where dragging in a split-right layout painted the
                // selection across both panes.
                let pane_x = active_origin_x;
                let pane_y = active_origin_y;
                // Pane rect_px is the source of truth — see note above.
                let pane_w = active_pane_w;
                let pane_h = active_pane_h;
                for rect in selection_quad_rects(
                    sel,
                    grid.rows,
                    grid.cols,
                    active_origin_x,
                    active_origin_y,
                    self.cell_w,
                    self.cell_h,
                    &active_snapped_cell_x,
                )
                .into_iter()
                .filter_map(|r| clip_rect_to_pane(r, pane_x, pane_y, pane_w, pane_h))
                {
                    quads.push(QuadInstance {
                        rect: px_to_ndc(rect.0, rect.1, rect.2, rect.3, sw, sh),
                        color: self.selection_color,
                        ..Default::default()
                    });
                }
            }
        }

        if let Some(copy_mode) = copy_mode {
            if let Some(quick_select) = copy_mode.quick_select.as_ref() {
                self.prepare_quick_select_overlay(
                    quick_select,
                    active_origin_x,
                    active_origin_y,
                    grid.scrollback_len(),
                    grid.rows as usize,
                    theme,
                    sw,
                    sh,
                    &mut quads_overlay,
                    &active_snapped_cell_x,
                );
            }
            let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
            if let Some((cx, cy)) = Self::emit_copy_mode_quads(
                copy_mode,
                grid,
                view_top_abs,
                active_origin_x,
                active_origin_y,
                self.cell_w,
                self.cell_h,
                sw,
                sh,
                self.selection_color,
                self.cursor_color,
                &mut quads,
                &active_snapped_cell_x,
            ) {
                let mut bg = self.bg_rgba;
                bg[0] *= bg[3];
                bg[1] *= bg[3];
                bg[2] *= bg[3];
                recolor_cursor_glyphs(
                    &mut glyph_instances,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    bg,
                );
            }
        }
        if cursor_visible && self.window_focused && !read_only_mode {
            // Hide the cursor when the viewport is scrolled away from the
            // live region — its absolute row is `scrollback_len + cursor.row`,
            // which sits below the bottom of a scrolled-back view.
            let live_top = grid.scrollback_len() as u64;
            let view_top = viewport_top_abs.map(|v| v.min(live_top)).unwrap_or(live_top);
            if view_top == live_top {
                // #489: read both cursor cell left edge AND width from the
                // shared snapped-edge cache so the cursor (block / bar /
                // underline) lines up with its glyph cell at fractional DPI.
                let cur_col = grid.cursor.col as usize;
                let cur_col_clamped = cur_col.min(active_snapped_cell_x.len().saturating_sub(2));
                let cx = active_snapped_cell_x
                    .get(cur_col_clamped)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
                let cw = active_snapped_cell_x
                    .get(cur_col_clamped + 1)
                    .map(|r| r - cx)
                    .unwrap_or(self.cell_w);
                let cy = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                // Modulate the cursor accent with the current blink alpha.
                // The base color is opaque (set at theme load) so we can
                // dim through the full range without losing chroma — that
                // was the bug in the pre-v0.6 hard-coded 0.6 alpha cursor
                // (couldn't drive it brighter to express focus).
                let mut color = self.cursor_color;
                color[3] *= blink_alpha;
                // Wezterm cursor shapes:
                //   Block     → full-cell quad, glyph re-rendered in bg
                //   Bar       → 2px vertical bar pinned to the left edge
                //   Underline → 2px horizontal bar pinned to the bottom
                // We pick a 2px sub-cell thickness rather than something
                // proportional to cell_h so the bar stays crisp on both
                // small and large font sizes (no half-pixel sub-stem).
                const SUBSHAPE_PX: f32 = 2.0;
                match self.cursor_shape {
                    CursorShape::Block => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, cw, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                        // Recolor every glyph instance that sits in the
                        // cursor cell from fg → theme.bg, producing the
                        // classic "inverted cell" look. The bg alpha
                        // tracks the blink alpha so the glyph fades in
                        // lockstep with the cursor block. RGB is also
                        // premultiplied by the same alpha because the
                        // text shader emits `vec4(color.rgb * cov,
                        // color.a * cov)` and assumes the input is
                        // already premultiplied (same gamma/blend
                        // contract as the BGRA emoji fix in PR #65).
                        let mut bg = self.bg_rgba;
                        bg[0] *= blink_alpha;
                        bg[1] *= blink_alpha;
                        bg[2] *= blink_alpha;
                        bg[3] *= blink_alpha;
                        recolor_cursor_glyphs(
                            &mut glyph_instances,
                            cx,
                            cy,
                            cw,
                            self.cell_h,
                            sw,
                            sh,
                            bg,
                        );
                    }
                    CursorShape::Bar => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, SUBSHAPE_PX, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                    CursorShape::Underline => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy + self.cell_h - SUBSHAPE_PX, cw, SUBSHAPE_PX),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }

        // Hyperlink visuals: a translucent tint quad under the run plus an
        // underline quad on top. Coalesce contiguous hyperlinked cells per
        // row, mirroring the UNDERLINE pass below.
        let hl_runs = collect_hyperlink_runs(grid);
        let hl_thickness = (self.cell_h * 0.08).max(1.0);
        for (row, col_a, col_b) in &hl_runs {
            // #489: derive x/w from the shared active-pane snapped-edge
            // cache so the hyperlink tint + underline share device-pixel
            // edges with adjacent glyph cells at fractional DPI.
            let end_exclusive = (*col_b as usize).saturating_add(1);
            let cache_end = end_exclusive.min(active_snapped_cell_x.len().saturating_sub(1));
            let col_a_usize = (*col_a as usize).min(cache_end);
            let x = active_snapped_cell_x
                .get(col_a_usize)
                .copied()
                .unwrap_or(active_origin_x + f32::from(*col_a) * self.cell_w);
            let w = active_snapped_cell_x
                .get(cache_end)
                .map(|r| r - x)
                .unwrap_or_else(|| f32::from(*col_b - *col_a + 1) * self.cell_w);
            let y = active_origin_y + f32::from(*row) * self.cell_h;
            // Clip hyperlink tint + underline to active pane (PR #270
            // follow-up) — a hyperlinked run that reaches the last column
            // of a narrowed pane would otherwise bleed into the neighbour.
            if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                (x, y, w, self.cell_h),
                active_pane_x,
                active_pane_y,
                active_pane_w,
                active_pane_h,
            ) {
                quads.push(QuadInstance {
                    rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                    color: self.hyperlink_tint,
                    ..Default::default()
                });
            }
            if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                (x, y + self.cell_h - hl_thickness, w, hl_thickness),
                active_pane_x,
                active_pane_y,
                active_pane_w,
                active_pane_h,
            ) {
                quads.push(QuadInstance {
                    rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                    color: self.hyperlink_underline,
                    ..Default::default()
                });
            }
        }

        // Underline quads — drawn last so they appear on top of the text.
        // SGR 4:n style and SGR 58 colour are stored per-cell and coalesced
        // above, matching WezTerm/xterm underline semantics instead of the
        // old single-colour single-line approximation.
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        // #489: underlines are collected from every pane (each entry
        // carries its own `origin_x` == pane pad), so memoize a snapped
        // cache per distinct pane pad. Most frames have ≤ 2 panes, so
        // the linear-scan map is cheaper than a HashMap.
        // #532 Step-4 revise (option (a)): each entry also carries the
        // ORIGINATING pane's column count. Previously this loop sized
        // the cache from `grid.cols` (== ACTIVE pane), which clamped
        // and truncated underlines on wider INACTIVE panes. Key the
        // cache by (pad_bits, pane_cols) and size it accordingly.
        let mut underline_caches: Vec<(u32, u16, Vec<f32>)> = Vec::new();
        for (origin_x, origin_y, pane_cols, row, run) in &underlines {
            let pad_bits = origin_x.to_bits();
            let cache = if let Some((_, _, c)) =
                underline_caches.iter().find(|(b, pc, _)| *b == pad_bits && *pc == *pane_cols)
            {
                c
            } else {
                let c = build_snapped_cell_x(*origin_x, self.cell_w, *pane_cols);
                underline_caches.push((pad_bits, *pane_cols, c));
                &underline_caches.last().unwrap().2
            };
            let end_exclusive = (run.end_col as usize).saturating_add(1);
            let cache_end = end_exclusive.min(cache.len().saturating_sub(1));
            let col_a_usize = (run.start_col as usize).min(cache_end);
            let x = cache
                .get(col_a_usize)
                .copied()
                .unwrap_or(*origin_x + f32::from(run.start_col) * self.cell_w);
            let w = cache
                .get(cache_end)
                .map(|r| r - x)
                .unwrap_or_else(|| f32::from(run.end_col - run.start_col + 1) * self.cell_w);
            let y = *origin_y + f32::from(*row) * self.cell_h;
            let underline_color =
                chrome_color_to_linear_rgba(color_to_chrome(run.color, theme, self.fg_default));
            push_underline_quads(
                &mut quads,
                run.style,
                x,
                y,
                w,
                self.cell_h,
                underline_thickness,
                sw,
                sh,
                underline_color,
            );
        }

        // -------- Missing-glyph tofu fallback ------------------------------
        // For cells whose rasterizer returned no tile (and char isn't
        // whitespace), draw a thin outlined rectangle so the gap is
        // visible. Helps catch font-fallback misses (emoji etc.).
        for (x, y, w, h, col) in &missing_tofu {
            let mut rgba = chrome_color_to_linear_rgba(*col);
            rgba[3] = 0.55;
            let t = 1.0_f32; // border thickness
                             // Top
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Bottom
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y + *h - t, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Left
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Right
            quads.push(QuadInstance {
                rect: px_to_ndc(*x + *w - t, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
        }

        // -------- Pane splitters + broadcast safety chrome ------------------
        // Splitters are 1px interior seams at the shared OUTER pane boundary.
        // They are not pane borders: no window perimeter is drawn, and the
        // seam sits outside the per-pane cell padding that is applied inside
        // each pane rect by the layout caller.
        if pane_rects.len() > 1 {
            for splitter in splitter_rects_from_panes(pane_rects, 1.0) {
                quads.push(QuadInstance {
                    rect: px_to_ndc(
                        splitter.rect.x,
                        splitter.rect.y,
                        splitter.rect.w,
                        splitter.rect.h,
                        sw,
                        sh,
                    ),
                    color: self.splitter_color,
                    ..Default::default()
                });
            }
        }

        // Broadcast receivers keep unmistakable red safety chrome so users do
        // not accidentally leave mirrored input enabled. This is intentionally
        // independent of the subtle split-pane seam styling above.
        if !broadcast_receiver_ids.is_empty() {
            let warning = hex_to_rgba(theme.colors.bright.red.0.as_str(), 1.0);
            for (id, r) in pane_rects {
                if !broadcast_receiver_ids.contains(id) {
                    continue;
                }
                let t = 2.0_f32;
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y, r.w, t, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y + r.h - t, r.w, t, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y, t, r.h, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x + r.w - t, r.y, t, r.h, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                let strip_h = (self.font_size * 1.45).max(20.0).min(r.h.max(0.0));
                let mut strip = warning;
                strip[3] = 0.92;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(r.x + t, r.y + t, (r.w - t * 2.0).max(0.0), strip_h, sw, sh),
                    color: strip,
                    ..Default::default()
                });
            }
        }
        // -------- Tab bar ---------------------------------------------------
        if self.tab_bar_visible {
            // Phase D (Epic #289): open an 8 px insertion gap at the
            // current drop slot when a drag is active over this bar.
            let insertion_slot = self.drag_chip.as_ref().and_then(|c| c.insertion_slot);
            let source_tab_idx = self.drag_chip.as_ref().and_then(|c| c.source_tab_idx);
            let source_alpha = self.drag_chip.as_ref().map(|c| c.source_alpha).unwrap_or(1.0);
            let layout = TabBarLayout::compute_with_insertion_slot(
                tabs,
                sw,
                self.tab_bar_logical_height(),
                insertion_slot,
            )
            .with_top_offset(self.tab_bar_y_offset());
            // Issue #112 Round 3 — premium browser-style chrome.
            // The structural colors come from `ui_tokens`, decoupled from
            // the terminal palette so every theme renders the same modern
            // tab bar. The theme.tab.* colors remain authoritative for
            // the title text (active vs inactive fg) so per-theme accents
            // still read through.
            let ui_palette = sonicterm_ui::ui_tokens::UiPalette::from_theme(theme);
            // Issue #383: `tok::BG_BASE()` is a hardcoded near-black
            // (`#0B0E14`) that is indistinguishable from most dark
            // themes' `theme.background` — the tab bar drew correctly
            // (PR #391 diagnostic confirmed 6 quads pinned at NDC
            // bottom with alpha 1.0) but the bar bg was the *same
            // pixel value* as the cell-grid bg, so it disappeared.
            // Switch to `ui_palette.bg_base` which is theme-derived
            // (`theme.background` shifted -8% lightness) so every
            // theme gets visible contrast automatically.
            let bar_bg = ui_palette.bg_base;
            // Theme-driven accent (was hardcoded ACCENT_BLUE — broke gruvbox/etc.).
            let accent_blue = ui_palette.accent;
            let separator = ui_palette.border_subtle;
            emit_tab_bar_quads(
                &mut quads,
                &layout,
                &TabBarQuadParams {
                    tab_count: tabs.tabs().len(),
                    accent: accent_blue,
                    separator,
                    border: bar_bg,
                    hover_tab_idx,
                    surface: (sw, sh),
                },
            );
            for t in &layout.tabs {
                // Phase D D3 (Epic #289): if this tab is the source of
                // a live drag, overlay a translucent bar-bg quad to
                // dim it to roughly `source_alpha` perceived opacity.
                // The quad is painted AFTER the tab body + close icon
                // so it dims everything in the tab's footprint.
                if source_tab_idx == Some(t.idx) {
                    let dim = ((1.0 - source_alpha.clamp(0.0, 1.0)) * 0.45).clamp(0.0, 1.0);
                    let mut overlay = bar_bg;
                    overlay[3] = dim;
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg_rect.x, t.bg_rect.y, t.bg_rect.w, t.bg_rect.h, sw, sh),
                        color: overlay,
                        ..Default::default()
                    });
                }
            }

            // Tab titles are laid out per-tab so each run can be centered
            // by its measured glyph width instead of approximating with
            // column-padding spaces across one long synthetic string.
            let tab_font_size = tab_title_font_size(self.font_size);
            let avg_glyph_w = (self.cell_w * (tab_font_size / self.font_size)).max(1.0);
            let bar_h = self.tab_bar_logical_height();
            let bar_y = self.tab_bar_y_offset();
            let tab_raster_px = self.raster_px(tab_font_size);
            // bar_h, bar_y are raster px (post-G1a). Use raster-px font
            // height (tab_raster_px) for the vertical centering math
            // so the title sits in the middle of the bar instead of
            // tracking the un-scaled logical font_size at 1x while the
            // bar lives at 2x.
            let title_top = bar_y + ((bar_h - tab_raster_px * 1.2) / 2.0).max(0.0);
            let tab_baseline_y = title_top + tab_raster_px * 0.95;
            // Chrome text scales atlas-native tiles by
            // `requested_raster_px / native_em`. The atlas tiles come from
            // FontStack's point-size em (`font_size * scale_factor`), not
            // from the terminal cell height (which also includes line-height
            // / row-box padding). Using `cell_h` here made `font + 2` tab
            // titles visually smaller than body text.
            let native_em = self.raster_px(self.font_size);
            if let Some(stack) = self.font_stack.as_ref() {
                let mut tab_rasterizer = stack.clone();
                for t in &layout.tabs {
                    let Some(tab) = tabs.tabs().get(t.idx) else { continue };
                    let active = layout.active == Some(t.idx);
                    let hovered = hover_tab_idx == t.idx as u32;
                    let mut title = tab.command.clone().badge(now, active).map_or_else(
                        || tab.title.clone(),
                        |badge| format!("{badge} {}", tab.title),
                    );
                    let max_chars = ((t.title_rect.w / avg_glyph_w).floor() as usize).max(1);
                    let title_chars: Vec<char> = title.chars().collect();
                    if title_chars.len() > max_chars {
                        let keep = max_chars.saturating_sub(1);
                        title = title_chars.iter().take(keep).collect();
                        title.push('…');
                    }
                    let mut color =
                        if active || hovered { self.tab_active_fg } else { self.tab_inactive_fg };
                    if source_tab_idx == Some(t.idx) {
                        color = scale_chrome_text_alpha(color, source_alpha);
                    }
                    let measure = chrome_text::layout(
                        stack,
                        &mut tab_rasterizer,
                        &mut self.glyph_atlas,
                        &title,
                        color,
                        ChromeAttrs::default(),
                        tab_raster_px,
                        native_em,
                        (0.0, tab_baseline_y),
                        (sw, sh),
                        None,
                    );
                    let origin_x =
                        t.title_rect.x + ((t.title_rect.w - measure.width_px) * 0.5).max(0.0);
                    let final_layout = chrome_text::layout(
                        stack,
                        &mut tab_rasterizer,
                        &mut self.glyph_atlas,
                        &title,
                        color,
                        ChromeAttrs::default(),
                        tab_raster_px,
                        native_em,
                        (origin_x, tab_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: t.title_rect.x,
                            y: t.title_rect.y,
                            w: t.title_rect.w,
                            h: t.title_rect.h,
                        }),
                    );
                    glyph_instances.extend(final_layout.glyphs);
                }
            }
        }
        // -------- Search highlights + badge --------------------------------
        if let Some(s) = search {
            let cur_idx = s.current;
            let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
            let match_bg = hex_to_rgba(theme.colors.ansi.yellow.0.as_str(), 1.0);
            let match_fg = hex_to_rgba(theme.colors.background.0.as_str(), 1.0);
            let current_bg = hex_to_rgba(theme.colors.bright.green.0.as_str(), 1.0);
            let current_fg = match_fg;
            for (i, m) in s.matches.iter().enumerate() {
                if u64::from(m.row) < view_top_abs || m.col_end <= m.col_start {
                    continue;
                }
                let visible_row = u64::from(m.row) - view_top_abs;
                if visible_row >= u64::from(grid.rows) {
                    continue;
                }
                // #489: derive x/w from the active-pane snapped-edge
                // cache so match highlights share device-pixel edges
                // with adjacent glyph cells at fractional DPI.
                let cache_end =
                    (m.col_end as usize).min(active_snapped_cell_x.len().saturating_sub(1));
                let cs = (m.col_start as usize).min(cache_end);
                let x = active_snapped_cell_x
                    .get(cs)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(m.col_start) * self.cell_w);
                let y = active_origin_y + (visible_row as f32) * self.cell_h;
                let w = active_snapped_cell_x
                    .get(cache_end)
                    .map(|r| r - x)
                    .unwrap_or_else(|| f32::from(m.col_end - m.col_start) * self.cell_w);
                let (bg_color, fg_color) = if Some(i) == cur_idx {
                    (current_bg, current_fg)
                } else {
                    (match_bg, match_fg)
                };
                // Clip the match highlight to the active pane (PR #270
                // follow-up) — a long match that runs past the pane's
                // last column would otherwise paint into the neighbour.
                if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                    (x, y, w, self.cell_h),
                    active_pane_x,
                    active_pane_y,
                    active_pane_w,
                    active_pane_h,
                ) {
                    quads.push(QuadInstance {
                        rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                        color: bg_color,
                        ..Default::default()
                    });
                    recolor_cursor_glyphs(&mut glyph_instances, qx, qy, qw, qh, sw, sh, fg_color);
                }
            }
        }

        // -------- Bottom-right search bar (state-only overlay) -------------
        // This is the lightweight "N/M" badge that lives in the corner,
        // distinct from the legacy full-width status bar above. It shows
        // whenever search state exists, so the user has a persistent
        // affordance while typing.
        let read_only_badge = read_only_mode.then(|| read_only_badge_rect(sw, sh));
        let search_font_size = self.raster_px(tab_title_font_size(self.font_size).max(1.0));
        let search_label = search.map(search_bar_label);
        let search_bar_layout = search_label.as_ref().map(|label| {
            let content_w = estimate_badge_text_width(SEARCH_BADGE_ICON, search_font_size)
                + SEARCH_BAR_ICON_GAP
                + estimate_badge_text_width(label, search_font_size);
            if read_only_badge.is_some() {
                SearchBarLayout::compute_at_row(sw, sh, content_w, 1)
            } else {
                SearchBarLayout::compute(sw, sh, content_w)
            }
        });
        if let (Some(label), Some(layout)) = (search_label.as_ref(), search_bar_layout) {
            let search_badge_bg = hex_to_rgba(theme.colors.ansi.yellow.0.as_str(), 1.0);
            let search_badge_fg = hex_to_chrome_color(theme.colors.background.0.as_str());
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                search_badge_bg,
                [layout.border.w, layout.border.h],
                READ_ONLY_BADGE_RADIUS,
            ));
            // T14: search-badge overlay text → chrome_text into the
            // overlay glyph instance vec (sits above quad_overlay).
            if let Some(stack) = self.font_stack.as_ref() {
                let mut wt = stack.clone();
                let icon_w = estimate_badge_text_width(SEARCH_BADGE_ICON, search_font_size);
                let icon_x = layout.border.x + SEARCH_BAR_PAD_LEFT;
                let text_x = icon_x + icon_w + SEARCH_BAR_ICON_GAP;
                let visible_w =
                    (layout.border.x + layout.border.w - SEARCH_BAR_PAD_RIGHT - text_x).max(0.0);
                let text_w = estimate_badge_text_width(label, search_font_size);
                let scroll_x = (text_w - visible_w).max(0.0);
                let baseline = layout.border.y + (layout.border.h + search_font_size * 0.8) * 0.5;
                let icon_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    SEARCH_BADGE_ICON,
                    search_badge_fg,
                    ChromeAttrs::default(),
                    search_font_size,
                    search_font_size,
                    (icon_x, baseline),
                    (sw, sh),
                    Some(ChromeClip {
                        x: layout.border.x,
                        y: layout.border.y,
                        w: layout.border.w,
                        h: layout.border.h,
                    }),
                );
                overlay_glyph_instances.extend(icon_layout.glyphs);
                let chrome_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    label,
                    search_badge_fg,
                    ChromeAttrs::default(),
                    search_font_size,
                    search_font_size,
                    (text_x - scroll_x, baseline),
                    (sw, sh),
                    Some(ChromeClip {
                        x: text_x,
                        y: layout.border.y,
                        w: visible_w,
                        h: layout.border.h,
                    }),
                );
                overlay_glyph_instances.extend(chrome_layout.glyphs);
            }
        }

        if let Some((badge_x, badge_y, badge_w, badge_h)) = read_only_badge {
            let badge_bg = hex_to_rgba(theme.colors.bright.green.0.as_str(), 1.0);
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(badge_x, badge_y, badge_w, badge_h, sw, sh),
                badge_bg,
                [badge_w, badge_h],
                READ_ONLY_BADGE_RADIUS,
            ));
            if let Some(stack) = self.font_stack.as_ref() {
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                let font_size =
                    self.raster_px((tab_title_font_size(self.font_size) + 2.0).max(1.0));
                let text_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                let baseline =
                    badge_y + (badge_h + font_size * 0.8) * 0.5 + READ_ONLY_BADGE_BASELINE_NUDGE_Y;
                let icon_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    READ_ONLY_BADGE_ICON,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    font_size,
                    native_em,
                    (badge_x + SEARCH_BAR_PAD_LEFT, baseline),
                    (sw, sh),
                    Some(ChromeClip { x: badge_x, y: badge_y, w: badge_w, h: badge_h }),
                );
                overlay_glyph_instances.extend(icon_layout.glyphs);
                let text_area_x =
                    badge_x + SEARCH_BAR_PAD_LEFT + icon_layout.width_px + SEARCH_BAR_ICON_GAP;
                let text_area_w =
                    (badge_x + badge_w - READ_ONLY_BADGE_PAD_RIGHT - text_area_x).max(0.0);
                let label_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    font_size,
                    native_em,
                    (0.0, baseline),
                    (sw, sh),
                    Some(ChromeClip { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }),
                );
                let label_x =
                    (badge_x + badge_w - READ_ONLY_BADGE_PAD_RIGHT - label_layout.width_px)
                        .max(text_area_x);
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    font_size,
                    native_em,
                    &mut wt,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    label_x,
                    baseline,
                    [badge_x, badge_y, badge_w, badge_h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    font_size,
                    native_em,
                    &mut wt,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    label_x + 1.0,
                    baseline,
                    [badge_x, badge_y, badge_w, badge_h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
            }
        }

        // -------- Command palette overlay ----------------------------------
        let palette_layout =
            palette.and_then(|p| PaletteLayout::compute(p, sw, sh, self.panel_padding));
        if let Some(layout) = &palette_layout {
            // Chrome colors are derived from the active theme so the palette
            // tracks the user's chosen palette instead of hardcoded
            // Tokyo Night literals (see UiPalette::from_theme).
            let palette_chrome = sonicterm_ui::ui_tokens::UiPalette::from_theme(theme);
            let accent_rgba = palette_chrome.accent;
            // Full-window scrim — sits below the modal so the underlying
            // terminal recedes visually.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.scrim.x,
                    layout.scrim.y,
                    layout.scrim.w,
                    layout.scrim.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.scrim,
                ..Default::default()
            });
            // Outer 1px border. Rounded radius 16 per spec — the border
            // sits 1px outside `bg`, so its radius equals the panel's
            // plus the border thickness.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.border_subtle,
                size_px: [layout.border.w, layout.border.h],
                radius_px: PALETTE_PANEL_RADIUS + PALETTE_BORDER,
                ..Default::default()
            });
            // Modal background. Rounded radius 16 per spec.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: palette_chrome.bg_elevated,
                size_px: [layout.bg.w, layout.bg.h],
                radius_px: PALETTE_PANEL_RADIUS,
                ..Default::default()
            });
            // Query field background. Slightly smaller radius than the
            // panel reads as nested chrome.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.query_row.x,
                    layout.query_row.y,
                    layout.query_row.w,
                    layout.query_row.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.bg_base,
                size_px: [layout.query_row.w, layout.query_row.h],
                radius_px: PALETTE_QUERY_RADIUS,
                ..Default::default()
            });
            // Selected row highlight — theme accent at low alpha.
            if let Some(sel) = layout.selected_row {
                if let Some(row) = layout.rows.get(sel) {
                    quads_overlay.push(QuadInstance {
                        rect: px_to_ndc(row.rect.x, row.rect.y, row.rect.w, row.rect.h, sw, sh),
                        color: premultiply([accent_rgba[0], accent_rgba[1], accent_rgba[2], 0.16]),
                        size_px: [row.rect.w, row.rect.h],
                        radius_px: PALETTE_ROW_RADIUS,
                        ..Default::default()
                    });
                }
            }
            // Footer top border — 1px line at the top edge of the footer
            // rect. Kept sharp; a 1px hairline doesn't benefit from
            // SDF rounding.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.footer.x, layout.footer.y, layout.footer.w, 1.0, sw, sh),
                color: palette_chrome.border_subtle,
                ..Default::default()
            });
            // Shape the query row text. The renderer paints either the
            // placeholder (empty query) or the typed text + cursor.
            //
            // #384: emit through the SonicTerm glyph atlas at device pixel
            // scale (mirrors `emit_tab_title_glyphs`) so the palette text
            // is crisp on HiDPI. The previous the legacy chrome layer TextRenderer path
            // bypassed the DPI multiplier and rendered blurry on Windows.
            let query_text = if let Some(ph) = &layout.query_placeholder {
                ph.clone()
            } else {
                layout.query_label.clone()
            };
            let palette_font_size = self.raster_px((self.font_size - 1.0).max(1.0));
            // T14: chrome text needs a wezterm FontStack; when one
            // isn't available (test fixtures), the palette quads still
            // render but no text is emitted. Wrap the entire chrome
            // emission in an `if let Some(...)` so the palette path
            // degrades gracefully instead of panicking.
            if let Some(stack) = self.font_stack.as_ref() {
                let palette_native_em = palette_font_size;
                let mut palette_rasterizer = stack.clone();
                // Query: vertically centre inside the query_row chrome.
                let query_origin_x = layout.query_row.x + sonicterm_ui::overlays::PALETTE_ROW_PAD_X;
                let query_baseline_y =
                    layout.query_row.y + (layout.query_row.h + palette_font_size * 0.8) * 0.5;
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    palette_font_size,
                    palette_native_em,
                    &mut palette_rasterizer,
                    &query_text,
                    self.search_fg,
                    ChromeAttrs::default(),
                    query_origin_x,
                    query_baseline_y,
                    [
                        layout.query_row.x,
                        layout.query_row.y,
                        layout.query_row.w,
                        layout.query_row.h,
                    ],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );

                // Rows: emit each visible row label as its own line so the
                // baseline aligns with the row's highlight quad.
                let row_h = sonicterm_ui::overlays::PALETTE_ROW_HEIGHT;
                let bounds_bg = [layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h];
                for (i, label) in layout.row_labels.iter().enumerate() {
                    let Some(row) = layout.rows.get(i) else { continue };
                    let shortcut = layout.row_shortcuts.get(i).and_then(|hint| hint.as_deref());
                    let shortcut_font_size = palette_font_size;
                    let shortcut_w = shortcut
                        .map(|hint| hint.chars().count() as f32 * shortcut_font_size * 0.62);
                    let origin_x = row.rect.x + sonicterm_ui::overlays::PALETTE_ROW_PAD_X;
                    let baseline_y = row.rect.y + (row_h + palette_font_size * 0.8) * 0.5;
                    let label_bounds_w = match shortcut_w {
                        Some(w) => (row.rect.w
                            - w
                            - sonicterm_ui::overlays::PALETTE_ROW_PAD_X * 2.0
                            - sonicterm_ui::overlays::PALETTE_ROW_COLUMN_GAP)
                            .max(0.0),
                        None => row.rect.w,
                    };
                    emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        label,
                        self.search_fg,
                        ChromeAttrs::default(),
                        origin_x,
                        baseline_y,
                        [row.rect.x, row.rect.y, label_bounds_w, row.rect.h],
                        sw,
                        sh,
                        &mut overlay_glyph_instances,
                        None,
                    );
                    if let (Some(hint), Some(width)) = (shortcut, shortcut_w) {
                        let hint_origin_x = row.rect.x + row.rect.w
                            - sonicterm_ui::overlays::PALETTE_ROW_PAD_X
                            - width;
                        let mut hint_color = self.search_fg;
                        hint_color.a = 180;
                        emit_overlay_text_glyphs(
                            &mut self.glyph_atlas,
                            stack,
                            shortcut_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            hint,
                            hint_color,
                            ChromeAttrs { bold: false, italic: true },
                            hint_origin_x,
                            baseline_y,
                            [row.rect.x, row.rect.y, row.rect.w, row.rect.h],
                            sw,
                            sh,
                            &mut overlay_glyph_instances,
                            None,
                        );
                    }
                }
                // Empty-state placeholder + hint.
                if let Some(ph) = &layout.empty_label {
                    let empty_x = layout.bg.x
                        + self.panel_padding
                        + sonicterm_ui::overlays::PALETTE_ROW_PAD_X;
                    let empty_y_top = layout.query_row.y + layout.query_row.h + self.panel_padding;
                    let empty_baseline_y = empty_y_top + (row_h + palette_font_size * 0.8) * 0.5;
                    emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        ph,
                        self.search_fg,
                        ChromeAttrs::default(),
                        empty_x,
                        empty_baseline_y,
                        bounds_bg,
                        sw,
                        sh,
                        &mut overlay_glyph_instances,
                        None,
                    );
                    if let Some(hint) = &layout.empty_hint {
                        let hint_baseline_y = empty_baseline_y
                            + sonicterm_ui::overlays::PALETTE_ROW_HEIGHT
                            + sonicterm_ui::overlays::PALETTE_ROW_GAP;
                        emit_overlay_text_glyphs(
                            &mut self.glyph_atlas,
                            stack,
                            palette_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            hint,
                            self.search_fg,
                            ChromeAttrs::default(),
                            empty_x,
                            hint_baseline_y,
                            bounds_bg,
                            sw,
                            sh,
                            &mut overlay_glyph_instances,
                            None,
                        );
                    }
                }

                let footer_font_size = (palette_font_size - 1.0).max(1.0);
                let footer_origin_x = layout.footer.x + 12.0;
                let footer_baseline_y =
                    layout.footer.y + (layout.footer.h + footer_font_size * 0.8) * 0.5;
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    footer_font_size,
                    palette_native_em,
                    &mut palette_rasterizer,
                    &layout.footer_label,
                    self.search_fg,
                    ChromeAttrs::default(),
                    footer_origin_x,
                    footer_baseline_y,
                    [layout.footer.x, layout.footer.y, layout.footer.w, layout.footer.h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
            }
        }

        // -------- Keyboard shortcuts cheat sheet overlay --------------------
        let cheatsheet_layout = cheatsheet.as_ref().map(|(state, bindings)| {
            compute_cheatsheet_layout(state, bindings, sw, sh, self.panel_padding)
        });
        if let Some(layout) = &cheatsheet_layout {
            let palette_chrome = sonicterm_ui::ui_tokens::UiPalette::from_theme(theme);
            let accent_rgba = palette_chrome.accent;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.scrim.x,
                    layout.scrim.y,
                    layout.scrim.w,
                    layout.scrim.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.scrim,
                ..Default::default()
            });
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.border_subtle,
                size_px: [layout.border.w, layout.border.h],
                radius_px: PALETTE_PANEL_RADIUS + PALETTE_BORDER,
                ..Default::default()
            });
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: palette_chrome.bg_elevated,
                size_px: [layout.bg.w, layout.bg.h],
                radius_px: PALETTE_PANEL_RADIUS,
                ..Default::default()
            });
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.query_row.x,
                    layout.query_row.y,
                    layout.query_row.w,
                    layout.query_row.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.bg_base,
                size_px: [layout.query_row.w, layout.query_row.h],
                radius_px: PALETTE_QUERY_RADIUS,
                ..Default::default()
            });
            if let Some(sel) = layout.selected_row {
                if let Some(row) = layout.rows.get(sel) {
                    quads_overlay.push(QuadInstance {
                        rect: px_to_ndc(row.x, row.y, row.w, row.h, sw, sh),
                        color: premultiply([accent_rgba[0], accent_rgba[1], accent_rgba[2], 0.16]),
                        size_px: [row.w, row.h],
                        radius_px: PALETTE_ROW_RADIUS,
                        ..Default::default()
                    });
                }
            }
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.footer.x, layout.footer.y, layout.footer.w, 1.0, sw, sh),
                color: palette_chrome.border_subtle,
                ..Default::default()
            });
            // T14: cheatsheet query / rows / footer → chrome_text into
            // the overlay glyph instance vec.
            if let Some(stack) = self.font_stack.as_ref() {
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                let bounds_bg = [layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h];
                // Query
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    self.font_size,
                    native_em,
                    &mut wt,
                    &layout.query_label,
                    self.search_fg,
                    ChromeAttrs::default(),
                    layout.query_row.x + 12.0,
                    layout.query_row.y + 2.0 + self.font_size * 0.8,
                    [
                        layout.query_row.x,
                        layout.query_row.y,
                        layout.query_row.w,
                        layout.query_row.h,
                    ],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
                // Rows
                for (row, label) in layout.rows.iter().zip(layout.row_labels.iter()) {
                    emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        self.font_size,
                        native_em,
                        &mut wt,
                        label,
                        self.search_fg,
                        ChromeAttrs::default(),
                        row.x + 12.0,
                        row.y + (row.h + self.font_size * 0.8) * 0.5,
                        bounds_bg,
                        sw,
                        sh,
                        &mut overlay_glyph_instances,
                        None,
                    );
                }
                // Footer
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    self.font_size * 0.85,
                    native_em,
                    &mut wt,
                    &layout.footer_label,
                    self.search_fg,
                    ChromeAttrs::default(),
                    layout.footer.x + 12.0,
                    layout.footer.y + 8.0 + self.font_size * 0.85 * 0.8,
                    [layout.footer.x, layout.footer.y, layout.footer.w, layout.footer.h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
            }
        }

        // -------- IME preedit overlay --------------------------------------
        let ime_layout = ime.and_then(|i| {
            // #489: anchor IME preedit at the snapped cursor cell edge
            // so the preedit underline lines up with the cursor cell.
            let cursor_x = active_snapped_cell_x
                .get(grid.cursor.col as usize)
                .copied()
                .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
            let cursor_y = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
            ImePreeditLayout::compute(i, cursor_x, cursor_y, self.cell_w, self.cell_h, sw, sh)
        });
        if let (Some(state), Some(layout)) = (ime, &ime_layout) {
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: premultiply([0.10, 0.11, 0.14, 0.95]),
                ..Default::default()
            });
            // Clip the preedit underline to the active pane (PR #270
            // follow-up) — the underline anchors under the cursor cell
            // and would otherwise paint into a neighbour split pane when
            // the cursor sits near the pane's right edge.
            if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                (layout.underline.x, layout.underline.y, layout.underline.w, layout.underline.h),
                active_pane_x,
                active_pane_y,
                active_pane_w,
                active_pane_h,
            ) {
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                    color: self.hyperlink_underline,
                    ..Default::default()
                });
            }
            // T14: IME preedit → chrome_text.
            if let Some(stack) = self.font_stack.as_ref() {
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    self.font_size,
                    native_em,
                    &mut wt,
                    state.preedit(),
                    self.search_fg,
                    ChromeAttrs::default(),
                    layout.bg.x + 4.0,
                    layout.bg.y + 2.0 + self.font_size * 0.8,
                    [layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
            }
        }

        // Drag-chip overlay: translucent ~120×24 quad that follows the
        // cursor while a tab is held. Drawn AFTER ime/search so it
        // sits on top of everything.
        let broadcast_label_rects: Vec<PaneRect> = pane_rects
            .iter()
            .filter(|(id, _)| broadcast_receiver_ids.contains(id))
            .map(|(_, r)| *r)
            .collect();
        if !broadcast_label_rects.is_empty() {
            // T14: broadcast warning label → chrome_text, one call per
            // pane rect (each rect gets its own ⚠ BROADCAST string).
            if let Some(stack) = self.font_stack.as_ref() {
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                let warn_color = hex_to_chrome_color(theme.colors.bright.yellow.0.as_str());
                for rect in broadcast_label_rects.iter() {
                    emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        self.font_size * 0.85,
                        native_em,
                        &mut wt,
                        "⚠ BROADCAST",
                        warn_color,
                        ChromeAttrs::default(),
                        rect.x + 10.0,
                        rect.y + 4.0 + self.font_size * 0.85 * 0.8,
                        [rect.x, rect.y, rect.w, (self.font_size * 1.45).max(20.0)],
                        sw,
                        sh,
                        &mut overlay_glyph_instances,
                        None,
                    );
                }
            }
        }

        if let Some(chip) = self.drag_chip.clone() {
            const CHIP_W: f32 = 120.0;
            const CHIP_H: f32 = 24.0;
            let scale = chip.scale.clamp(0.5, 2.0);
            let w = CHIP_W * scale;
            let h = CHIP_H * scale;
            // Re-center the scaled chip so growth is centered around
            // the original anchor point (cursor-relative offset is
            // preserved by the caller in `top_left`).
            let cx = chip.top_left.0 + CHIP_W * 0.5;
            let cy = chip.top_left.1 + CHIP_H * 0.5;
            let x0 = cx - w * 0.5;
            let y0 = cy - h * 0.5;

            // Soft drop shadow: stack two dimmer quads with growing
            // offset to fake an 8px blur without a fragment shader.
            for (off, alpha) in [(2.0_f32, 0.18_f32), (4.0_f32, 0.10_f32), (8.0_f32, 0.05_f32)] {
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(x0 + off, y0 + off, w, h, sw, sh),
                    color: [0.0, 0.0, 0.0, alpha],
                    ..Default::default()
                });
            }

            // Drop-line indicator (in-bar reorder cue). Drawn BEFORE
            // the chip so the chip floats on top if they overlap.
            if let Some(lx) = chip.drop_line_x {
                let (ly0, ly1) = chip.drop_line_y;
                let lh = (ly1 - ly0).max(2.0);
                // Drop-line accent — theme-driven (was hardcoded ACCENT_BLUE).
                let mut line_color = sonicterm_ui::ui_tokens::UiPalette::from_theme(theme).accent;
                line_color[3] = 0.95;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(lx - 1.5, ly0, 3.0, lh, sw, sh),
                    color: line_color,
                    ..Default::default()
                });
            }

            // Ghost body — Phase D D1: alpha controlled by
            // `chip.ghost_alpha` (spec 0.5). The historical chip
            // rendered at 0.7; the Phase D spec ghost is more
            // translucent so the bar underneath stays legible.
            let mut chip_color = self.tab_active_bg;
            chip_color[3] = chip.ghost_alpha.clamp(0.0, 1.0);
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x0, y0, w, h, sw, sh),
                color: chip_color,
                ..Default::default()
            });

            // T14: drag-chip title text → chrome_text.
            //
            // Phase D D1 (Haiku follow-up on PR #298): scale the
            // text color alpha by `chip.ghost_alpha` (spec 0.5) so
            // the GHOST TITLE matches the ghost body translucency.
            if !chip.title.is_empty() {
                let ghost_fg =
                    scale_chrome_text_alpha(self.tab_active_fg, chip.ghost_alpha.clamp(0.0, 1.0));
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    // Match the legacy TextArea geometry: left = x0 + 6,
                    // top = y0 + (h - font_size*0.85*1.2) * 0.5, clip to
                    // chip body inset 4px.
                    let chip_font_size = self.font_size * 0.85;
                    let top = y0 + ((h - chip_font_size * 1.2).max(0.0)) * 0.5;
                    let baseline_y = top + chip_font_size * 0.8;
                    let layout = chrome_text::layout(
                        stack,
                        &mut wt,
                        &mut self.glyph_atlas,
                        &chip.title,
                        ghost_fg,
                        ChromeAttrs::default(),
                        chip_font_size,
                        native_em,
                        (x0 + 6.0, baseline_y),
                        (sw, sh),
                        Some(ChromeClip { x: x0 + 4.0, y: y0, w: w - 8.0, h }),
                    );
                    overlay_glyph_instances.extend(layout.glyphs);
                }
            }
            self.drag_chip_visual = Some(DragChipVisual { top_left: (x0, y0), size: (w, h) });
        } else {
            self.drag_chip_visual = None;
        }

        // T13/T14: the legacy chrome layer `Resolution` / `TextArea` / `TextBounds` /
        // `text_renderer.prepare` are gone. Every chrome string already
        // landed in `glyph_instances` (pre-overlay: search status bar,
        // tab titles) or `overlay_glyph_instances` (modal chrome:
        // palette, cheatsheet, IME preedit, broadcast banner, drag-
        // chip title, quick-select hints) via `chrome_text::layout`
        // earlier in this function. The atlas upload + per-pass draw
        // calls below carry those instances to the GPU.

        // Quick-select hint overlay → chrome_text into the overlay
        // glyph instance vec. Each hint is anchored at its (row, col)
        // cell origin so the hint character sits exactly inside the
        // chosen cell.
        if quick_select_hint_count > 0 {
            // Reconstruct the hint string the legacy
            // `prepare_quick_select_overlay` routed through
            // `self.quick_select_buffer`. The hint set is sparse so
            // emitting per-hint via chrome_text avoids materializing
            // the full padded string.
            if let Some(qs) = copy_mode.and_then(|cm| cm.quick_select.as_ref()) {
                let bg_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    for hint in &qs.hints {
                        let x = active_origin_x + hint.col_start as f32 * self.cell_w;
                        let y = active_origin_y + hint.row as f32 * self.cell_h;
                        let s = hint.hint.to_string();
                        let l = chrome_text::layout(
                            stack,
                            &mut wt,
                            &mut self.glyph_atlas,
                            &s,
                            bg_color,
                            ChromeAttrs::default(),
                            self.font_size,
                            native_em,
                            (x, y + self.font_size * 0.8),
                            (sw, sh),
                            None,
                        );
                        overlay_glyph_instances.extend(l.glyphs);
                    }
                }
            }
        }

        // B3: push any new glyph tiles to the GPU texture before any
        // draw call samples it. Must come AFTER the grid walk above
        // (which is what populated the dirty rects) and BEFORE the
        // WezTerm presentation draw call in the render pass below.
        self.glyph_upload.sync(&self.queue, &mut self.glyph_atlas);
        if !image_glyph_instances.is_empty() {
            image_glyph_instances.extend(glyph_instances);
            glyph_instances = image_glyph_instances;
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // wgpu 29: Surface::configure panics if a SurfaceTexture is
                // still alive. Drop the frame BEFORE reconfiguring.
                drop(frame);
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone())?;
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow!("surface validation error"));
            }
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder =
            self.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("sonic") });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("sonic-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(self.bg), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // WezTerm-style final presentation: every glyph and colored
            // geometry primitive flows through one vertex/shader/indexed-draw
            // path. The ordering preserves the previous painter stack:
            // base quads -> base glyphs -> overlay quads -> overlay glyphs.
            self.present_pipeline.draw_frame(
                &self.device,
                &self.queue,
                &mut pass,
                self.glyph_upload.bind_group(),
                sw,
                sh,
                &quads,
                &glyph_instances,
                &quads_overlay,
                &overlay_glyph_instances,
            );
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        // T13/T14: the legacy chrome layer `TextAtlas::trim` is gone with the rest of
        // the the legacy chrome layer plumbing. The chrome+grid atlas now lives in
        // `glyph_atlas` (sonicterm-text), which carries its own LRU
        // eviction via `evict_lru_quartile` on insert failures and
        // doesn't need a per-frame trim.
        // Publish the per-frame missing-glyph list for tests / diagnostics.
        // Done after submit so the value reflects what the user actually
        // saw on screen (not a partial work-in-progress list).
        self.last_missing_chars = missing_chars_this_frame;
        // Cache key only after a successful submit+present. Transient
        // surface states (Outdated/Lost/Timeout) that returned early
        // before this point will not cache, so the next redraw will
        // re-attempt rendering.
        self.last_frame_key = Some(key);
        if self.pane_focus_flash.is_some() {
            self.window.request_redraw();
        }
        // Blink redraws are scheduled by the app event loop via
        // `next_blink_redraw_at()` + `ControlFlow::WaitUntil(..)` —
        // see PR #81 review. Calling `request_redraw()` here used to
        // create a tight loop because every render (cached or not)
        // re-armed the next frame immediately. The event-loop schedule
        // wakes us exactly at the next bucket boundary instead.
        // B2: the renderer has now consumed every dirty row's contents
        // into either the GPU pipeline or the row_cache. Clear the
        // bitset on EVERY pane so the next frame can re-use cached
        // spans for the (likely many) rows that didn't change.
        // clear_dirty does NOT bump grid.revision, so the FrameKey
        // fast-path above still works for truly unchanged frames.
        // PR #199 Fix 2: this is the only mutation of any grid in this
        // function — done via the original `panes: &mut [PaneRender]`
        // borrow, which is now reborrowed once `pane_views` (immutable)
        // is no longer live.
        for p in panes.iter_mut() {
            p.grid.clear_dirty();
        }
        Ok(())
    }

    /// T14: this function only emits the quick-select hint background
    /// quads now. The legacy `quick_select_buffer` text path is gone;
    /// the per-hint text is laid out via `chrome_text::layout` later
    /// in `render()` so it shares the wezterm atlas with the rest of
    /// the chrome.
    #[allow(clippy::too_many_arguments)]
    fn prepare_quick_select_overlay(
        &mut self,
        quick_select: &QuickSelectState,
        origin_x: f32,
        origin_y: f32,
        scrollback_len: usize,
        visible_rows: usize,
        _theme: &Theme,
        sw: f32,
        sh: f32,
        quads_overlay: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) {
        // #489: derive each hint cell's x/w from the shared snapped-edge
        // cache so quick-select hint backgrounds share device-pixel
        // edges with adjacent glyph cells at fractional DPI.
        let raw_fallback = snapped_cell_x.is_empty();
        for hint in &quick_select.hints {
            let Some(visible_row) = hint.row.checked_sub(scrollback_len) else { continue };
            if visible_row >= visible_rows {
                continue;
            }
            let (x, w) = if raw_fallback {
                (origin_x + hint.col_start as f32 * self.cell_w, self.cell_w)
            } else {
                let col = (hint.col_start).min(snapped_cell_x.len().saturating_sub(2));
                let lo = snapped_cell_x[col];
                let hi = snapped_cell_x[col + 1];
                (lo, hi - lo)
            };
            let y = origin_y + visible_row as f32 * self.cell_h;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                color: self.cursor_color,
                ..Default::default()
            });
        }
    }

    /// Shape a single style-run worth of cells and append the
    /// resulting glyph instances + missing-glyph tofus to the frame's
    /// queues. Factored out of the per-row loop so the loop body stays
    /// readable; otherwise it would inline ~80 lines of placement +
    /// fallback handling four times (run start, mid-row flush, end of
    /// row, etc.).
    ///
    /// T9 (wezterm-takeover G2/C): non-ASCII clusters drive through
    /// `shape_run_with_wezterm` only — the cosmic-text path plus the
    /// legacy wezterm-cluster-width overlay are gone. Each cluster
    /// lead cell dispatches on
    /// [`sonicterm_block_glyph::BlockKey::from_char`]: on `Some`, the
    /// atlas pulls a [`sonicterm_block_glyph::block_sprite`] tile
    /// keyed under the block-glyph sentinel
    /// (`GlyphKey { font_slot: 0xFF, glyph_id: <hashed SizedBlockKey>,
    /// .. }`) so the wezterm shape path and the block-sprite path
    /// share the atlas without colliding; on `None`, the cluster
    /// follows the normal sonicterm-font rasterize path. Box drawing,
    /// Powerline, Sextant, Octant, and Braille all reach the renderer
    /// through this dispatch — there is no fallback to the swash-
    /// rasterized font glyph for codepoints `BlockKey` recognizes.
    // Hot inner-loop helper called per shaped run per row. Every
    // argument is an exclusive `&mut` borrow of a *different* field of
    // `GpuRenderer` (atlas, rasterizer, instance buffers, missing-glyph
    // trackers) — bundling them into a struct would force a single
    // `&mut Ctx` that conflicts with the surrounding loop's own
    // borrows. Suppression stays with this explanatory comment.
    #[allow(clippy::too_many_arguments)]
    fn flush_shape_run(
        glyph_atlas: &mut GlyphAtlas,
        _font_family: &str,
        _font_size: f32,
        glyph_instances: &mut Vec<GlyphInstance>,
        missing_tofu: &mut Vec<(f32, f32, f32, f32, ChromeColor)>,
        missing_chars_this_frame: &mut Vec<char>,
        row: u16,
        _run_first_col: u16,
        style: RunStyle,
        cells: &[(u16, Cell)],
        theme: &Theme,
        fg_default: ChromeColor,
        cell_w: f32,
        cell_h: f32,
        top_inset: f32,
        _pad: f32,
        sw: f32,
        sh: f32,
        baseline_y_in_cell: f32,
        snapped_cell_x: &[f32],
        // T9 (wezterm-takeover G2/C): `font_stack` is now the sole
        // shape entry point — when None, the non-ASCII branch can
        // emit nothing (test fixtures without bundled fonts hit
        // this; the ASCII branch still drives through `wt_raster`
        // if it's been wired). The Option shape is kept so
        // `GpuRenderer::new` can continue to construct a partly-
        // degraded renderer in tests.
        font_stack: Option<&sonicterm_engine::FontStack>,
        // T13/T14 (wezterm-takeover G3): sonicterm-font is now the sole
        // atlas insertion path. The legacy `rasterizer: &mut
        // SwashRasterizer` parameter is gone (T10 deletes the type
        // entirely). When `wt_raster` is None (test fixtures without
        // a FontStack), the function emits no glyphs — the renderer
        // still paints quads (bg, cursor, underlines) so the frame is
        // visually coherent.
        mut wt_raster: Option<&mut sonicterm_engine::FontStack>,
    ) {
        if cells.is_empty() {
            return;
        }

        // ASCII fast path: every cell is printable-ASCII (0x20..=0x7E)
        // with no cluster extras and no ligature trigger, so the shaper
        // would emit a 1:1 mapping anyway. Skip the shape call entirely
        // and drive the glyph atlas straight from each cell's GlyphKey.
        //
        // T9: ASCII codepoints (0x20..=0x7E) never overlap the
        // `BlockKey::from_char` ranges (≥ U+2500) and never carry a
        // Powerline / NF PUA codepoint, so the BlockKey dispatch is
        // safely skipped here.
        if run_is_ascii_fast(cells) {
            for (col, cell) in cells {
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: cell.ch,
                    font_slot: 0,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                };
                // T13/T14: sonicterm-font owns the atlas. No swash
                // fallback — when `wt_raster` is None (test fixture
                // without a FontStack) the glyph is silently skipped
                // so the renderer still paints quads.
                let Some(wt) = wt_raster.as_deref_mut() else {
                    continue;
                };
                let info_opt = glyph_atlas.get_or_insert(key, wt);
                let Some(info) = info_opt else {
                    if !cell.ch.is_whitespace() {
                        missing_chars_this_frame.push(cell.ch);
                    }
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    continue;
                }
                let cx = snapped_cell_x[*col as usize];
                let cy = top_inset + f32::from(row) * cell_h;
                // G1a: atlas px == draw px == raster px, so the prior
                // atlas-to-logical projection collapses to the identity.
                let inv_s = 1.0_f32;
                let gx = cx + info.px_offset[0] as f32 * inv_s;
                let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
                let gw = info.px_size[0] as f32 * inv_s;
                let gh = info.px_size[1] as f32 * inv_s;
                // T13/T14: the legacy `apply_symbol_fit_v2` +
                // `block_element_rect` overlay tracks the SwashRasterizer
                // path; sonicterm-font handles cell fit natively. ASCII
                // glyphs are always `Natural` (identity) so dropping
                // the overlay is a no-op for the steady-state hot path.
                let (gx, gy, gw, gh) =
                    sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
                let color = cell_fg(cell, theme, fg_default);
                let rgba = chrome_color_to_linear_rgba(color);
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: [0.0, 0.0, 0.0, 0.0],
                });
            }
            return;
        }

        // ── Non-ASCII / mixed run ── T9: drive sonicterm-font directly.
        //
        // Build the text + byte-to-col map for `shape_run_with_wezterm`.
        // Identical to the legacy cluster-width overlay's input
        // assembly so wezterm sees the same input bytes.
        let Some(stack) = font_stack else {
            // No FontStack → no shaper available; non-ASCII clusters
            // emit nothing this frame. Test-only path (production
            // always carries a stack).
            return;
        };
        let mut text = String::with_capacity(cells.len() * 2);
        let mut cell_cols: Vec<u16> = Vec::with_capacity(cells.len() * 2);
        for (col, cell) in cells {
            let start = text.len();
            text.push(cell.ch);
            if let Some(extras) = cell.extras() {
                for ch in extras.chars() {
                    text.push(ch);
                }
            }
            let appended = text.len() - start;
            for _ in 0..appended {
                cell_cols.push(*col);
            }
        }
        if text.is_empty() {
            return;
        }

        let infos = match stack.shape_text(&text) {
            Ok(v) => v,
            Err(_) => return,
        };

        // Build a lookup from col → cell so we can recover per-cell
        // attributes (color, WIDE flag, the actual codepoint for tofu
        // diagnostics) from the shaped output's `lead_col`.
        let mut cell_by_col: std::collections::HashMap<u16, Cell> =
            std::collections::HashMap::with_capacity(cells.len());
        for (col, c) in cells {
            cell_by_col.insert(*col, c.clone());
        }

        // We consume WezTerm's GlyphInfo directly here and project it into the
        // Sonic glyph record the rest of the renderer already uses. No
        // WtShapedGlyph wrapper: cluster byte offsets map straight back through
        // `cell_cols`.
        let mut shaped = Vec::with_capacity(infos.len());
        let mut last_col: u16 = cell_cols.first().copied().unwrap_or(0);
        for info in infos {
            let cluster_byte = info.cluster as usize;
            let lead_col = cell_cols
                .get(cluster_byte)
                .copied()
                .or_else(|| (0..=cluster_byte).rev().find_map(|i| cell_cols.get(i).copied()))
                .unwrap_or(last_col);
            last_col = lead_col;
            let lead_ch =
                cell_by_col.get(&lead_col).map(|c| c.ch).or(info.only_char).unwrap_or(' ');
            shaped.push(sonicterm_text::shape::ShapedGlyph {
                lead_col,
                cluster_cells: info.num_cells as u16,
                font_slot: u8::try_from(info.font_idx).unwrap_or(u8::MAX),
                glyph_id: info.glyph_pos,
                x_advance: info.x_advance.get() as f32,
                y_offset: info.y_offset.get() as f32,
                ch: lead_ch,
            });
        }

        // Belt-and-braces (#594): once shape grouped a ligature cluster
        // into ONE ShapedGlyph, no SUBSEQUENT glyph should land inside
        // that cluster's column span. debug_only — release builds skip
        // the bookkeeping entirely.
        #[cfg(debug_assertions)]
        let mut _covered_span_end: Option<u16> = None;

        for g in &shaped {
            #[cfg(debug_assertions)]
            {
                if let Some(end) = _covered_span_end {
                    debug_assert!(
                        g.lead_col >= end,
                        "shape_run_with_wezterm emitted overlapping ShapedGlyphs: \
                         lead_col={} lands inside previously-emitted cluster \
                         ending at col {} (ch={:?}, cluster_cells={})",
                        g.lead_col,
                        end,
                        g.ch,
                        g.cluster_cells,
                    );
                }
                _covered_span_end = Some(g.lead_col + g.cluster_cells.max(1));
            }
            let lead_cell = cell_by_col.get(&g.lead_col).cloned().unwrap_or_default();
            let is_wide = lead_cell.flags.contains(CellFlags::WIDE);
            let cluster_cells = g.cluster_cells.max(1) as usize;
            let cells_to_span = if is_wide { 2 } else { cluster_cells };
            let cell_pixel_width = cell_w * cells_to_span as f32;

            // ── T9: BlockKey dispatch at the cluster lead cell ──
            //
            // Box-drawing (U+2500..=U+259F), Powerline (U+E0A0..=U+E0D7),
            // Sextant (U+1FB00..), Octant, and Braille (U+2800..) all
            // recognize via `BlockKey::from_char`. When the lead cell
            // resolves, the vendored wezterm geometry produces the
            // glyph; the atlas keys it under
            // `(font_slot = 0xFF, glyph_id = hashed SizedBlockKey)` so
            // it never collides with a wezterm-shaped glyph
            // (`FallbackIdx` truncated to u8 cannot reach 0xFF in
            // practice — wezterm chains a handful of fallbacks, never
            // 255). The shaper-reported `glyph_id` is intentionally
            // ignored for this branch — wezterm itself draws block
            // glyphs through the same `customglyph::block_sprite` we
            // vendored, so taking the font glyph would produce the
            // wrong rendering (or tofu, if the chosen face lacks the
            // codepoint).
            if let Some(block_key) = sonicterm_block_glyph::BlockKey::from_char(lead_cell.ch) {
                let cx = snapped_cell_x[g.lead_col as usize];
                let cy = top_inset + f32::from(row) * cell_h;
                let span = if is_wide { 2usize } else { cluster_cells };
                let end_col = ((g.lead_col as usize) + span).min(snapped_cell_x.len() - 1);
                let cell_pixel_width_snapped =
                    snapped_cell_x[end_col] - snapped_cell_x[g.lead_col as usize];
                // Cell box used both as the block_sprite metric and as
                // the on-screen rect. raster-px throughout.
                let cell_w_i = cell_w.round().max(1.0) as isize;
                let cell_h_i = cell_h.round().max(1.0) as isize;
                // Bug 4 / wezterm-takeover: stroke width for the
                // `PolyStyle::Outline` box-drawing path comes from the
                // font's actual `underline_thickness`, mirroring
                // wezterm-gui's `utilsprites.rs:29` (`metrics
                // .underline_thickness.get().round().max(1.) as isize`).
                // A hardcoded 1 was producing nearly-invisible 1-device-px
                // strokes that looked like tofu rectangles at every font
                // size — the user-reported "U+2500 renders as a single
                // tofu box" symptom. Use the font metric when we have
                // it; fall back to a 1/16-cell-height heuristic for the
                // test fixture path with no FontStack.
                let underline_h_isize: isize = font_stack
                    .and_then(|s| s.cell_metrics_raster_px().ok())
                    .map(|m| m.underline_h.round().max(1.0) as isize)
                    .unwrap_or_else(|| ((cell_h / 16.0).round().max(1.0)) as isize);
                let size = sonicterm_block_glyph::glue::Size::new(cell_w_i, cell_h_i);
                let sized_key = sonicterm_block_glyph::SizedBlockKey { block: block_key, size };
                // BlockKey identity collapses to a u32 via the std
                // `DefaultHasher`. Block glyphs are size-sensitive
                // (the same key at a different cell pitch produces a
                // different bitmap) so the hash inputs include the
                // packed cell dims as well as the variant. We don't
                // need cryptographic strength — only collision
                // resistance among the ~hundred block glyphs the
                // renderer touches per frame; `DefaultHasher` is
                // overkill but free.
                //
                // Bug 4 fix: `underline_h_isize` participates in the
                // hash too — the same SizedBlockKey at the same cell
                // size renders with a different stroke width when the
                // font's underline_thickness changes (e.g. live font
                // family swap), so the cached tile would be stale
                // without this bit of the key.
                let glyph_id_u32: u32 = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    sized_key.hash(&mut h);
                    underline_h_isize.hash(&mut h);
                    let h64 = h.finish();
                    // Fold to u32 by xoring the halves so all 64 bits
                    // contribute to the atlas key.
                    ((h64 >> 32) as u32) ^ (h64 as u32)
                };
                // Block glyphs ignore bold/italic — the geometry is the
                // same regardless of cell style, so collapse those bits
                // to keep the cache footprint minimal.
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: lead_cell.ch,
                    font_slot: 0xFF,
                    weight_bold: false,
                    italic: false,
                    glyph_id: glyph_id_u32,
                };
                // Wrap `block_sprite` in a thin `Rasterizer` so the
                // atlas only computes the sprite on a cache miss.
                // Identity is captured by `key` above; the rasterizer
                // ignores its `GlyphKey` argument and returns the
                // tile derived from `sized_key`.
                struct BlockSpriteRasterizer {
                    sized_key: sonicterm_block_glyph::SizedBlockKey,
                    underline_h: isize,
                }
                impl sonicterm_text::glyph_atlas::Rasterizer for BlockSpriteRasterizer {
                    fn rasterize(
                        &mut self,
                        _key: sonicterm_types::glyph_key::GlyphKey,
                    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
                        // Synthesize the BlockCellMetrics input that
                        // `block_sprite` expects. Customglyph reads
                        // `cell_size`, `underline_height`, and (only
                        // under the `PolyWithCustomMetrics` arm)
                        // descender / descender_row / descender_plus_two
                        // / strike_row. Cell metrics are derived from
                        // the SizedBlockKey's `size`. The underline
                        // height arrives from the font (Bug 4 fix —
                        // hardcoded 1 made Outline strokes invisible).
                        // anti_alias=true — matches the wezterm-gui
                        // default behavior (`config.anti_alias = true`).
                        // We don't surface a config knob: per spec
                        // "where wezterm and sonicterm disagree,
                        // wezterm wins" + the upstream default is AA.
                        let block_tile = sonicterm_block_glyph::block_sprite_with_cell_metrics(
                            self.sized_key,
                            self.underline_h,
                            true,
                        )
                        .ok()?;
                        // T7 Option A: field-for-field copy
                        // `BlockRasterTile` → `RasterTile`. Same 7
                        // fields, same semantics; T10 may collapse the
                        // duplicate by re-exporting `RasterTile`
                        // directly from `sonicterm-text` once that
                        // crate compiles again.
                        let alpha_mask: Vec<u8> =
                            block_tile.coverage.chunks_exact(4).map(|px| px[3]).collect();
                        Some(sonicterm_text::glyph_atlas::RasterTile {
                            width: block_tile.width,
                            height: block_tile.height,
                            offset_x: block_tile.offset_x,
                            offset_y: block_tile.offset_y,
                            advance: block_tile.advance,
                            coverage: alpha_mask,
                            // WezTerm customglyph geometry is a mask for the
                            // cell foreground, not a self-colored emoji. Treat
                            // it as monochrome coverage so brand/icons like
                            // claude's red block logo inherit SGR fg.
                            is_color: false,
                        })
                    }
                }
                let mut block_raster =
                    BlockSpriteRasterizer { sized_key, underline_h: underline_h_isize };
                let Some(info) = glyph_atlas.get_or_insert(key, &mut block_raster) else {
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    continue;
                }
                // Block glyphs are cell-sized BGRA tiles aligned to the
                // cell-box origin. No baseline offset, no symbol-fit,
                // no per-glyph stretching — `block_sprite` already
                // emits exactly the cell rect, so we draw it 1:1 at
                // `(cx, cy)` with width = the snapped cell-box width.
                // Color tiles are pre-shaded (BGRA); the shader skips
                // the `cov * fg_color` modulation.
                let gx = cx;
                let gy = cy;
                let gw = cell_pixel_width_snapped;
                let gh = cell_h;
                let (gx, gy, gw, gh) =
                    sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
                let color = cell_fg(&lead_cell, theme, fg_default);
                // block_sprite emits BGRA tiles. The atlas reports
                // `is_color = true`, so the shader uses the tile
                // unmodulated; set `color` to white as a safety net
                // mirroring the color-emoji path.
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    chrome_color_to_linear_rgba(color)
                };
                tracing::debug!(
                    target: "sonic::render::glyph",
                    ch = ?lead_cell.ch,
                    codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                    code_u32 = lead_cell.ch as u32,
                    final_rect = ?(gx, gy, gw, gh),
                    final_rgba = ?rgba,
                    is_color = info.is_color,
                    path = "block_sprite",
                    "glyph render emit (block-glyph)"
                );
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
                });
                continue;
            }

            // ── Normal wezterm-shape path (non-block cluster) ──
            //
            // T13/T14: post-the legacy chrome layer the char-fallback path is wezterm-
            // only. FontStack is the sole rasterizer; missing chars
            // emit tofu via `Rasterizer::rasterize` returning
            // None (when sonicterm-font's fallback chain has nothing).
            if g.glyph_id == 0 {
                let ch = lead_cell.ch;
                if ch == '\0' || ch.is_whitespace() {
                    continue;
                }
                // T13/T14: drop the `resolve_slot` swash walk. wezterm
                // handles fallback internally — pass `font_slot = 0`
                // and let `FontStack::rasterize` find a face
                // (it shapes the single char against the loaded font
                // when glyph_id == 0).
                let slot: u8 = 0;
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch,
                    font_slot: slot,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                };
                let Some(wt) = wt_raster.as_deref_mut() else {
                    continue;
                };
                let info_opt = glyph_atlas.get_or_insert(key, wt);
                let Some(info) = info_opt else {
                    // True tofu — wezterm fallback chain rejected.
                    let cx = snapped_cell_x[g.lead_col as usize];
                    let cy = top_inset + f32::from(row) * cell_h;
                    let inset = (cell_h * 0.12).max(1.0);
                    missing_tofu.push((
                        cx + inset,
                        cy + inset,
                        cell_pixel_width - inset * 2.0,
                        cell_h - inset * 2.0,
                        cell_fg(&lead_cell, theme, fg_default),
                    ));
                    missing_chars_this_frame.push(ch);
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    continue;
                }
                let cx = snapped_cell_x[g.lead_col as usize];
                let cy = top_inset + f32::from(row) * cell_h;
                let inv_s = 1.0_f32;
                let gx = cx + info.px_offset[0] as f32 * inv_s;
                let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
                let gw = info.px_size[0] as f32 * inv_s;
                let gh = info.px_size[1] as f32 * inv_s;
                // Bug 2 / wezterm-takeover § "Prefer wezterm everywhere":
                // No `gw > cell_pixel_width_snapped` clamp here either —
                // see the matching comment in the main shaped branch below.
                // sonicterm-font sizes glyphs to the cell box natively for
                // typical cells; ligature halves intentionally exceed it
                // and must be allowed to.
                let color = cell_fg(&lead_cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    chrome_color_to_linear_rgba(color)
                };
                let (gx, gy, gw, gh) =
                    sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
                });
                continue;
            }

            let key = sonicterm_types::glyph_key::GlyphKey::shaped(
                g.ch,
                g.font_slot,
                g.glyph_id,
                style.bold,
                style.italic,
            );
            // T13/T14: sonicterm-font is the sole rasterizer; the
            // legacy `swash_rasterizer::classify_symbol` / SymbolFit
            // family routes through the SwashRasterizer which is gone.
            // sonicterm-font sizes glyphs natively, so the IconCellFit
            // resample helper isn't needed either. Atlas keys remain
            // identical (font_slot, glyph_id) so cached tiles survive.
            let span_cells = if is_wide { 2usize } else { g.cluster_cells.max(1) as usize };
            let span_end_col = ((g.lead_col as usize) + span_cells).min(snapped_cell_x.len() - 1);
            let cell_box_w_logical =
                snapped_cell_x[span_end_col] - snapped_cell_x[g.lead_col as usize];
            let _ = cell_box_w_logical;
            let Some(wt) = wt_raster.as_deref_mut() else {
                continue;
            };
            let Some(info) = glyph_atlas.get_or_insert(key, wt) else {
                continue;
            };
            if info.px_size[0] == 0 || info.px_size[1] == 0 {
                continue;
            }
            let cx = snapped_cell_x[g.lead_col as usize];
            let cy = top_inset + f32::from(row) * cell_h;
            let inv_s = 1.0_f32;
            let gx = cx + info.px_offset[0] as f32 * inv_s;
            let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
            let gw = info.px_size[0] as f32 * inv_s;
            let gh = info.px_size[1] as f32 * inv_s;
            // Bug 2 / wezterm-takeover § "Prefer wezterm everywhere":
            // No `gw > cell_pixel_width_snapped` clamp here. Half-
            // ligature glyphs from sonicterm-font's GSUB substitutions
            // (e.g. `=>` substitutes the source `=` into glyph_id
            // 41082 and `>` into glyph_id 40766; each is a 16-px-wide
            // bitmap with `bearing_x = -8` so the two halves visually
            // fuse across cells N and N+1) MUST be allowed to extend
            // beyond a single cell — wezterm-gui takes the same
            // approach (see `wezterm-gui/src/termwindow/render/
            // screen_line.rs` `width = sprite.coords.size.width *
            // scale`, no cell-width cap). Squashing the bitmap to one
            // cell collapses the ligature into a single-cell glyph
            // with an inert neighbour cell.
            let color = cell_fg(&lead_cell, theme, fg_default);
            let rgba = if info.is_color {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                chrome_color_to_linear_rgba(color)
            };
            let (gx, gy, gw, gh) =
                sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
            glyph_instances.push(GlyphInstance {
                rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                uv: info.uv,
                color: rgba,
                flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            });
        }
    }
}

fn cell_fg(cell: &Cell, theme: &Theme, default: ChromeColor) -> ChromeColor {
    if cell.flags.contains(CellFlags::INVERSE) {
        let default_bg = hex_to_chrome_color(theme.colors.background.0.as_str());
        color_to_chrome(cell.bg, theme, default_bg)
    } else {
        color_to_chrome(cell.fg, theme, default)
    }
}

fn color_to_chrome(color: Color, theme: &Theme, default: ChromeColor) -> ChromeColor {
    match color {
        Color::Default => default,
        Color::Rgb(r, g, b) => ChromeColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_inline_image_instances(
    glyph_atlas: &mut GlyphAtlas,
    out: &mut Vec<GlyphInstance>,
    images: &[sonicterm_render_model::InlineImage],
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
) {
    for image in images {
        if image.width == 0 || image.height == 0 || image.bgra.is_empty() {
            continue;
        }
        let key = sonicterm_types::glyph_key::GlyphKey {
            ch: '\u{fffc}',
            font_slot: 0xFE,
            weight_bold: false,
            italic: false,
            glyph_id: fold_u64_to_u32(image.id),
        };
        struct ImageRasterizer<'a> {
            image: &'a sonicterm_render_model::InlineImage,
        }
        impl sonicterm_text::glyph_atlas::Rasterizer for ImageRasterizer<'_> {
            fn rasterize(
                &mut self,
                _key: sonicterm_types::glyph_key::GlyphKey,
            ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
                Some(sonicterm_text::glyph_atlas::RasterTile {
                    width: self.image.width,
                    height: self.image.height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: self.image.width as f32,
                    coverage: self.image.bgra.as_ref().to_vec(),
                    is_color: true,
                })
            }
        }
        let mut raster = ImageRasterizer { image };
        let Some(info) = glyph_atlas.get_or_insert(key, &mut raster) else {
            continue;
        };
        let x = origin_x + image.col as f32 * cell_w;
        let y = origin_y + image.row as f32 * cell_h;
        out.push(GlyphInstance {
            rect: px_to_ndc(x, y, info.px_size[0] as f32, info.px_size[1] as f32, sw, sh),
            uv: info.uv,
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [1.0, 0.0, 0.0, 0.0],
        });
    }
}

fn fold_u64_to_u32(value: u64) -> u32 {
    ((value >> 32) as u32) ^ (value as u32)
}

fn underline_key(cell: &Cell) -> Option<(UnderlineStyle, Color)> {
    cell.flags
        .contains(CellFlags::UNDERLINE)
        .then(|| (cell.underline_style(), cell.underline_color().unwrap_or(cell.fg)))
}

#[allow(clippy::too_many_arguments)]
fn push_underline_quads(
    out: &mut Vec<QuadInstance>,
    style: UnderlineStyle,
    x: f32,
    y: f32,
    w: f32,
    cell_h: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    if w <= 0.0 {
        return;
    }
    let bottom_y = y + cell_h - thickness;
    match style {
        UnderlineStyle::Single => {
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Double => {
            let gap = thickness.max(1.0);
            let y1 = (bottom_y - gap - thickness).max(y);
            out.push(QuadInstance::sharp(px_to_ndc(x, y1, w, thickness, sw, sh), color));
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Dotted => {
            let dot = (thickness * 1.6).max(1.0);
            let step = dot * 2.0;
            let mut dx = 0.0;
            while dx < w {
                let size = dot.min(w - dx);
                out.push(QuadInstance::rounded(
                    px_to_ndc(x + dx, bottom_y, size, dot, sw, sh),
                    color,
                    [size, dot],
                    dot * 0.5,
                ));
                dx += step;
            }
        }
        UnderlineStyle::Dashed => {
            let dash = (thickness * 4.0).max(4.0);
            let gap = (thickness * 2.0).max(2.0);
            let mut dx = 0.0;
            while dx < w {
                let len = dash.min(w - dx);
                out.push(QuadInstance::sharp(
                    px_to_ndc(x + dx, bottom_y, len, thickness, sw, sh),
                    color,
                ));
                dx += dash + gap;
            }
        }
        UnderlineStyle::Curly => {
            let amp = (thickness * 1.4).max(1.0);
            let step = (thickness * 4.0).max(4.0);
            let mid_y = y + cell_h - thickness - amp;
            let mut sx = x;
            let mut up = true;
            while sx < x + w {
                let ex = (sx + step).min(x + w);
                let sy = if up { mid_y + amp } else { mid_y - amp };
                let ey = if up { mid_y - amp } else { mid_y + amp };
                push_line_segment_px(out, sx, sy, ex, ey, thickness, sw, sh, color);
                sx = ex;
                up = !up;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_line_segment_px(
    out: &mut Vec<QuadInstance>,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    let pad = thickness * 0.5 + 1.0;
    let x0 = ax.min(bx) - pad;
    let y0 = ay.min(by) - pad;
    let x1 = ax.max(bx) + pad;
    let y1 = ay.max(by) + pad;
    let w = (x1 - x0).max(1.0);
    let h = (y1 - y0).max(1.0);
    let cx = x0 + w * 0.5;
    let cy = y0 + h * 0.5;
    out.push(QuadInstance::line(
        px_to_ndc(x0, y0, w, h, sw, sh),
        color,
        [w, h],
        [ax - cx, ay - cy],
        [bx - cx, by - cy],
        thickness,
    ));
}

/// Resolve a cell's background to a linear-space `[r,g,b,a]` suitable for the
/// quad pipeline, OR `None` if the cell should fall through to the surface
/// clear color (`theme.colors.background`).
///
/// Returning `None` for the default-bg case lets the per-row emit loop skip
/// pushing a no-op quad over every blank cell — the `LoadOp::Clear(self.bg)`
/// already covers that area.
///
/// Note on color space: the wgpu surface is `Bgra8UnormSrgb`, so the quad
/// fragment shader's output is sRGB-encoded on write. Inputs MUST therefore
/// be in linear-light space, otherwise gamma is applied twice and the result
/// looks washed out (same trap documented in `color.rs::hex_to_rgba`). The
/// sRGB→linear LUT here is bit-exact with the one feeding `hex_to_rgba`, so
/// `Color::Indexed(1)` (ANSI red) ends up identical to the theme's `ansi.red`
/// rendered through the LoadOp clear path.
#[doc(hidden)]
pub fn cell_bg_rgba(cell: &Cell, theme: &Theme) -> Option<[f32; 4]> {
    let color = if cell.flags.contains(CellFlags::INVERSE) {
        let default_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        color_to_chrome(cell.fg, theme, default_fg)
    } else {
        match cell.bg {
            Color::Default => return None,
            bg => color_to_chrome(bg, theme, ChromeColor::rgb(0, 0, 0)),
        }
    };
    let lut = super::color::srgb_u8_to_linear_lut();
    Some([lut[color.r() as usize], lut[color.g() as usize], lut[color.b() as usize], 1.0])
}

/// Walk the visible rows of `grid`, emit one `QuadInstance` per maximal run
/// of horizontally-adjacent cells that share the same non-default background
/// color. Cells whose `bg` resolves to the theme default are skipped — the
/// surface `LoadOp::Clear(theme.background)` already covers them, so emitting
/// a quad there would be wasted bandwidth.
///
/// Run-length coalescing is essential: a single `\033[41m` color-fill of an
/// 80-column row would otherwise produce 80 quads where 1 suffices. The
/// renderer can hit tens of thousands of background cells per frame during
/// e.g. `htop` or `vim` syntax highlighting; per-cell quads would blow the
/// instance buffer and tank fill-rate.
///
/// `WIDE_CONT` cells (the right half of a wide CJK cell) inherit the lead
/// cell's bg via the parser, so they participate in the same run naturally.
///
/// The emitted quads are sharp-edged (no SDF) and pushed onto `out` in row-
/// major order. Caller is responsible for placing this BEFORE selection /
/// cursor / overlay quads in the draw vector so those still paint on top.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)] // mirrors flush_shape_run / collect_hyperlink_runs siblings — all geometry must be threaded in explicitly to keep this a free function (testable without a full GpuRenderer)
pub fn emit_cell_bg_quads(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    emit_cell_bg_quads_clipped(
        grid,
        view_top_abs,
        theme,
        PaneRect {
            x: pad,
            y: top_inset,
            w: f32::from(grid.cols) * cell_w,
            h: f32::from(grid.rows) * cell_h,
        },
        cell_w,
        cell_h,
        sw,
        sh,
        out,
    );
}

/// Like [`emit_cell_bg_quads`] but clips runs to a pane sub-rect. This is
/// the production split-pane path: a pane whose grid is wider than its
/// current tile must never emit quads into its neighbour's rectangle.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_clipped(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pane_rect: PaneRect,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    let pad = pane_rect.x;
    let top_inset = pane_rect.y;
    let max_cols = ((pane_rect.w / cell_w).floor() as i32).clamp(0, i32::from(grid.cols)) as u16;
    let max_rows = ((pane_rect.h / cell_h).floor() as i32).clamp(0, i32::from(grid.rows)) as u16;
    if max_cols == 0 || max_rows == 0 {
        return;
    }
    // #489: build this pane's snapped-edge cache once. Per the
    // diagnosis, per-pane bg builds its own cache (not the active
    // pane's) so split-pane bg edges stay aligned with that pane's
    // glyph cells. G1a: `build_snapped_cell_x` no longer takes a
    // scale parameter — inputs are raster px already, so snapping
    // is fixed to scale = 1.0 internally.
    let snapped_cell_x = build_snapped_cell_x(pad, cell_w, grid.cols);
    for r in 0..max_rows {
        emit_cell_bg_quads_for_row(
            grid,
            view_top_abs,
            theme,
            pad,
            top_inset,
            cell_w,
            cell_h,
            sw,
            sh,
            max_cols,
            r,
            out,
            &snapped_cell_x,
        );
    }
}

/// #489: shared device-pixel-snapped column-edge cache. Returns
/// `cols + 1` entries where slot `c` is the snapped left edge of cell
/// `c`, and slot `c + span` is its right edge. Every overlay/glyph
/// path that derives a horizontal rect from a column index must read
/// from this cache so adjacent overlays share an exact device-pixel
/// edge with the glyph cells they cover.
///
/// G1a (wezterm-takeover): inputs are raster pixels, so "snapping to
/// device pixels" reduces to integer-pixel rounding — the helper now
/// passes scale = 1.0 to [`snap_to_device_pixels`] (raster px IS the
/// device-pixel grid) instead of threading the renderer's DPI scale
/// through the call. Behaviour at integer DPIs is identical to the
/// pre-G1a `cell_w * scale` arithmetic; at fractional DPIs the new
/// path matches what the renderer actually paints (a single integer
/// raster-pixel-aligned grid) rather than the prior logical-px
/// half-pixel cache.
#[doc(hidden)]
#[must_use]
pub fn build_snapped_cell_x(origin_x: f32, cell_w: f32, cols: u16) -> Vec<f32> {
    (0..=cols)
        .map(|col| {
            sonicterm_render_model::geometry::snap_to_device_pixels(
                (origin_x + (col as f32) * cell_w, 0.0, 0.0, 0.0),
                1.0,
            )
            .0
        })
        .collect()
}

/// Pure column-from-pixel lookup that mirrors the renderer's
/// device-pixel-snapped edge cache (#569). `edges` is the output of
/// `build_snapped_cell_x` for the pane in question (length `cols + 1`).
/// Returns `Some(col)` for any `px` in `[edges[0], edges[cols])` using
/// half-open buckets `edges[col] <= px < edges[col+1]` — boundary px
/// resolve to the RHS cell, matching the renderer's draw bias.
///
/// Returns `None` if `px` is left of `edges[0]` or `>= edges[cols]`
/// (caller already gated negatives via the pane resolution step, but
/// this is defensive). Returns `None` if `edges` is malformed
/// (`len < 2`) — that only happens for a 0-col pane, which has no
/// addressable cell to begin with.
#[doc(hidden)]
#[must_use]
pub fn pixel_to_local_col(px: f32, edges: &[f32], cols: u16) -> Option<u16> {
    if cols == 0 || edges.len() < 2 {
        return None;
    }
    if px < edges[0] {
        return None;
    }
    if px >= edges[cols as usize] {
        return None;
    }
    // Linear scan: half-open buckets edges[i] <= px < edges[i+1].
    // Cell counts are bounded (<= a few hundred) so a scan beats the
    // branch overhead of binary search at typical widths. For very wide
    // grids (cols >> 200) this could switch to `partition_point` — the
    // input is monotone non-decreasing by construction.
    for i in 0..cols as usize {
        if px < edges[i + 1] {
            return Some(i as u16);
        }
    }
    // Unreachable given the `>= edges[cols]` guard above, but keep the
    // total function obvious.
    None
}

/// Emit background quads for a single visible row. Extracted so the
/// `LineQuadCache` miss path (Epic #300 P2) can call it for one row
/// at a time and capture the resulting quads into the cache.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_for_row(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    max_cols: u16,
    r: u16,
    out: &mut Vec<QuadInstance>,
    snapped_cell_x: &[f32],
) {
    {
        let row_abs = view_top_abs + r as u64;
        let Some(row) = grid.row_at_abs(row_abs) else {
            return;
        };
        // Run-length encode adjacent same-bg cells into one quad.
        let mut run_start: Option<u16> = None;
        let mut run_color: Option<[f32; 4]> = None;
        let mut col: u16 = 0;
        // #489: derive x/w from the shared snapped-edge cache so bg
        // runs share device-pixel edges with adjacent glyph cells at
        // fractional DPI. Falls back to raw arithmetic if the cache is
        // empty (defensive — production always passes the full cache).
        let raw_fallback = snapped_cell_x.is_empty();
        let flush =
            |start: u16, end_exclusive: u16, color: [f32; 4], out: &mut Vec<QuadInstance>| {
                let clipped_end = end_exclusive.min(max_cols);
                if clipped_end <= start {
                    return;
                }
                let (x, w) = if raw_fallback {
                    (pad + f32::from(start) * cell_w, f32::from(clipped_end - start) * cell_w)
                } else {
                    let lo = snapped_cell_x[start as usize];
                    let hi = snapped_cell_x[clipped_end as usize];
                    (lo, hi - lo)
                };
                let y = top_inset + f32::from(r) * cell_h;
                out.push(QuadInstance::sharp(px_to_ndc(x, y, w, cell_h, sw, sh), color));
            };
        for cell in row.iter().take(max_cols as usize) {
            let bg = cell_bg_rgba(cell, theme);
            match (run_color, bg) {
                (Some(prev), Some(cur)) if prev == cur => {
                    // extend run
                }
                (Some(prev), _) => {
                    // PANIC: safe — `run_color` and `run_start` are written
                    // together (search this fn for `run_start = ` to see they
                    // are always assigned in the same statement-pair). Matching
                    // `run_color == Some(_)` therefore proves `run_start ==
                    // Some(_)`. Hot per-frame path: no Result conversion.
                    let start = run_start.expect("run_start set when run_color is");
                    flush(start, col, prev, out);
                    run_start = bg.map(|_| col);
                    run_color = bg;
                }
                (None, Some(_)) => {
                    run_start = Some(col);
                    run_color = bg;
                }
                (None, None) => {}
            }
            col = col.saturating_add(1);
        }
        if let (Some(start), Some(color)) = (run_start, run_color) {
            flush(start, col, color, out);
        }
    }
}

fn indexed(i: u8, theme: &Theme) -> Option<ChromeColor> {
    let p = &theme.colors;
    let pick = |h: &str| hex_to_chrome_color(h);
    match i {
        0 => Some(pick(p.ansi.black.0.as_str())),
        1 => Some(pick(p.ansi.red.0.as_str())),
        2 => Some(pick(p.ansi.green.0.as_str())),
        3 => Some(pick(p.ansi.yellow.0.as_str())),
        4 => Some(pick(p.ansi.blue.0.as_str())),
        5 => Some(pick(p.ansi.magenta.0.as_str())),
        6 => Some(pick(p.ansi.cyan.0.as_str())),
        7 => Some(pick(p.ansi.white.0.as_str())),
        8 => Some(pick(p.bright.black.0.as_str())),
        9 => Some(pick(p.bright.red.0.as_str())),
        10 => Some(pick(p.bright.green.0.as_str())),
        11 => Some(pick(p.bright.yellow.0.as_str())),
        12 => Some(pick(p.bright.blue.0.as_str())),
        13 => Some(pick(p.bright.magenta.0.as_str())),
        14 => Some(pick(p.bright.cyan.0.as_str())),
        15 => Some(pick(p.bright.white.0.as_str())),
        16..=231 => {
            let v = i - 16;
            let r = v / 36;
            let g = (v / 6) % 6;
            let b = v % 6;
            let to8bit = |c: u8| if c == 0 { 0 } else { c * 40 + 55 };
            Some(ChromeColor::rgb(to8bit(r), to8bit(g), to8bit(b)))
        }
        232..=255 => {
            let g = (i - 232) * 10 + 8;
            Some(ChromeColor::rgb(g, g, g))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_color_supports_full_xterm_256_palette() {
        let theme = Theme::default();
        assert_eq!(indexed(16, &theme), Some(ChromeColor::rgb(0, 0, 0)));
        assert_eq!(indexed(231, &theme), Some(ChromeColor::rgb(255, 255, 255)));
        assert_eq!(indexed(232, &theme), Some(ChromeColor::rgb(8, 8, 8)));
        assert_eq!(indexed(255, &theme), Some(ChromeColor::rgb(238, 238, 238)));
    }

    #[test]
    fn inverse_swaps_foreground_and_background_for_rendering() {
        let theme = Theme::default();
        let cell = Cell::plain('x', Color::Indexed(1), Color::Indexed(2), CellFlags::INVERSE);
        assert_eq!(cell_fg(&cell, &theme, ChromeColor::WHITE), indexed(2, &theme).unwrap());
        assert_eq!(
            cell_bg_rgba(&cell, &theme),
            Some(chrome_color_to_linear_rgba(indexed(1, &theme).unwrap()))
        );
    }
}

// T13/T14 (wezterm-takeover G3): `hex_to_the legacy chrome layer` and
// `scale_the legacy chrome layer_alpha` have moved into `crate::color` under the
// renamed `hex_to_chrome_color` / `scale_chrome_text_alpha` names and
// now consume `ChromeColor` instead of `legacy chrome color`.
// Re-export them at the legacy path so callers that imported
// `sonicterm_gpu::core::scale_the legacy chrome layer_alpha` can switch to the new
// identifier (see `crates/sonicterm-app/tests/drag_visual_feedback.rs`
// for the port). The legacy names are gone from this file entirely;
// any caller that lingers on them will fail to compile (intentional —
// it's the must-pass #4 grep gate's job to catch survivors).
pub use crate::color::scale_chrome_text_alpha;

// T13/T14: `terminal_font_attrs` re-export removed. It returned
// `legacy chrome attrs` which carried per-span family/weight; the
// chrome-text path replaces it with `ChromeAttrs { bold, italic }`
// constructed per-span at the call site. Downstream callers
// (`sonicterm-ui::tab_spans`) build `(text, ChromeColor, ChromeAttrs)`
// span tuples directly. The grid/chrome shape calls reach the loaded
// wezterm font via `FontStack::default_font()` — there is no per-span
// font attribute layer in this path.

/// Walk the grid and collect runs of contiguous cells that share a hyperlink
/// id, per row. Wide-cell continuations don't break a run (they inherit the
/// lead cell's hyperlink). Returns `(row, col_start, col_end_inclusive)`.
#[doc(hidden)]
pub fn collect_hyperlink_runs(grid: &Grid) -> Vec<(u16, u16, u16)> {
    let mut runs = Vec::new();
    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut start: Option<u16> = None;
        let mut current: Option<sonicterm_grid::hyperlink::HyperlinkId> = None;
        let mut last_col: u16 = 0;
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                if start.is_some() {
                    last_col = col as u16;
                }
                continue;
            }
            match (cell.hyperlink(), current) {
                (Some(hid), Some(cur)) if hid == cur => {
                    last_col = col as u16;
                }
                (Some(hid), _) => {
                    if let (Some(s), Some(_)) = (start, current) {
                        runs.push((r, s, last_col));
                    }
                    start = Some(col as u16);
                    current = Some(hid);
                    last_col = col as u16;
                }
                (None, Some(_)) => {
                    if let Some(s) = start.take() {
                        runs.push((r, s, last_col));
                    }
                    current = None;
                }
                (None, None) => {}
            }
        }
        if let (Some(s), Some(_)) = (start, current) {
            runs.push((r, s, last_col));
        }
    }
    runs
}

// T13/T14: `load_bundled_fonts` (cosmic-text bundle loader) is gone.
// Bundled fonts ship via sonicterm-font's `vendor-jetbrains`,
// `vendor-noto-emoji`, `vendor-nerd-font-symbols` features (see
// `sonicterm-text/Cargo.toml`), so the FontStack discovers them
// automatically without an explicit per-file disk load.

/// Stable fingerprint for command badges, including wall-clock buckets that
/// change when badge visibility can transition without a tab model mutation.
#[doc(hidden)]
pub fn command_status_hash(status: &sonicterm_ui::tabs::CommandStatus, now: Instant) -> u64 {
    match status {
        sonicterm_ui::tabs::CommandStatus::Idle => 0,
        sonicterm_ui::tabs::CommandStatus::Running(started_at) => {
            let elapsed_secs = now.duration_since(*started_at).as_secs().min(5);
            let badge_visible = u64::from(now.duration_since(*started_at).as_secs() > 5);
            1 | (elapsed_secs << 32) | (badge_visible << 40)
        }
        sonicterm_ui::tabs::CommandStatus::Done { exit, until } => {
            let is_past_expiry = u64::from(now >= *until);
            2 | (u64::from(exit.unwrap_or(255)) << 8) | (is_past_expiry << 32)
        }
    }
}

/// Compute the per-row selection quad rects (in physical pixels) that the
/// renderer would emit for `sel` against a grid of `rows` × `cols`, anchored
/// at `(origin_x, origin_y)` with `cell_w × cell_h` cells.
///
/// Pure helper, no clipping applied — pair with [`clip_rect_to_pane`] before
/// pushing to the GPU. Exposed so integration tests can verify the
/// pre-clip / post-clip relationship without standing up a real surface.
///
/// Each returned tuple is `(x, y, w, h)` in physical pixels.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn selection_quad_rects(
    sel: &sonicterm_ui::selection::Selection,
    rows: u16,
    cols: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Vec<(f32, f32, f32, f32)> {
    if sel.is_empty() {
        return Vec::new();
    }
    let (a, b) = sel.normalized();
    let mut out = Vec::with_capacity(usize::from(b.0.saturating_sub(a.0)) + 1);
    // #489: derive each row's x/w from the shared snapped-edge cache so
    // selection rects share device-pixel edges with adjacent glyph
    // cells at fractional DPI. Empty-cache fallback preserves the old
    // raw-arithmetic behavior for callers (debug/test helpers) that
    // don't carry a real cache; integer scales make the two identical.
    let raw_fallback = snapped_cell_x.is_empty();
    for r in a.0..=b.0 {
        if r >= rows {
            break;
        }
        let col_a = if r == a.0 { a.1 } else { 0 };
        // Note: do NOT clamp `col_b` to `cols - 1` here. The selection may
        // legitimately reach the grid's last column, and the per-pane clip
        // below trims any pixel overhang. Clamping pre-clip would silently
        // shrink the selection on the last row when the user dragged past
        // the rightmost cell — which is precisely the path that hides
        // bugs like the split-pane bleed-through.
        let col_b = if r == b.0 { b.1 } else { cols.saturating_sub(1) };
        if col_b < col_a {
            continue;
        }
        let end_exclusive = col_b.saturating_add(1);
        let (x, w) = if raw_fallback {
            (origin_x + f32::from(col_a) * cell_w, f32::from(end_exclusive - col_a) * cell_w)
        } else {
            // Clamp the right edge to the cache bounds (`cols + 1`); a
            // selection that touches col `cols - 1` reads `snapped[cols]`.
            let cache_end = end_exclusive.min((snapped_cell_x.len() - 1) as u16);
            if cache_end <= col_a {
                continue;
            }
            let lo = snapped_cell_x[col_a as usize];
            let hi = snapped_cell_x[cache_end as usize];
            (lo, hi - lo)
        };
        let y = origin_y + f32::from(r) * cell_h;
        out.push((x, y, w, cell_h));
    }
    out
}

/// Clip a quad rect (in physical pixels) to the active pane's bounding box.
/// Returns `None` if the rect is entirely outside the pane.
///
/// Selection / cursor / overlay quads are anchored to the active pane's
/// origin and can extend past its right or bottom edge when the user drags
/// beyond the pane (or the cursor temporarily sits outside the grid bounds
/// due to a resize race). Pushing the unclipped quad would paint into the
/// neighbouring pane in a split layout — see the regression test for
/// the split-right drag-select bug.
#[doc(hidden)]
pub fn clip_rect_to_pane(
    rect: (f32, f32, f32, f32),
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, h) = rect;
    let clipped_x = x.max(pane_x);
    let clipped_right = (x + w).min(pane_x + pane_w);
    let clipped_y = y.max(pane_y);
    let clipped_bottom = (y + h).min(pane_y + pane_h);
    let clipped_w = clipped_right - clipped_x;
    let clipped_h = clipped_bottom - clipped_y;
    if clipped_w > 0.0 && clipped_h > 0.0 {
        Some((clipped_x, clipped_y, clipped_w, clipped_h))
    } else {
        None
    }
}
