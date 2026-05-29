//! GPU renderer for the terminal grid using wgpu 29 + glyphon 0.11.
#![allow(deprecated)] // PR #119 deprecated literal `color::*` helpers — one residual site (drop-line indicator) pending migration.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use sonic_core::{
    config::BackdropKind,
    grid::{Cell, CellFlags, Color, Grid},
    theme::{Color as ThemeColor, Theme},
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, MultisampleState, Operations, PresentMode, RenderPassColorAttachment,
    RenderPassDescriptor, RequestAdapterOptions, SurfaceConfiguration, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use super::color::{glyphon_color_to_linear_rgba, hex_to_rgba, hex_to_wgpu_with_alpha};
use super::cursor::{push_hollow_rect_clipped, recolor_cursor_glyphs, InactivePaneCursor};
use super::drag_chip::{DragChipOverlay, DragChipVisual};
use super::metrics::{atlas_dim_for_scale, measure_cell, natural_line_h_px};
use super::tab_spans::{
    build_tab_title_rich_text_spans, build_tab_title_spans, tab_title_font_size, TabSpanInput,
    TabTitleRichTextSpans,
};

/// Renderer compositor settings that affect surface configuration.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceAppearance {
    /// System backdrop material requested by config.
    pub backdrop: BackdropKind,
    /// Theme background opacity.
    pub opacity: f32,
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

/// Integer-pixel inset for the Windows custom titlebar strip; 0
/// elsewhere. Duplicated (in tiny form) from `sonic_app::app` so this
/// module can stay independent of the app crate.
#[inline]
fn integrated_titlebar_inset_px() -> u32 {
    #[cfg(target_os = "windows")]
    {
        32
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

use crate::{
    atlas_upload::AtlasUpload,
    cheatsheet::{filter_indices, CheatsheetState},
    command_palette::CommandPalette,
    copy_mode::{CopyModeState, QuickSelectState},
    cursor::{self, CursorShape},
    glyph_atlas::GlyphAtlas,
    ime::ImeState,
    overlays::{
        search_bar_label, ImePreeditLayout, PaletteLayout, SearchBarLayout, PALETTE_BORDER,
        PALETTE_INNER_PAD, PALETTE_PANEL_RADIUS, PALETTE_QUERY_RADIUS, PALETTE_ROW_GAP,
        PALETTE_ROW_HEIGHT, PALETTE_ROW_RADIUS,
    },
    pane::{Rect as PaneRect, SplitAxis, SplitterRect},
    quad::{px_to_ndc, QuadInstance, QuadPipeline},
    search::SearchState,
    selection::Selection,
    shape::{run_is_ascii_fast, RunStyle, ShapeCache},
    swash_rasterizer::{self, SwashRasterizer},
    tabbar_view::{tab_bar_height, TabBarLayout, TAB_GAP},
    tabs::TabBar,
    text_pipeline::{GlyphInstance, TextPipeline},
};

struct CheatsheetLayout {
    scrim: crate::tabbar_view::Rect,
    border: crate::tabbar_view::Rect,
    bg: crate::tabbar_view::Rect,
    query_row: crate::tabbar_view::Rect,
    rows: Vec<crate::tabbar_view::Rect>,
    selected_row: Option<usize>,
    query_label: String,
    rows_text: String,
    footer: crate::tabbar_view::Rect,
    footer_label: String,
}

fn compute_cheatsheet_layout(
    state: &CheatsheetState,
    bindings: &[(String, String)],
    window_w: f32,
    window_h: f32,
) -> CheatsheetLayout {
    let modal_w = 760.0_f32.min((window_w - 48.0).max(180.0));
    let modal_h = 520.0_f32.min((window_h - 96.0).max(140.0));
    let border = crate::tabbar_view::Rect {
        x: ((window_w - modal_w) * 0.5).max(0.0),
        y: (window_h * 0.14).max(48.0).min((window_h - modal_h).max(0.0)),
        w: modal_w,
        h: modal_h,
    };
    let bg = crate::tabbar_view::Rect {
        x: border.x + PALETTE_BORDER,
        y: border.y + PALETTE_BORDER,
        w: (border.w - PALETTE_BORDER * 2.0).max(0.0),
        h: (border.h - PALETTE_BORDER * 2.0).max(0.0),
    };
    let query_row = crate::tabbar_view::Rect {
        x: bg.x + PALETTE_INNER_PAD,
        y: bg.y + PALETTE_INNER_PAD,
        w: (bg.w - PALETTE_INNER_PAD * 2.0).max(0.0),
        h: 44.0,
    };
    let footer = crate::tabbar_view::Rect {
        x: bg.x,
        y: (bg.y + bg.h - 32.0).max(query_row.y + query_row.h),
        w: bg.w,
        h: 32.0,
    };
    let list_top = query_row.y + query_row.h + PALETTE_INNER_PAD;
    let list_bottom = footer.y - PALETTE_INNER_PAD;
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
    let mut rows_text = String::new();
    for (row_i, idx_pos) in (window_start..window_end).enumerate() {
        rows.push(crate::tabbar_view::Rect {
            x: bg.x + PALETTE_INNER_PAD,
            y: list_top + (row_i as f32) * row_stride,
            w: (bg.w - PALETTE_INNER_PAD * 2.0).max(0.0),
            h: PALETTE_ROW_HEIGHT,
        });
        if row_i > 0 {
            rows_text.push('\n');
        }
        if let Some((keys, action)) = idxs.get(idx_pos).and_then(|idx| bindings.get(*idx)) {
            rows_text.push_str(keys);
            rows_text.push_str("    ");
            rows_text.push_str(action);
        }
    }
    if total == 0 {
        rows_text.push_str("No shortcuts found");
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
        scrim: crate::tabbar_view::Rect { x: 0.0, y: 0.0, w: window_w, h: window_h },
        border,
        bg,
        query_row,
        rows,
        selected_row,
        query_label,
        rows_text,
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

    pub(crate) font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    /// Second TextRenderer used exclusively for overlay text (palette
    /// query/rows, search input badge, IME preedit). Sharing the atlas with
    /// `text_renderer` keeps glyph caching unified; using a distinct renderer
    /// lets us submit two `render()` calls inside one pass — the overlay
    /// renderer's draw is sequenced AFTER the terminal glyph pipeline so
    /// overlay glyphs always paint on top of terminal content (fix for
    /// PR #45 review: overlays were being undercut by terminal text).
    text_renderer_overlay: TextRenderer,
    tab_buffer: Buffer,
    quad: QuadPipeline,
    /// Second QuadPipeline for overlay backgrounds / accents drawn AFTER
    /// terminal text. Same rationale as `text_renderer_overlay`: a single
    /// pipeline can't be `draw()`ed twice in one pass without clobbering
    /// its own instance buffer.
    quad_overlay: QuadPipeline,

    // B3 GPU text path for the terminal grid.
    glyph_atlas: GlyphAtlas,
    glyph_upload: AtlasUpload,
    text_pipeline: TextPipeline,

    font_family: String,
    font_size: f32,
    line_height: f32,
    /// DPI scale factor (e.g. 2.0 on Retina). Atlas tiles are rasterized at
    /// `font_size * scale_factor` (physical pixels) so the GPU has crisp
    /// source pixels; cell metrics (`cell_w`/`cell_h`) stay in *logical*
    /// pixels so grid layout doesn't reflow when the user drags the window
    /// between displays of different DPIs.
    scale_factor: f32,
    /// Logical cell width in pixels (one terminal column).
    pub cell_w: f32,
    /// Logical cell height in pixels (one terminal row).
    pub cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    padding_top: f32,
    padding_bottom: f32,
    bg: wgpu::Color,
    bg_opacity: f32,
    fg_default: GColor,
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
    /// Whether the OS window currently holds keyboard focus. Drives
    /// the wezterm-style "hollow" block cursor when the window is in
    /// the background. Defaults to `true` so a freshly created
    /// renderer draws the filled cursor on the very first frame,
    /// before winit has a chance to deliver `Focused(true)`.
    window_focused: bool,
    /// Cursor positions inside inactive panes (panes that share the
    /// window with the active pane but don't currently own keyboard
    /// focus). Drawn as hollow rectangles so the user can see where
    /// the cursor sits in every split simultaneously. Set by the app
    /// on every redraw via [`Self::set_inactive_pane_cursors`].
    inactive_pane_cursors: Vec<InactivePaneCursor>,
    selection_color: [f32; 4],
    tab_bar_bg: [f32; 4],
    tab_active_bg: [f32; 4],
    tab_inactive_bg: [f32; 4],
    tab_active_fg: GColor,
    tab_inactive_fg: GColor,
    tab_close_fg: [f32; 4],
    /// Optional user override for the close-button color. When `Some`,
    /// the × is always drawn at this color (matching WezTerm's
    /// `tab_close_button_color`). When `None`, the close button follows
    /// WezTerm fancy-mode parity: hidden until the cursor is over the
    /// tab, dim by default, brightened to `tab_active_fg` when the
    /// cursor is over the × glyph itself.
    tab_close_override: Option<[f32; 4]>,
    /// Last reported cursor position in LOGICAL pixels, or `None` when
    /// the cursor is outside the window. Drives tab-close hover state.
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
    search_fg: GColor,
    search_bg: [f32; 4],
    search_buffer: Buffer,
    quick_select_buffer: Buffer,
    palette_query_buffer: Buffer,
    palette_rows_buffer: Buffer,
    palette_footer_buffer: Buffer,
    cheatsheet_query_buffer: Buffer,
    cheatsheet_rows_buffer: Buffer,
    cheatsheet_footer_buffer: Buffer,
    ime_buffer: Buffer,
    /// Dedicated text buffer for broadcast-warning pane strips.
    broadcast_buffer: Buffer,
    /// Dedicated text buffer for the drag-chip title overlay.
    drag_chip_buffer: Buffer,
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
    /// titlebar. Non-zero on macOS when the window uses
    /// `with_fullsize_content_view(true)` — without this the tab bar
    /// would paint under the traffic lights + window title. See
    /// [`crate::app::integrated_titlebar_inset`].
    titlebar_inset: f32,
    /// Characters from the most recent `render()` call that the
    /// rasterizer could not produce a tile for (i.e. would draw as a
    /// tofu outline). Whitespace is excluded. Test-only diagnostic
    /// surfaced through [`Self::last_missing_tofu`]; production code
    /// must not depend on it.
    last_missing_chars: Vec<char>,
    /// Per-row shaped-glyph cache. Keyed by (text, style, family,
    /// px); a row whose content + style hasn't changed since the
    /// last frame hits the cache and skips cosmic-text entirely.
    shape_cache: ShapeCache,
    /// Per-row glyph cache (PR after #130). Stores the shaped
    /// `GlyphInstance`s, underline coalescing, and missing-tofu list
    /// for each visible row, keyed by absolute row index + a content
    /// hash. A row whose contents / style / selection-overlap haven't
    /// changed splices its cached output straight into the frame and
    /// skips the entire `flush_shape_run` walk.
    row_glyph_cache: crate::row_glyph_cache::RowGlyphCache,
    /// Per-pane origins recorded on the most recent `render()` call.
    /// `(pane_id, [origin_x_px, origin_y_px])` for every pane in the
    /// frame's pane slice. Test-only diagnostic surfaced through
    /// [`Self::last_emitted_origins`]; production code must not rely
    /// on it. Part B step 7 hook for the per-pane render integration
    /// test.
    last_emit_origins: Vec<(u64, [f32; 2])>,
    /// Monotonic counter bumped on theme / default-fg / default-bg
    /// changes. Folded into every `row_hash` so palette swaps
    /// invalidate cached colours without iterating the cache.
    style_rev: u64,
    /// Active drag-chip overlay: translucent rect drawn at the cursor
    /// while a tab is held. Cleared on release.
    drag_chip: Option<DragChipOverlay>,
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
    pane_revs: Vec<(u64, u64)>,
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
    /// Whether the window has keyboard focus — toggles the active
    /// cursor between filled and hollow.
    window_focused: bool,
    /// Number of inactive-pane cursors drawn this frame. Folded in so
    /// adding/removing a split refreshes the cache.
    inactive_cursor_count: u32,
    /// Index of the tab the cursor is currently over, or `u32::MAX`
    /// when the cursor is not over any tab. Drives the WezTerm-style
    /// "× only visible on tab hover" behaviour — moving the cursor
    /// between tabs must invalidate the cached frame.
    hover_tab: u32,
    /// `1` when the cursor is over the close-button rect of the hovered
    /// tab, `0` otherwise. Drives the dim → bright × transition.
    hover_close: u8,
    /// `1` when an always-on close-button color override is active.
    /// Folded in so toggling the config option live invalidates the
    /// frame cache.
    close_override: u8,
    broadcast_receivers_hash: u64,
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
        let RendererSettings { font_family, font_size, line_height_mult, padding, appearance } =
            settings;
        let [padding_left, padding_right, padding_top, padding_bottom] = padding;
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
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

        let mut font_system = FontSystem::new();
        // Load bundled fonts from assets/fonts/ next to the executable (or
        // workspace-root in dev) so the user gets Recursive Code without
        // having to install it system-wide.
        load_bundled_fonts(&mut font_system);
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let text_renderer_overlay =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let quad = QuadPipeline::new(&device, format);
        let quad_overlay = QuadPipeline::new(&device, format);

        // B3 GPU text path. Allocate the CPU + GPU side of the glyph
        // atlas up front so the first frame can stream tiles into it.
        // On HiDPI displays we bump the atlas so a 2× tile set fits
        // without thrashing the shelf-packer.
        let mut glyph_atlas =
            GlyphAtlas::new(atlas_dim_for_scale(scale_factor), atlas_dim_for_scale(scale_factor));
        let text_pipeline = TextPipeline::new(&device, format, 4096);
        // Pre-bake box-drawing + Powerline glyphs into the atlas before
        // the first frame so TUIs that draw a wall of │ ─ ┌ ┐ chars on
        // launch don't pay the font-fallback charmap-walk cost per cell
        // in the first paint. See `swash_rasterizer::prebake_box_and_powerline`.
        {
            let mut prebake_raster =
                SwashRasterizer::new(&mut font_system, font_family, font_size * scale_factor);
            let _inserted =
                swash_rasterizer::prebake_box_and_powerline(&mut prebake_raster, &mut glyph_atlas);
        }
        let glyph_upload =
            AtlasUpload::new(&device, &queue, &glyph_atlas, &text_pipeline.bind_group_layout);

        let natural_h = natural_line_h_px(&mut font_system, font_family, font_size);
        let line_height = natural_h * line_height_mult;
        let metrics = Metrics::new(font_size, line_height);
        // Grid no longer uses a glyphon Buffer — the atlas-backed
        // text_pipeline draws it directly. We still construct
        // `metrics` to share it with measure_cell below.
        let _ = metrics;

        // A second buffer is used for the tab-bar titles. Tab titles use a
        // tighter line height than the terminal grid; one buffer per bar
        // means we only re-shape titles when the tab set changes.
        //
        // Tab title size = body font size + 1pt so the bar reads slightly
        // heavier than the grid below it (design polish per PR
        // "tabbar: centered title with config font, larger size").
        let tab_font_size = tab_title_font_size(font_size);
        let tab_metrics = Metrics::new(tab_font_size, tab_font_size * 1.2);
        let mut tab_buffer = Buffer::new(&mut font_system, tab_metrics);
        let bar_h = tab_bar_height(font_size);
        tab_buffer.set_size(&mut font_system, Some(size.width as f32 / scale_factor), Some(bar_h));

        let (cell_w, cell_h) = measure_cell(&mut font_system, font_family, font_size, line_height);

        let bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), appearance.opacity);
        let bg_rgba = hex_to_rgba(theme.colors.background.0.as_str(), 1.0);
        let fg_default = hex_to_glyphon(theme.colors.foreground.0.as_str());
        let cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 1.0);
        let selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        let tab_bar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        let tab_active_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        let tab_inactive_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        let tab_active_fg = hex_to_glyphon(theme.colors.tab.active_fg.0.as_str());
        let tab_inactive_fg = hex_to_glyphon(theme.colors.tab.inactive_fg.0.as_str());
        let tab_close_fg = hex_to_rgba(theme.colors.tab.close_button_fg.0.as_str(), 1.0);
        let tab_separator = hex_to_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        // Hyperlink visuals: theme-aware. Use the theme's cursor color as the
        // accent (every bundled theme designates it). Underline reads as
        // deliberate at high opacity; the tint behind the run is subtle.
        let hyperlink_underline = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.9);
        let splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonic_core::theme::Appearance::Dark => 0.14,
            sonic_core::theme::Appearance::Light => 0.10,
        };
        let hyperlink_tint = hex_to_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        let search_highlight = hex_to_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        // Current (selected) match draws in orange so it's distinguishable
        // from the other yellow matches at a glance.
        let search_highlight_current = [1.0, 0.5, 0.0, 0.55];
        let search_fg = hex_to_glyphon(theme.colors.foreground.0.as_str());
        let search_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        let search_metrics = Metrics::new(font_size * 0.85, font_size * 0.85 * 1.2);
        let mut search_buffer = Buffer::new(&mut font_system, search_metrics);
        search_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(font_size * 0.85 * 1.2),
        );
        let mut quick_select_buffer = Buffer::new(&mut font_system, search_metrics);
        quick_select_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );

        // Overlay text buffers. Sized lazily inside render() since palette
        // and ime geometry depend on state. They start out at window
        // width so glyphon doesn't reject them before the first frame.
        let palette_metrics = Metrics::new(font_size, font_size * 1.25);
        let mut palette_query_buffer = Buffer::new(&mut font_system, palette_metrics);
        palette_query_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(font_size * 1.25),
        );
        // Rows buffer: line height MUST equal the full row stride
        // (PALETTE_ROW_HEIGHT + PALETTE_ROW_GAP), not just the row
        // background height. Otherwise the Nth label drifts upward by
        // N * PALETTE_ROW_GAP px relative to its background rect — at
        // row 6 the text sits a full row above the highlight, producing
        // the "selection highlights an empty slot" bug from live testing.
        let palette_rows_metrics = Metrics::new(
            font_size,
            crate::overlays::PALETTE_ROW_HEIGHT + crate::overlays::PALETTE_ROW_GAP,
        );
        let mut palette_rows_buffer = Buffer::new(&mut font_system, palette_rows_metrics);
        palette_rows_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );
        // Dedicated footer buffer so the hint sits in `layout.footer`
        // instead of being appended to the rows list (which made it
        // appear near the top of the visible window).
        let palette_footer_metrics =
            Metrics::new(font_size * 0.85, crate::overlays::PALETTE_FOOTER_HEIGHT);
        let mut palette_footer_buffer = Buffer::new(&mut font_system, palette_footer_metrics);
        palette_footer_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(crate::overlays::PALETTE_FOOTER_HEIGHT),
        );
        let mut cheatsheet_query_buffer = Buffer::new(&mut font_system, palette_metrics);
        cheatsheet_query_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(font_size * 1.25),
        );
        let mut cheatsheet_rows_buffer = Buffer::new(&mut font_system, palette_rows_metrics);
        cheatsheet_rows_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );
        let mut cheatsheet_footer_buffer = Buffer::new(&mut font_system, palette_footer_metrics);
        cheatsheet_footer_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(crate::overlays::PALETTE_FOOTER_HEIGHT),
        );
        let ime_metrics = Metrics::new(font_size, font_size * 1.25);
        let mut ime_buffer = Buffer::new(&mut font_system, ime_metrics);
        ime_buffer.set_size(&mut font_system, Some(size.width as f32), Some(font_size * 1.5));
        let broadcast_metrics = Metrics::new(font_size * 0.85, font_size * 0.85 * 1.2);
        let mut broadcast_buffer = Buffer::new(&mut font_system, broadcast_metrics);
        broadcast_buffer.set_size(&mut font_system, Some(size.width as f32), Some(font_size * 1.5));
        let drag_chip_metrics = Metrics::new(font_size * 0.85, font_size * 0.85 * 1.2);
        let mut drag_chip_buffer = Buffer::new(&mut font_system, drag_chip_metrics);
        drag_chip_buffer.set_size(&mut font_system, Some(220.0), Some(font_size * 1.5));

        Ok(Self {
            instance,
            device,
            queue,
            surface,
            config,
            window,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            text_renderer_overlay,
            tab_buffer,
            quad,
            quad_overlay,
            glyph_atlas,
            glyph_upload,
            text_pipeline,
            font_family: font_family.to_string(),
            font_size,
            line_height,
            scale_factor,
            cell_w,
            cell_h,
            padding_left,
            padding_right,
            padding_top,
            padding_bottom,
            bg,
            bg_opacity: appearance.opacity.clamp(0.0, 1.0),
            fg_default,
            cursor_color,
            bg_rgba,
            cursor_shape: CursorShape::default(),
            cursor_blink: true,
            blink_epoch: Instant::now(),
            window_focused: true,
            inactive_pane_cursors: Vec::new(),
            selection_color,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            tab_active_fg,
            tab_inactive_fg,
            tab_close_fg,
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
            search_buffer,
            quick_select_buffer,
            palette_query_buffer,
            palette_rows_buffer,
            palette_footer_buffer,
            cheatsheet_query_buffer,
            cheatsheet_rows_buffer,
            cheatsheet_footer_buffer,
            ime_buffer,
            broadcast_buffer,
            drag_chip_buffer,
            drag_chip_visual: None,
            last_frame_key: None,
            skipped_frames: 0,
            tab_bar_visible: true,
            titlebar_inset: 0.0,
            last_missing_chars: Vec::new(),
            shape_cache: ShapeCache::new(),
            row_glyph_cache: crate::row_glyph_cache::RowGlyphCache::new(),
            last_emit_origins: Vec::new(),
            style_rev: 0,
            drag_chip: None,
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
        // Text buffers are laid out in LOGICAL pixels (their font_size
        // is logical); pass logical widths so wrapping/clipping doesn't
        // give them 2× the room on Retina.
        let logical_w = self.config.width as f32 / self.scale_factor;
        let logical_h = self.config.height as f32 / self.scale_factor;
        let bar_h = self.tab_bar_logical_height();
        self.tab_buffer.set_size(&mut self.font_system, Some(logical_w), Some(bar_h));
        self.search_buffer.set_size(
            &mut self.font_system,
            Some(logical_w),
            Some(self.font_size * 0.85 * 1.2),
        );
        self.quick_select_buffer.set_size(&mut self.font_system, Some(logical_w), Some(logical_h));
        self.palette_query_buffer.set_size(
            &mut self.font_system,
            Some(logical_w),
            Some(self.font_size * 1.25),
        );
        self.palette_rows_buffer.set_size(&mut self.font_system, Some(logical_w), Some(logical_h));
        self.palette_footer_buffer.set_size(
            &mut self.font_system,
            Some(logical_w),
            Some(crate::overlays::PALETTE_FOOTER_HEIGHT),
        );
        self.ime_buffer.set_size(
            &mut self.font_system,
            Some(logical_w),
            Some(self.font_size * 1.5),
        );
    }

    /// Top inset reserved above the grid: OS titlebar band (when active)
    /// plus the tab bar strip (when shown via [`Self::set_tab_bar_visible`]).
    pub fn top_inset(&self) -> f32 {
        let bar = if self.tab_bar_visible {
            self.tab_bar_logical_height() + self.padding_top
        } else {
            self.padding_top
        };
        self.titlebar_inset + bar
    }

    /// Logical-pixel height of the tab bar for the renderer's current font
    /// size. Derived from [`tab_bar_height`] so the bar tracks
    /// `window_frame.font_size × 2` like WezTerm fancy-mode.
    pub fn tab_bar_logical_height(&self) -> f32 {
        tab_bar_height(self.font_size)
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
        cursor::redraw_interval()
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

    /// Update the cached "is the OS window focused" flag. Drives the
    /// hollow-block cursor when `false`. Bumps the FrameKey via
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

    /// Publish the per-frame list of inactive-pane cursors. Each entry
    /// is `(row, col, pane_rect_in_px)`. The renderer draws a hollow
    /// rectangle at the cell so the user can locate the cursor in
    /// every split simultaneously.
    pub fn set_inactive_pane_cursors(&mut self, cursors: Vec<InactivePaneCursor>) {
        if self.inactive_pane_cursors != cursors {
            self.inactive_pane_cursors = cursors;
            self.last_frame_key = None;
        }
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
    /// Per-pane origins recorded by the most recent `render()` call, as
    /// `(pane_id, [origin_x_px, origin_y_px])`. Test-only hook for the
    /// Part B step 7 per-pane render integration test. Production code
    /// must not depend on this.
    #[doc(hidden)]
    pub fn last_emitted_origins(&self) -> Vec<(u64, [f32; 2])> {
        self.last_emit_origins.clone()
    }

    /// Test-only snapshot of the renderer's text cache sizes. Used to
    /// assert that a font-family live apply re-derives metrics and drops
    /// shaped rows from the old face instead of reusing stale advances.
    #[doc(hidden)]
    pub fn text_cache_sizes_for_test(&self) -> (usize, usize) {
        (self.shape_cache.len(), self.row_glyph_cache.len())
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
    ) -> Option<(f32, f32)> {
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
                let x = origin_x + col_a as f32 * cell_w;
                let y = origin_y + f32::from(visible_row) * cell_h;
                let w = (col_b - col_a + 1) as f32 * cell_w;
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, cell_h, sw, sh),
                    color: selection_color,
                    ..Default::default()
                });
            }
        }

        let visible_row = Self::viewport_relative_row(copy_mode.cursor.1, view_top_abs, grid.rows)?;
        let copy_col = copy_mode.cursor.0.min(grid.cols.saturating_sub(1) as usize);
        let cx = origin_x + copy_col as f32 * cell_w;
        let cy = origin_y + f32::from(visible_row) * cell_h;
        quads.push(QuadInstance {
            rect: px_to_ndc(cx, cy, cell_w, cell_h, sw, sh),
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
    /// reload path so editing `sonic.toml` takes effect without restart).
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

    /// Logical (DPI-independent) size of the render surface in CSS pixels.
    ///
    /// The pane layout, padding, top inset and cell metrics are all expressed
    /// in logical units; mixing in physical `width()`/`height()` (which are
    /// scaled by `scale_factor`) produced over-sized pane borders at 2×
    /// displays. Call this when computing the outer rect for `PaneTree::layout`.
    pub fn logical_size(&self) -> (f32, f32) {
        (
            self.config.width as f32 / self.scale_factor,
            self.config.height as f32 / self.scale_factor,
        )
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

    /// Current grid dimensions in `(cols, rows)`. Computed from the
    /// LOGICAL surface size divided by the LOGICAL cell pitch so the
    /// result matches what the user actually sees on Retina.
    pub fn cells(&self) -> (u16, u16) {
        // Convert physical surface dims back to LOGICAL before dividing
        // by logical cell metrics; otherwise a 2× display would report
        // 2× the columns/rows the user actually sees (and the renderer
        // would happily address rows past the visible viewport).
        let logical_w = self.config.width as f32 / self.scale_factor;
        let logical_h = self.config.height as f32 / self.scale_factor;
        let inner_w = (logical_w - self.padding_left - self.padding_right).max(self.cell_w);
        let inner_h = (logical_h - self.top_inset() - self.padding_bottom).max(self.cell_h);
        let cols = (inner_w / self.cell_w).floor() as u16;
        let rows = (inner_h / self.cell_h).floor() as u16;
        (cols.max(1), rows.max(1))
    }

    /// Logical cell metrics (width, height) in CSS pixels. Pair with a
    /// `sonic_ui::pane::Rect` from `PaneTree::layout` to compute how many
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
        let inset = self.titlebar_inset;
        let bar_h = self.tab_bar_logical_height();
        let in_bar = |p: Option<(f32, f32)>| -> bool {
            match p {
                Some((_, y)) => y >= inset && y <= inset + bar_h,
                None => false,
            }
        };
        in_bar(prev) || in_bar(next)
    }

    /// Optional override for the close-button color. When `Some`, the ×
    /// is drawn in this color and is always visible (matching WezTerm's
    /// `tab_close_button_color` config). Accepts a `#rrggbb` string;
    /// invalid strings are ignored.
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
        let new_line_h = natural_line_h_px(&mut self.font_system, family, size) * line_height_mult;
        let no_change = self.font_family == family
            && (self.font_size - size).abs() < f32::EPSILON
            && (self.line_height - new_line_h).abs() < f32::EPSILON;
        if no_change {
            return;
        }
        self.font_family = family.to_string();
        self.font_size = size;
        self.line_height = new_line_h;
        let (cw, ch) = measure_cell(&mut self.font_system, family, size, self.line_height);
        self.cell_w = cw;
        self.cell_h = ch;
        let w = self.glyph_atlas.width();
        let h = self.glyph_atlas.height();
        self.glyph_atlas = GlyphAtlas::new(w, h);
        self.shape_cache = ShapeCache::new();
        {
            let mut prebake_raster = SwashRasterizer::new(
                &mut self.font_system,
                &self.font_family,
                self.font_size * self.scale_factor,
            );
            let _inserted = swash_rasterizer::prebake_box_and_powerline(
                &mut prebake_raster,
                &mut self.glyph_atlas,
            );
        }
        self.row_glyph_cache.invalidate_all();
        self.last_frame_key = None;
        tracing::info!(
            "renderer.set_font: family={family} size={size} line_h={} cell={cw:.2}x{ch:.2}",
            self.line_height
        );
    }

    /// Current DPI scale factor in effect (1.0 on standard displays, 2.0
    /// on Retina, etc.).
    #[doc(hidden)]
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Apply a new DPI scale factor without reconstructing the renderer.
    ///
    /// The atlas is cleared (and possibly re-sized) because existing tiles
    /// were rasterized at the old physical-px em-size — sampling them at the
    /// new scale would produce the same blurry result we're fixing. The
    /// frame-key cache is invalidated so the next `render()` re-rasterizes.
    ///
    /// Cell metrics are intentionally NOT recomputed: they stay in logical
    /// pixels so columns/rows in a fixed-size window are stable when the
    /// user drags between displays of different DPIs.
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        let sf = scale_factor.max(0.1);
        if (self.scale_factor - sf).abs() < f32::EPSILON {
            return;
        }
        self.rebuild_for_scale(sf);
    }

    /// Force-rebuild atlas + GPU upload for the given scale factor,
    /// regardless of whether the cached value matches. Used by the
    /// tear-out path where `GpuRenderer::new` may have latched the
    /// wrong scale (window not yet placed on a display, so
    /// `window.scale_factor()` reports 1.0); once the OS places the
    /// new window on its real Retina display, we must re-rasterize
    /// glyphs at the correct physical em-size or the child window
    /// shows blurry tiles + atlas tofu instead of real text. See the
    /// bug report on torn-out windows rendering with wrong cell width
    /// and missing nerd-font glyphs.
    pub fn force_rebuild_for_scale(&mut self, scale_factor: f32) {
        let sf = scale_factor.max(0.1);
        self.rebuild_for_scale(sf);
    }

    fn rebuild_for_scale(&mut self, sf: f32) {
        self.scale_factor = sf;
        let dim = atlas_dim_for_scale(sf);
        self.glyph_atlas = GlyphAtlas::new(dim, dim);
        {
            let mut prebake_raster = SwashRasterizer::new(
                &mut self.font_system,
                &self.font_family,
                self.font_size * self.scale_factor,
            );
            let _inserted = swash_rasterizer::prebake_box_and_powerline(
                &mut prebake_raster,
                &mut self.glyph_atlas,
            );
        }
        self.row_glyph_cache.invalidate_all();
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
            &self.text_pipeline.bind_group_layout,
        );
        self.last_frame_key = None;
        if let Some(w) = Some(&self.window) {
            w.request_redraw();
        }
        tracing::info!(
            "renderer.set_scale_factor: scale={sf} atlas={dim}x{dim} raster_px={}",
            self.font_size * sf
        );
    }

    /// Apply a new color theme without reconstructing the renderer.
    /// Recomputes every cached wgpu / glyphon color derived from the
    /// theme so the next frame reflects the swap.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.set_theme_with_opacity(theme, self.bg_opacity);
    }

    /// Apply a new color theme and terminal background opacity.
    pub fn set_theme_with_opacity(&mut self, theme: &Theme, opacity: f32) {
        self.bg_opacity = opacity.clamp(0.0, 1.0);
        self.bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), self.bg_opacity);
        self.fg_default = hex_to_glyphon(theme.colors.foreground.0.as_str());
        self.cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 1.0);
        self.bg_rgba = hex_to_rgba(theme.colors.background.0.as_str(), 1.0);
        self.selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        self.tab_bar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        self.tab_active_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        self.tab_inactive_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        self.tab_active_fg = hex_to_glyphon(theme.colors.tab.active_fg.0.as_str());
        self.tab_inactive_fg = hex_to_glyphon(theme.colors.tab.inactive_fg.0.as_str());
        self.tab_close_fg = hex_to_rgba(theme.colors.tab.close_button_fg.0.as_str(), 1.0);
        self.tab_separator = hex_to_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        self.hyperlink_underline = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.9);
        self.splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonic_core::theme::Appearance::Dark => 0.14,
            sonic_core::theme::Appearance::Light => 0.10,
        };
        self.hyperlink_tint = hex_to_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        self.search_highlight = hex_to_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        self.search_fg = hex_to_glyphon(theme.colors.foreground.0.as_str());
        self.search_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        self.last_frame_key = None;
        self.style_rev = self.style_rev.wrapping_add(1);
        self.row_glyph_cache.invalidate_all();
        tracing::info!("renderer.set_theme: {}", theme.name);
    }

    /// Translate physical-pixel `(px, py)` (as winit reports) into a
    /// `(col, row)` cell address inside the grid, or `None` if the point
    /// falls outside the grid (in the tab bar, padding, etc.).
    pub fn pixel_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        // Winit reports cursor positions in PHYSICAL pixels; our cell
        // grid is in LOGICAL pixels. Normalize at the boundary so click
        // targeting lands on the cell the user actually sees on Retina.
        let px = px / self.scale_factor;
        let py = py / self.scale_factor;
        let x = px - self.padding_left;
        let y = py - self.top_inset();
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let col = (x / self.cell_w).floor() as i32;
        let row = (y / self.cell_h).floor() as i32;
        if col < 0 || row < 0 {
            return None;
        }
        Some((row.min(u16::MAX as i32) as u16, col.min(u16::MAX as i32) as u16))
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
        panes: &mut [sonic_render_model::PaneRender<'_>],
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
        self.last_emit_origins =
            panes.iter().map(|p| (p.id, [p.rect_px.x as f32, p.rect_px.y as f32])).collect();
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
        }
        let pane_views: Vec<PaneView<'_>> = panes
            .iter()
            .map(|p| PaneView {
                grid: &*p.grid,
                pane_id: p.id,
                origin_x: p.rect_px.x as f32,
                origin_y: p.rect_px.y as f32,
                rect_w: p.rect_px.w as f32,
                rect_h: p.rect_px.h as f32,
                is_active: p.is_active,
            })
            .collect();
        // Pre-compute pane revisions for FrameKey from the safe borrows.
        let pane_revs_vec: Vec<(u64, u64)> =
            pane_views.iter().map(|pv| (pv.pane_id, pv.grid.revision())).collect();
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
        let blink_alpha = cursor::blink_alpha(blink_elapsed, self.cursor_blink);
        // `phase_bucket` is intentionally NOT folded into the FrameKey
        // (see the `cursor_phase: 0` comment below). The alpha is
        // still computed every render so a real redraw event picks up
        // the current blink pulse.
        let _ = cursor::phase_bucket(blink_elapsed, self.cursor_blink);
        // Compute hover state against the tab bar layout. Done before
        // the FrameKey is built so the cache invalidates as the cursor
        // moves between tabs / on and off the × glyph.
        let (hover_tab_idx, hover_close_hit) = {
            let mut idx: u32 = u32::MAX;
            let mut on_close: u8 = 0;
            if self.tab_bar_visible {
                if let Some((cx, cy)) = self.hover_cursor {
                    let sw_log = self.config.width as f32 / self.scale_factor;
                    let layout = TabBarLayout::compute_with_height(
                        tabs,
                        sw_log,
                        self.tab_bar_logical_height(),
                    )
                    .with_top_offset(self.titlebar_inset);
                    for t in layout.tabwidgets() {
                        match t.hover_at(Some(sonic_ui::tabbar_view::Point { x: cx, y: cy })) {
                            sonic_ui::tabbar_view::TabHover::None => {}
                            sonic_ui::tabbar_view::TabHover::Body => {
                                idx = t.idx as u32;
                                break;
                            }
                            sonic_ui::tabbar_view::TabHover::Close => {
                                idx = t.idx as u32;
                                on_close = 1;
                                break;
                            }
                        }
                    }
                }
            }
            (idx, on_close)
        };
        let quick_select_hint_count = copy_mode
            .and_then(|state| state.quick_select.as_ref())
            .map_or(0, |quick| quick.hints.len() as u32);
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
            inactive_cursor_count: self.inactive_pane_cursors.len() as u32,
            hover_tab: hover_tab_idx,
            hover_close: hover_close_hit,
            close_override: u8::from(self.tab_close_override.is_some()),
            broadcast_receivers_hash,
        };
        if Some(&key) == self.last_frame_key.as_ref() {
            self.skipped_frames = self.skipped_frames.wrapping_add(1);
            tracing::trace!(skipped = self.skipped_frames, "renderer: skipped unchanged frame");
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
        // buffer, no glyphon shape pass for the terminal grid.
        let fg_default = self.fg_default;
        // Underline runs collected per pane. We record (origin_x, origin_y, row,
        // col_a, col_b) where origin_{x,y} is the PANE's origin (pad / top_inset)
        // captured at insert time. Pre-fix this was (row, col_a, col_b) and the
        // emit loop used `active_origin_x/y` for every entry — that placed
        // inactive-pane underlines under the active pane's coordinates (Haiku
        // round-3 finding on PR #199).
        let mut underlines: Vec<(f32, f32, u16, u16, u16)> = Vec::new();
        let mut glyph_instances: Vec<GlyphInstance> =
            Vec::with_capacity(grid.cols as usize * grid.rows as usize);
        // Missing-glyph "tofu" outlines collected during the cell walk.
        // Drawn via the quad pipeline after the text instances.
        let mut missing_tofu: Vec<(f32, f32, f32, f32, glyphon::Color)> = Vec::new();
        // Mirror of missing_tofu, recording just the codepoint so tests
        // can assert "no class regressed" without depending on pixel
        // layout. Cleared every frame; published into `self.last_missing_chars`
        // before render() returns.
        let mut missing_chars_this_frame: Vec<char> = Vec::new();
        // `config.width/height` are PHYSICAL pixels (winit 0.30
        // `WindowEvent::Resized` reports PhysicalSize, which we forward
        // straight into wgpu surface configure). All layout math in
        // this function — cell_w/cell_h, padding, top_inset, font_size
        // — is in LOGICAL pixels. NDC is a unit-agnostic ratio, so we
        // MUST hand `px_to_ndc` a surface size that's in the *same*
        // unit as the rect we're converting. Pre-PR #63 the renderer
        // was monolithically logical and got away with it; #63 made
        // the atlas physical-correct, but left this mismatch which
        // halves every rect on a 2× display — the grid renders in a
        // tiny corner with sub-pixel glyphs. Regression target:
        // `sonic-shared/tests/hidpi2.rs::glyph_rect_scales_with_dpi`.
        let sw = self.config.width as f32 / self.scale_factor;
        let sh = self.config.height as f32 / self.scale_factor;
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

        {
            let mut rasterizer = SwashRasterizer::new(
                &mut self.font_system,
                &self.font_family,
                self.font_size * self.scale_factor,
            );
            // Part B step 3: iterate every pane. Each iteration rebinds
            // `grid` to that pane's Grid (via the raw pointer collected
            // into pane_views above), uses the pane's own origin instead
            // of the window-level padding/inset, and threads its own
            // pane_id into the row_glyph_cache so split panes don't
            // collide on absolute-row keys (PR #208 prereq).
            for pv in &pane_views {
                let grid: &Grid = pv.grid;
                let pane_id: crate::row_glyph_cache::PaneId = pv.pane_id;
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
                let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
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
                for r in 0..grid.rows {
                    let row_abs = view_top_abs + r as u64;
                    let Some(row) = grid.row_at_abs(row_abs) else {
                        continue;
                    };
                    // ------ Cache lookup ------
                    let key = crate::row_glyph_cache::row_hash(
                        view_top_abs,
                        r as usize,
                        row,
                        self.style_rev,
                        cell_w,
                        cell_h,
                        self.scale_factor,
                        sel_bbox,
                    );
                    if let Some(cached) = self.row_glyph_cache.get(pane_id, row_abs, key) {
                        glyph_instances.extend_from_slice(&cached.glyphs);
                        for (s, e) in &cached.underlines {
                            underlines.push((pad, top_inset, r, *s, *e));
                        }
                        for t in &cached.tofu {
                            missing_tofu.push(*t);
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
                    let mut row_underlines: Vec<(u16, u16)> = Vec::new();
                    let mut ul_start: Option<u16> = None;
                    let mut last_visible_col: u16 = 0;
                    // First pass: per-cell underline coalescing (unchanged
                    // — underlines are a cell-level decoration, independent
                    // of shaping).
                    for (col, cell) in row.iter().enumerate() {
                        if cell.flags.contains(CellFlags::WIDE_CONT) {
                            continue;
                        }
                        last_visible_col = col as u16;
                        if cell.flags.contains(CellFlags::UNDERLINE) {
                            if ul_start.is_none() {
                                ul_start = Some(col as u16);
                            }
                        } else if let Some(s) = ul_start.take() {
                            let end = (col as u16).saturating_sub(1);
                            row_underlines.push((s, end));
                            underlines.push((pad, top_inset, r, s, end));
                        }
                    }
                    if let Some(s) = ul_start.take() {
                        row_underlines.push((s, last_visible_col));
                        underlines.push((pad, top_inset, r, s, last_visible_col));
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
                                    self.font_size * self.scale_factor,
                                    self.scale_factor,
                                    &mut rasterizer,
                                    &mut self.shape_cache,
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
                            self.font_size * self.scale_factor,
                            self.scale_factor,
                            &mut rasterizer,
                            &mut self.shape_cache,
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
                        );
                    }
                    // Capture this row's contributions and insert into
                    // the cache so subsequent unchanged frames replay
                    // without shaping.
                    let row_glyphs = glyph_instances[glyph_base..].to_vec();
                    let row_tofu = missing_tofu[tofu_base..].to_vec();
                    let row_missing = missing_chars_this_frame[miss_base..].to_vec();
                    self.row_glyph_cache.insert(
                        pane_id,
                        row_abs,
                        key,
                        crate::row_glyph_cache::CachedRow {
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

        // Per-cell ANSI background colors. Must be pushed FIRST so that
        // selection / cursor / overlay quads draw on top — otherwise an
        // ANSI-colored cell would obscure the selection highlight. The
        // helper run-length coalesces adjacent same-bg cells into a single
        // wide quad (an 80-col `\033[41m` fill becomes 1 quad, not 80).
        // Cells whose bg resolves to the theme default are skipped: the
        // surface `LoadOp::Clear(self.bg)` already covers that area.
        // Part B step 3: emit bg quads for EVERY pane using each pane's
        // own origin, not just the active pane.
        for pv in &pane_views {
            let pv_grid: &Grid = pv.grid;
            let pane_rect = pane_rects
                .iter()
                .find(|(id, _)| *id == pv.pane_id)
                .map(|(_, rect)| *rect)
                .unwrap_or(PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h });
            emit_cell_bg_quads_clipped(
                pv_grid,
                Self::resolved_view_top_abs(pv_grid, viewport_top_abs),
                theme,
                pane_rect,
                cell_w,
                cell_h,
                sw,
                sh,
                &mut quads,
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
        if cursor_visible {
            // Hide the cursor when the viewport is scrolled away from the
            // live region — its absolute row is `scrollback_len + cursor.row`,
            // which sits below the bottom of a scrolled-back view.
            let live_top = grid.scrollback_len() as u64;
            let view_top = viewport_top_abs.map(|v| v.min(live_top)).unwrap_or(live_top);
            if view_top == live_top {
                let cx = active_origin_x + f32::from(grid.cursor.col) * self.cell_w;
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
                        if self.window_focused {
                            if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                                (cx, cy, self.cell_w, self.cell_h),
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
                                self.cell_w,
                                self.cell_h,
                                sw,
                                sh,
                                bg,
                            );
                        } else {
                            // Unfocused window: draw a hollow block
                            // (2px border, transparent fill) so the
                            // user can still see the cursor without
                            // losing the text under it. Matches
                            // wezterm/iTerm2 behaviour. The glyph
                            // remains in the original fg color since
                            // the cell is not inverted.
                            push_hollow_rect_clipped(
                                &mut quads,
                                cx,
                                cy,
                                self.cell_w,
                                self.cell_h,
                                sw,
                                sh,
                                color,
                                2.0,
                                active_pane_x,
                                active_pane_y,
                                active_pane_w,
                                active_pane_h,
                            );
                        }
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
                            (cx, cy + self.cell_h - SUBSHAPE_PX, self.cell_w, SUBSHAPE_PX),
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

        // Hollow cursor for every inactive pane. Drawn outside the
        // active-cursor guard so they appear even when ?25l hides the
        // active cursor in this pane — the inactive panes' cursors
        // belong to other shells and shouldn't share that toggle.
        if !self.inactive_pane_cursors.is_empty() {
            let mut hollow_color = self.cursor_color;
            // Dim so the active pane's cursor still reads as the focus
            // marker. 0.6 matches the wezterm inactive-pane treatment.
            hollow_color[3] *= 0.6;
            for ic in &self.inactive_pane_cursors {
                // Cell origin inside the pane's own rect. Pane rects
                // are already padded by the layout (they line up with
                // pane.rs::Rect) so we anchor cells at the rect's
                // top-left without re-applying the global padding.
                let icx = ic.rect.x + f32::from(ic.col) * self.cell_w;
                let icy = ic.rect.y + f32::from(ic.row) * self.cell_h;
                // Clip to the pane rect so a stale cursor position from a
                // pre-resize grid never bleeds onto a sibling. Routed
                // through the shared clip helper (PR #270 follow-up) — a
                // partially out-of-bounds cell still draws the visible
                // portion instead of the previous all-or-nothing skip.
                push_hollow_rect_clipped(
                    &mut quads,
                    icx,
                    icy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    hollow_color,
                    2.0,
                    ic.rect.x,
                    ic.rect.y,
                    ic.rect.w,
                    ic.rect.h,
                );
            }
        }

        // OSC 133 shell-integration: draw a small left-edge marker on every
        // row whose absolute position matches a recorded prompt-start. The
        // marker is rendered inside the left padding so it never overlaps
        // text. Color matches the cursor accent at half alpha — distinctive
        // but not noisy.
        let marker_w = (self.padding_left * 0.35).max(2.0).min(self.cell_w * 0.25);
        let marker_h = self.cell_h * 0.6;
        let mut marker_color = self.cursor_color;
        marker_color[3] = (marker_color[3] * 0.55).clamp(0.0, 1.0);
        let prompt_rows: Vec<u16> = {
            let live_top = grid.scrollback_len() as u64;
            let view_top = viewport_top_abs.map(|v| v.min(live_top)).unwrap_or(live_top);
            grid.prompts()
                .filter_map(|p| {
                    let rel = p.start_row.checked_sub(view_top)?;
                    if rel < grid.rows as u64 {
                        Some(rel as u16)
                    } else {
                        None
                    }
                })
                .collect()
        };
        for row in prompt_rows {
            let mx = (active_origin_x - marker_w - 1.0).max(0.0);
            let my =
                active_origin_y + f32::from(row) * self.cell_h + (self.cell_h - marker_h) * 0.5;
            quads.push(QuadInstance {
                rect: px_to_ndc(mx, my, marker_w, marker_h, sw, sh),
                color: marker_color,
                ..Default::default()
            });
        }

        // Hyperlink visuals: a translucent tint quad under the run plus an
        // underline quad on top. Coalesce contiguous hyperlinked cells per
        // row, mirroring the UNDERLINE pass below.
        let hl_runs = collect_hyperlink_runs(grid);
        let hl_thickness = (self.cell_h * 0.08).max(1.0);
        for (row, col_a, col_b) in &hl_runs {
            let x = active_origin_x + f32::from(*col_a) * self.cell_w;
            let y = active_origin_y + f32::from(*row) * self.cell_h;
            let w = f32::from(*col_b - *col_a + 1) * self.cell_w;
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
        // Color: foreground default at full alpha, linearized so the sRGB
        // surface format doesn't double-encode (matches the body glyph path).
        let underline_color = glyphon_color_to_linear_rgba(self.fg_default);
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        for (origin_x, origin_y, row, col_a, col_b) in &underlines {
            let x = *origin_x + f32::from(*col_a) * self.cell_w;
            let y = *origin_y + f32::from(*row) * self.cell_h + self.cell_h - underline_thickness;
            let w = f32::from(*col_b - *col_a + 1) * self.cell_w;
            quads.push(QuadInstance {
                rect: px_to_ndc(x, y, w, underline_thickness, sw, sh),
                color: underline_color,
                ..Default::default()
            });
        }

        // -------- Missing-glyph tofu fallback ------------------------------
        // For cells whose rasterizer returned no tile (and char isn't
        // whitespace), draw a thin outlined rectangle so the gap is
        // visible. Helps catch font-fallback misses (emoji etc.).
        for (x, y, w, h, col) in &missing_tofu {
            let mut rgba = glyphon_color_to_linear_rgba(*col);
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
            .with_top_offset(self.titlebar_inset);
            // Issue #112 Round 3 — premium browser-style chrome.
            // The structural colors come from `ui_tokens`, decoupled from
            // the terminal palette so every theme renders the same modern
            // tab bar. The theme.tab.* colors remain authoritative for
            // the title text (active vs inactive fg) so per-theme accents
            // still read through.
            use crate::ui_tokens::color as tok;
            let bar_bg = tok::BG_BASE();
            let active_bg = tok::BG_ELEVATED();
            let hover_bg = tok::BG_HOVER();
            let ui_palette = crate::ui_tokens::UiPalette::from_theme(theme);
            // Theme-driven accent (was hardcoded ACCENT_BLUE — broke gruvbox/etc.).
            let accent_blue = ui_palette.accent;
            let separator = tok::BORDER_SUBTLE();
            let border_subtle = tok::BORDER_SUBTLE();
            let muted = tok::TEXT_MUTED();
            let secondary = tok::TEXT_SECONDARY();
            let primary = tok::TEXT_PRIMARY();
            let danger = tok::DANGER();
            // Bar background
            quads.push(QuadInstance {
                rect: px_to_ndc(layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h, sw, sh),
                color: bar_bg,
                ..Default::default()
            });
            // 1px bottom border across the whole bar.
            quads.push(QuadInstance {
                rect: px_to_ndc(
                    layout.bar.x,
                    layout.bar.y + layout.bar.h - 1.0,
                    layout.bar.w,
                    1.0,
                    sw,
                    sh,
                ),
                color: border_subtle,
                ..Default::default()
            });
            // Win11-style caption buttons — only painted when the
            // integrated titlebar inset is non-zero (Windows). On macOS the
            // inset is 0 and this is a no-op there. The symbols are geometric
            // primitives in `paint_caption_buttons`, avoiding Unicode caption
            // glyphs that may be absent from the bundled font.
            if integrated_titlebar_inset_px() > 0 {
                let rects = crate::tabbar_view::caption_button_rects(sw as u32, 1.0);
                let tuples = [
                    (rects[0].x, rects[0].y, rects[0].w, rects[0].h),
                    (rects[1].x, rects[1].y, rects[1].w, rects[1].h),
                    (rects[2].x, rects[2].y, rects[2].w, rects[2].h),
                ];
                let caption_bg = ui_palette.bg_surface;
                let caption_fg = ui_palette.text_primary;
                crate::quad::paint_caption_buttons(
                    &mut quads,
                    &tuples,
                    (sw, sh),
                    caption_bg,
                    caption_fg,
                );
            }
            for t in &layout.tabs {
                let is_active = layout.active == Some(t.idx);
                let cursor_on_this_tab = hover_tab_idx == t.idx as u32;
                if is_active {
                    // Elevated pill background.
                    // TODO: switch to rounded quad after #116.
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg_rect.x, t.bg_rect.y, t.bg_rect.w, t.bg_rect.h, sw, sh),
                        color: active_bg,
                        ..Default::default()
                    });
                    // 2px top accent bar, ACCENT_BLUE, anchored to the
                    // active tab's own bg rect via `active_accent_rect()`.
                    // Issue #171: the previous inline `t.bg_rect.x + INSET`
                    // math was correct in isolation but easy to drift
                    // away from on future refactors; centralising the
                    // computation in the layout keeps the regression
                    // covered by the `crates/sonic-ui/tests/
                    // tabbar_active_indicator.rs` unit tests.
                    if let Some(acc) = layout.active_accent_rect() {
                        quads.push(QuadInstance {
                            rect: px_to_ndc(acc.x, acc.y, acc.w, acc.h, sw, sh),
                            color: accent_blue,
                            ..Default::default()
                        });
                    }
                } else if cursor_on_this_tab {
                    // Hover overlay on inactive tab — #FFFFFF/6%.
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg_rect.x, t.bg_rect.y, t.bg_rect.w, t.bg_rect.h, sw, sh),
                        color: hover_bg,
                        ..Default::default()
                    });
                }
                // 1px BORDER_SUBTLE separator between adjacent inactive
                // tabs (PR #109 dedup) — height bar_h - 16, centered.
                if t.idx + 1 < tabs.tabs().len() {
                    let next_is_active = layout.active == Some(t.idx + 1);
                    if !is_active && !next_is_active {
                        let sep_w = 1.0_f32;
                        let sep_h = (layout.bar.h - 16.0).max(1.0);
                        let sep_y = layout.bar.y + (layout.bar.h - sep_h) * 0.5;
                        let gap_mid = t.bg_rect.x + t.bg_rect.w + (TAB_GAP - sep_w) * 0.5;
                        quads.push(QuadInstance {
                            rect: px_to_ndc(gap_mid, sep_y, sep_w, sep_h, sw, sh),
                            color: separator,
                            ..Default::default()
                        });
                    }
                }
                // Close × — visible on hover of tab OR if user enabled
                // an explicit close-button color override (PR #109 sem.).
                let cursor_on_close = cursor_on_this_tab && hover_close_hit == 1;
                let draw_close = self.tab_close_override.is_some() || cursor_on_this_tab;
                if draw_close {
                    let close_color = if let Some(o) = self.tab_close_override {
                        o
                    } else if cursor_on_close {
                        // The spec calls for DANGER on click-down; we
                        // don't currently track button-down here, so
                        // hover brightens to TEXT_PRIMARY. (DANGER is
                        // still wired through the override path for
                        // theme authors who want it.)
                        let _ = danger;
                        primary
                    } else {
                        muted
                    };
                    let cx = t.close_x_rect.x;
                    let cy = t.close_x_rect.y;
                    // 14×14 hit, 8×8 glyph (inset 3px each side).
                    let inset = (t.close_x_rect.w - 8.0) * 0.5;
                    let glyph = (t.close_x_rect.w - inset * 2.0).max(1.0);
                    let thick = 1.5_f32;
                    // Diagonal × built from a stair-step of small squares
                    // along both diagonals (the wgpu quad pipeline has no
                    // rotation; PR #117 emitted a horizontal+vertical pair
                    // which read as a `+`, not a close icon — fixed here).
                    build_close_x_quads(
                        CloseXQuadParams {
                            x: cx + inset,
                            y: cy + inset,
                            glyph,
                            thick,
                            color: close_color,
                            sw,
                            sh,
                        },
                        &mut quads,
                    );
                }
                // Phase D D3 (Epic #289): if this tab is the source of
                // a live drag, overlay a translucent bar-bg quad to
                // dim it to roughly `source_alpha` perceived opacity.
                // The quad is painted AFTER the tab body + close icon
                // so it dims everything in the tab's footprint.
                if source_tab_idx == Some(t.idx) {
                    let dim = (1.0 - source_alpha.clamp(0.0, 1.0)).clamp(0.0, 1.0);
                    let mut overlay = bar_bg;
                    overlay[3] = dim;
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg_rect.x, t.bg_rect.y, t.bg_rect.w, t.bg_rect.h, sw, sh),
                        color: overlay,
                        ..Default::default()
                    });
                }
            }
            // `+` new-tab button — 28×28, radius 8 pill, hover BG.
            let nt = layout.new_tab;
            // Hover detection: cursor inside the new-tab rect.
            let nt_hover = self
                .hover_cursor
                .map(|(cx, cy)| cx >= nt.x && cx < nt.x + nt.w && cy >= nt.y && cy < nt.y + nt.h)
                .unwrap_or(false);
            build_new_tab_button_quads(
                nt,
                nt_hover,
                NewTabButtonColors { hover_bg, primary, secondary },
                sw,
                sh,
                &mut quads,
            );

            // Tab titles: render as a single rich-text line where each tab title
            // is positioned by inserting padding spaces. This is approximate but
            // readable; precise per-tab text layout is a v0.4 polish item.
            //
            // Wezterm fancy-mode parity: every tab except the first is
            // prefixed with `│ ` (U+2502 BOX DRAWINGS LIGHT VERTICAL +
            // padding) drawn in `tab_inactive_fg` so a thin divider
            // appears between adjacent tab titles regardless of which
            // tab is active.
            // Tab font is `font_size + 1.0` (see ctor); scale the
            // approximate glyph width accordingly so the
            // column-arithmetic in `build_tab_title_spans` lines up
            // with where the shaped tab text actually lands.
            let tab_font_size = tab_title_font_size(self.font_size);
            let avg_glyph_w = (self.cell_w * (tab_font_size / self.font_size)).max(1.0);
            let tab_family_name = self.font_family.clone();
            let tab_inputs: Vec<TabSpanInput> = layout
                .tabs
                .iter()
                .map(|t| TabSpanInput {
                    index: t.idx,
                    title: &tabs.tabs()[t.idx].title,
                    title_x: t.title_rect.x,
                    title_w: t.title_rect.w,
                    is_active: layout.active == Some(t.idx),
                    badge: tabs.tabs()[t.idx]
                        .command
                        .clone()
                        .badge(now, layout.active == Some(t.idx)),
                })
                .collect();
            let (title_text, mut tab_spans) = build_tab_title_spans(
                &tab_inputs,
                avg_glyph_w,
                self.tab_active_fg,
                self.tab_inactive_fg,
            );
            // Phase D D3 (Epic #289, Haiku follow-up): dim the source
            // tab's TITLE TEXT at the same alpha as the source-tab
            // body quad so the dragged tab visibly "lifts off"
            // instead of leaving the title fully opaque on top of a
            // 30 %-dimmed body. The dim quad lands on top of the
            // text in z-order, so without this the title text was
            // still readable at full opacity (Haiku reviewer finding
            // on PR #298).
            if let Some(src_idx) = source_tab_idx {
                for (i, t) in tab_inputs.iter().enumerate() {
                    if t.index == src_idx {
                        if let Some(entry) = tab_spans.get_mut(i) {
                            entry.1 = scale_glyphon_alpha(entry.1, source_alpha);
                        }
                        break;
                    }
                }
            }
            let TabTitleRichTextSpans { spans: spans2, default_attrs } =
                build_tab_title_rich_text_spans(
                    &title_text,
                    &tab_spans,
                    tab_family_name.as_str(),
                    self.tab_inactive_fg,
                );
            self.tab_buffer.set_rich_text(
                &mut self.font_system,
                spans2,
                &default_attrs,
                Shaping::Advanced,
                None,
            );
            self.tab_buffer.shape_until_scroll(&mut self.font_system, false);
        }
        // -------- Search highlights + status bar ---------------------------
        // When search is active: paint a translucent yellow quad over every
        // match in the grid, then draw a single-line status bar pinned to
        // the bottom edge styled like the tab bar.
        let search_bar_h = self.font_size * 0.85 * 1.2;
        let mut search_bar_top = 0.0_f32;
        let mut have_search_bar = false;
        if let Some(s) = search {
            let cur_idx = s.current;
            for (i, m) in s.matches.iter().enumerate() {
                // Skip matches that aren't on screen (scrollback / off-viewport).
                let Some(visible_row) = s.match_visible_row(m) else { continue };
                if visible_row >= grid.rows || m.col_end <= m.col_start {
                    continue;
                }
                let x = active_origin_x + f32::from(m.col_start) * self.cell_w;
                let y = active_origin_y + f32::from(visible_row) * self.cell_h;
                let w = f32::from(m.col_end - m.col_start) * self.cell_w;
                let color = if Some(i) == cur_idx {
                    self.search_highlight_current
                } else {
                    self.search_highlight
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
                        color,
                        ..Default::default()
                    });
                }
            }
            // Status bar background pinned to bottom edge.
            search_bar_top = sh - search_bar_h;
            have_search_bar = true;
            quads.push(QuadInstance {
                rect: px_to_ndc(0.0, search_bar_top, sw, search_bar_h, sw, sh),
                color: self.search_bg,
                ..Default::default()
            });
            let n = s.matches.len();
            let cur = s.current.map(|i| i + 1).unwrap_or(0);
            let label = if n == 0 {
                format!("/ {} — no matches", s.query)
            } else {
                format!("/ {} — {}/{} matches", s.query, cur, n)
            };
            self.search_buffer.set_text(
                &mut self.font_system,
                &label,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.search_buffer.shape_until_scroll(&mut self.font_system, false);
        }

        // -------- Bottom-right search bar (state-only overlay) -------------
        // This is the lightweight "N/M" badge that lives in the corner,
        // distinct from the legacy full-width status bar above. It shows
        // whenever search state exists, so the user has a persistent
        // affordance while typing.
        let search_bar_layout = search.map(|_| SearchBarLayout::compute(sw, sh));
        let mut have_search_overlay = false;
        if let (Some(s), Some(layout)) = (search, search_bar_layout) {
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                color: self.hyperlink_underline,
                ..Default::default()
            });
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: self.search_bg,
                ..Default::default()
            });
            let label = search_bar_label(s);
            self.search_buffer.set_text(
                &mut self.font_system,
                &label,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.search_buffer.shape_until_scroll(&mut self.font_system, false);
            have_search_overlay = true;
            // Repurpose the (now-redundant) top status-bar slot below by
            // hiding it when the corner overlay carries the same info.
            have_search_bar = false;
            search_bar_top = layout.bg.y;
        }

        // -------- Command palette overlay ----------------------------------
        let palette_layout = palette.and_then(|p| PaletteLayout::compute(p, sw, sh));
        if let Some(layout) = &palette_layout {
            // Chrome colors are derived from the active theme so the palette
            // tracks the user's chosen palette instead of hardcoded
            // Tokyo Night literals (see UiPalette::from_theme).
            let palette_chrome = crate::ui_tokens::UiPalette::from_theme(theme);
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
                        color: [accent_rgba[0], accent_rgba[1], accent_rgba[2], 0.16],
                        size_px: [row.rect.w, row.rect.h],
                        radius_px: PALETTE_ROW_RADIUS,
                        ..Default::default()
                    });
                }
            }
            // Selected row left accent strip — full-opacity theme accent.
            // 3px wide, rounded with a 1.5px radius so it reads as a pill.
            if let Some(accent) = &layout.selected_accent {
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(accent.x, accent.y, accent.w, accent.h, sw, sh),
                    color: accent_rgba,
                    size_px: [accent.w, accent.h],
                    radius_px: accent.w * 0.5,
                    ..Default::default()
                });
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
            let query_text = if let Some(ph) = &layout.query_placeholder {
                ph.clone()
            } else {
                layout.query_label.clone()
            };
            self.palette_query_buffer.set_text(
                &mut self.font_system,
                &query_text,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.palette_query_buffer.shape_until_scroll(&mut self.font_system, false);

            // Shape the action-list as one multi-line buffer; the renderer
            // positions it at the first row's y and lets glyphon stack
            // lines at the buffer's line height (set to PALETTE_ROW_HEIGHT
            // so each label aligns with its row background quad). When
            // there are no matches, paint the empty-state placeholder +
            // hint instead.
            let mut rows_text = String::new();
            for (i, label) in layout.row_labels.iter().enumerate() {
                if i > 0 {
                    rows_text.push('\n');
                }
                rows_text.push_str(label);
            }
            if let Some(ph) = &layout.empty_label {
                rows_text.push_str(ph);
                if let Some(hint) = &layout.empty_hint {
                    rows_text.push('\n');
                    rows_text.push_str(hint);
                }
            }
            self.palette_rows_buffer.set_text(
                &mut self.font_system,
                &rows_text,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.palette_rows_buffer.shape_until_scroll(&mut self.font_system, false);

            // Footer hint — rendered into a dedicated buffer positioned in
            // `layout.footer` (see palette_footer_area below). Painting it
            // here rather than appending to `palette_rows_buffer` means
            // the hint always sits inside the footer strip instead of
            // being pushed up into the action list.
            self.palette_footer_buffer.set_text(
                &mut self.font_system,
                &layout.footer_label,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.palette_footer_buffer.shape_until_scroll(&mut self.font_system, false);
        }

        // -------- Keyboard shortcuts cheat sheet overlay --------------------
        let cheatsheet_layout = cheatsheet
            .as_ref()
            .map(|(state, bindings)| compute_cheatsheet_layout(state, bindings, sw, sh));
        if let Some(layout) = &cheatsheet_layout {
            let palette_chrome = crate::ui_tokens::UiPalette::from_theme(theme);
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
                        color: [accent_rgba[0], accent_rgba[1], accent_rgba[2], 0.16],
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
            self.cheatsheet_query_buffer.set_text(
                &mut self.font_system,
                &layout.query_label,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.cheatsheet_query_buffer.shape_until_scroll(&mut self.font_system, false);
            self.cheatsheet_rows_buffer.set_text(
                &mut self.font_system,
                &layout.rows_text,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.cheatsheet_rows_buffer.shape_until_scroll(&mut self.font_system, false);
            self.cheatsheet_footer_buffer.set_text(
                &mut self.font_system,
                &layout.footer_label,
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.cheatsheet_footer_buffer.shape_until_scroll(&mut self.font_system, false);
        }

        // -------- IME preedit overlay --------------------------------------
        let ime_layout = ime.and_then(|i| {
            let cursor_x = active_origin_x + f32::from(grid.cursor.col) * self.cell_w;
            let cursor_y = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
            ImePreeditLayout::compute(i, cursor_x, cursor_y, self.cell_w, self.cell_h, sw, sh)
        });
        if let (Some(state), Some(layout)) = (ime, &ime_layout) {
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: [0.10, 0.11, 0.14, 0.95],
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
            self.ime_buffer.set_text(
                &mut self.font_system,
                state.preedit(),
                &terminal_font_attrs(&self.font_family).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.ime_buffer.shape_until_scroll(&mut self.font_system, false);
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
            let label = (0..broadcast_label_rects.len())
                .map(|_| "⚠ BROADCAST")
                .collect::<Vec<_>>()
                .join("\n");
            self.broadcast_buffer.set_size(
                &mut self.font_system,
                Some(self.config.width as f32),
                Some(
                    (self.font_size * 1.5 * broadcast_label_rects.len() as f32)
                        .max(self.font_size * 1.5),
                ),
            );
            self.broadcast_buffer.set_text(
                &mut self.font_system,
                &label,
                &terminal_font_attrs(&self.font_family)
                    .color(hex_to_glyphon(theme.colors.bright.yellow.0.as_str())),
                Shaping::Advanced,
                None,
            );
            self.broadcast_buffer.shape_until_scroll(&mut self.font_system, false);
        } else {
            self.broadcast_buffer.set_text(
                &mut self.font_system,
                "",
                &Attrs::new(),
                Shaping::Advanced,
                None,
            );
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
                let mut line_color = crate::ui_tokens::UiPalette::from_theme(theme).accent;
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

            // Title text via glyphon: shape into the dedicated
            // drag-chip buffer so it composites on top of the ghost
            // body. Clipping is handled by TextBounds below.
            //
            // Phase D D1 (Haiku follow-up on PR #298): scale the
            // text color alpha by `chip.ghost_alpha` (spec 0.5) so
            // the GHOST TITLE matches the ghost body translucency.
            // Without this the body painted at 50 % alpha but the
            // title text rode on top at full opacity, which read
            // as "solid title on a faint plate" rather than a
            // unified ghost.
            if !chip.title.is_empty() {
                let ghost_fg =
                    scale_glyphon_alpha(self.tab_active_fg, chip.ghost_alpha.clamp(0.0, 1.0));
                let attrs = terminal_font_attrs(&self.font_family).color(ghost_fg);
                self.drag_chip_buffer.set_text(
                    &mut self.font_system,
                    &chip.title,
                    &attrs,
                    Shaping::Advanced,
                    None,
                );
                self.drag_chip_buffer.shape_until_scroll(&mut self.font_system, false);
            } else {
                self.drag_chip_buffer.set_text(
                    &mut self.font_system,
                    "",
                    &Attrs::new(),
                    Shaping::Advanced,
                    None,
                );
            }
            self.drag_chip_visual = Some(DragChipVisual { top_left: (x0, y0), size: (w, h) });
        } else {
            self.drag_chip_visual = None;
        }

        // Glyphon converts TextArea pixel positions to NDC using the
        // Resolution we hand it. Our positions (left/top/bounds) are in
        // LOGICAL pixels (they're computed from padding/cell_w/etc),
        // so the Resolution must match — feeding physical surface dims
        // here would shrink every text area 2× on Retina.
        self.viewport.update(
            &self.queue,
            Resolution {
                width: (self.config.width as f32 / self.scale_factor) as u32,
                height: (self.config.height as f32 / self.scale_factor) as u32,
            },
        );

        let bar_h = self.tab_bar_logical_height();
        let title_top =
            self.titlebar_inset + ((bar_h - self.font_size * 0.85 * 1.2) / 2.0).max(0.0);
        let tab_area = if self.tab_bar_visible {
            Some(TextArea {
                buffer: &self.tab_buffer,
                left: 0.0,
                top: title_top,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: self.titlebar_inset as i32,
                    right: self.config.width as i32,
                    bottom: (self.titlebar_inset + bar_h) as i32,
                },
                default_color: self.tab_inactive_fg,
                custom_glyphs: &[],
            })
        } else {
            None
        };

        let search_area = if have_search_bar {
            Some(TextArea {
                buffer: &self.search_buffer,
                left: self.padding_left,
                top: search_bar_top,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: search_bar_top as i32,
                    right: self.config.width as i32,
                    bottom: self.config.height as i32,
                },
                default_color: self.search_fg,
                custom_glyphs: &[],
            })
        } else {
            None
        };

        // Overlay text areas. Each is built only when its state is active.
        let search_overlay_area = if have_search_overlay {
            search_bar_layout.map(|layout| TextArea {
                buffer: &self.search_buffer,
                left: layout.bg.x + 6.0,
                top: layout.bg.y + 4.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: layout.bg.x as i32,
                    top: layout.bg.y as i32,
                    right: (layout.bg.x + layout.bg.w) as i32,
                    bottom: (layout.bg.y + layout.bg.h) as i32,
                },
                default_color: self.search_fg,
                custom_glyphs: &[],
            })
        } else {
            None
        };

        let palette_query_area = palette_layout.as_ref().map(|layout| TextArea {
            buffer: &self.palette_query_buffer,
            // Left padding inside the query field — matches PALETTE_ROW_PAD_X so the
            // placeholder/typed text doesn't hug the rounded left edge.
            left: layout.query_row.x + crate::overlays::PALETTE_ROW_PAD_X,
            top: layout.query_row.y + 2.0,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.query_row.x as i32,
                top: layout.query_row.y as i32,
                right: (layout.query_row.x + layout.query_row.w) as i32,
                bottom: (layout.query_row.y + layout.query_row.h) as i32,
            },
            default_color: self.search_fg,
            custom_glyphs: &[],
        });
        let palette_rows_area = palette_layout.as_ref().and_then(|layout| {
            // Pick the y-position: first real row when there are matches,
            // otherwise just below the query row for the empty placeholder.
            let (row_x, row_y) = if let Some(first) = layout.rows.first() {
                (first.rect.x, first.rect.y)
            } else if layout.empty_label.is_some() {
                let y = layout.query_row.y + layout.query_row.h + PALETTE_INNER_PAD;
                (layout.bg.x + PALETTE_INNER_PAD, y)
            } else {
                return None;
            };
            // Vertically center the row text inside its 40 px background
            // rect. The rows buffer's line_height is set to
            // (PALETTE_ROW_HEIGHT + PALETTE_ROW_GAP), so consecutive
            // lines stack at the same stride as the highlight rects.
            // glyphon places each line so its line-box (of size
            // line_height) starts at `top`. To center that line-box
            // inside the 40 px highlight, shift `top` up by half the
            // difference — i.e. offset = (HEIGHT - line_height) * 0.5,
            // which is negative when line_height > HEIGHT (the case here:
            // 40 - 44 = -2). The previous formula derived the offset from
            // `font_size`, which let the visual baseline sit BELOW the
            // highlight rect at any non-default font size — that was the
            // shipped regression where the gold highlight floated ABOVE
            // the row's text instead of wrapping it.
            let line_height =
                crate::overlays::PALETTE_ROW_HEIGHT + crate::overlays::PALETTE_ROW_GAP;
            let text_top_offset = (crate::overlays::PALETTE_ROW_HEIGHT - line_height) * 0.5;
            Some(TextArea {
                buffer: &self.palette_rows_buffer,
                // Match PALETTE_ROW_PAD_X — 4px hugged the rounded highlight edge.
                left: row_x + crate::overlays::PALETTE_ROW_PAD_X,
                top: row_y + text_top_offset,
                scale: 1.0,
                bounds: TextBounds {
                    left: layout.bg.x as i32,
                    top: (row_y + text_top_offset).floor() as i32,
                    right: (layout.bg.x + layout.bg.w) as i32,
                    bottom: (layout.bg.y + layout.bg.h) as i32,
                },
                default_color: self.search_fg,
                custom_glyphs: &[],
            })
        });
        let palette_footer_area = palette_layout.as_ref().map(|layout| TextArea {
            buffer: &self.palette_footer_buffer,
            left: layout.footer.x + 12.0,
            top: layout.footer.y + 8.0,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.footer.x as i32,
                top: layout.footer.y as i32,
                right: (layout.footer.x + layout.footer.w) as i32,
                bottom: (layout.footer.y + layout.footer.h) as i32,
            },
            default_color: self.search_fg,
            custom_glyphs: &[],
        });
        let cheatsheet_query_area = cheatsheet_layout.as_ref().map(|layout| TextArea {
            buffer: &self.cheatsheet_query_buffer,
            left: layout.query_row.x + 12.0,
            top: layout.query_row.y + 2.0,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.query_row.x as i32,
                top: layout.query_row.y as i32,
                right: (layout.query_row.x + layout.query_row.w) as i32,
                bottom: (layout.query_row.y + layout.query_row.h) as i32,
            },
            default_color: self.search_fg,
            custom_glyphs: &[],
        });
        let cheatsheet_rows_area = cheatsheet_layout.as_ref().map(|layout| {
            let (x, y) = layout.rows.first().map(|row| (row.x, row.y)).unwrap_or((
                layout.bg.x + PALETTE_INNER_PAD,
                layout.query_row.y + layout.query_row.h + PALETTE_INNER_PAD,
            ));
            let line_height = PALETTE_ROW_HEIGHT + PALETTE_ROW_GAP;
            TextArea {
                buffer: &self.cheatsheet_rows_buffer,
                left: x + 12.0,
                top: y + (PALETTE_ROW_HEIGHT - line_height) * 0.5,
                scale: 1.0,
                bounds: TextBounds {
                    left: layout.bg.x as i32,
                    top: y as i32,
                    right: (layout.bg.x + layout.bg.w) as i32,
                    bottom: layout.footer.y as i32,
                },
                default_color: self.search_fg,
                custom_glyphs: &[],
            }
        });
        let cheatsheet_footer_area = cheatsheet_layout.as_ref().map(|layout| TextArea {
            buffer: &self.cheatsheet_footer_buffer,
            left: layout.footer.x + 12.0,
            top: layout.footer.y + 8.0,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.footer.x as i32,
                top: layout.footer.y as i32,
                right: (layout.footer.x + layout.footer.w) as i32,
                bottom: (layout.footer.y + layout.footer.h) as i32,
            },
            default_color: self.search_fg,
            custom_glyphs: &[],
        });
        let ime_area = ime_layout.as_ref().map(|layout| TextArea {
            buffer: &self.ime_buffer,
            left: layout.bg.x + 4.0,
            top: layout.bg.y + 2.0,
            scale: 1.0,
            bounds: TextBounds {
                left: layout.bg.x as i32,
                top: layout.bg.y as i32,
                right: (layout.bg.x + layout.bg.w) as i32,
                bottom: (layout.bg.y + layout.bg.h) as i32,
            },
            default_color: self.search_fg,
            custom_glyphs: &[],
        });

        // Pre-overlay text areas: tab bar titles + (legacy) bottom status bar.
        // These render BEFORE overlay quads/text, so any overlay drawn on top
        // will visually cover them — same as the terminal grid glyphs.
        let mut areas: Vec<TextArea> = Vec::new();
        if let Some(a) = tab_area {
            areas.push(a);
        }
        if let Some(a) = search_area {
            areas.push(a);
        }

        // Overlay text areas: every dialog/popup/transient piece of UI that
        // should sit ABOVE both terminal text and pre-overlay chrome. Driven
        // through a dedicated TextRenderer so the draw call can be sequenced
        // after the terminal glyph pipeline inside the render pass below.
        let mut overlay_areas: Vec<TextArea> = Vec::new();
        if let Some(a) = search_overlay_area {
            overlay_areas.push(a);
        }
        if let Some(a) = palette_query_area {
            overlay_areas.push(a);
        }
        if let Some(a) = palette_rows_area {
            overlay_areas.push(a);
        }
        if let Some(a) = palette_footer_area {
            overlay_areas.push(a);
        }
        if let Some(a) = cheatsheet_query_area {
            overlay_areas.push(a);
        }
        if let Some(a) = cheatsheet_rows_area {
            overlay_areas.push(a);
        }
        if let Some(a) = cheatsheet_footer_area {
            overlay_areas.push(a);
        }
        if let Some(a) = ime_area {
            overlay_areas.push(a);
        }
        for (idx, rect) in broadcast_label_rects.iter().enumerate() {
            let line_h = self.font_size * 0.85 * 1.2;
            overlay_areas.push(TextArea {
                buffer: &self.broadcast_buffer,
                left: rect.x + 10.0,
                top: rect.y + 4.0 - idx as f32 * line_h,
                scale: 1.0,
                bounds: TextBounds {
                    left: rect.x as i32,
                    top: rect.y as i32,
                    right: (rect.x + rect.w) as i32,
                    bottom: (rect.y + (self.font_size * 1.45).max(20.0)) as i32,
                },
                default_color: hex_to_glyphon(theme.colors.bright.yellow.0.as_str()),
                custom_glyphs: &[],
            });
        }
        if quick_select_hint_count > 0 {
            overlay_areas.push(TextArea {
                buffer: &self.quick_select_buffer,
                left: 0.0,
                top: 0.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.config.width as i32,
                    bottom: self.config.height as i32,
                },
                default_color: self.tab_active_fg,
                custom_glyphs: &[],
            });
        }
        // Drag-chip title: render in the overlay text pass so it sits
        // above terminal glyphs and tab chrome. Mirrors the chip rect
        // computed in the overlay quad block above.
        if let Some(v) = self.drag_chip_visual {
            overlay_areas.push(TextArea {
                buffer: &self.drag_chip_buffer,
                left: v.top_left.0 + 6.0,
                top: v.top_left.1 + (v.size.1 - self.font_size * 0.85 * 1.2).max(0.0) * 0.5,
                scale: 1.0,
                bounds: TextBounds {
                    left: (v.top_left.0 + 4.0) as i32,
                    top: v.top_left.1 as i32,
                    right: (v.top_left.0 + v.size.0 - 4.0) as i32,
                    bottom: (v.top_left.1 + v.size.1) as i32,
                },
                default_color: self.tab_active_fg,
                custom_glyphs: &[],
            });
        }

        // B3: push any new glyph tiles to the GPU texture before any
        // draw call samples it. Must come AFTER the grid walk above
        // (which is what populated the dirty rects) and BEFORE the
        // text_pipeline.draw call in the render pass below.
        self.glyph_upload.sync(&self.queue, &mut self.glyph_atlas);

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash_cache,
        )?;
        self.text_renderer_overlay.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            overlay_areas,
            &mut self.swash_cache,
        )?;

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
            self.quad.draw(&self.device, &self.queue, &mut pass, &quads);
            // B3 grid text: instanced atlas quads. Sampled per-cell
            // from `self.glyph_atlas`'s GPU texture via `glyph_upload`.
            self.text_pipeline.draw(
                &self.device,
                &self.queue,
                &mut pass,
                self.glyph_upload.bind_group(),
                &glyph_instances,
            );
            self.text_renderer.render(&self.atlas, &self.viewport, &mut pass)?;
            // Overlay layer — backgrounds first, then text — drawn LAST so
            // command-palette / search-input / IME dialogs visually cover
            // the terminal content underneath. Order matters within the
            // pass: quad_overlay establishes the dim/dialog backdrop,
            // text_renderer_overlay paints the palette query, action rows,
            // search badge and IME preedit on top. (PR #45 review fix.)
            self.quad_overlay.draw(&self.device, &self.queue, &mut pass, &quads_overlay);
            self.text_renderer_overlay.render(&self.atlas, &self.viewport, &mut pass)?;
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.atlas.trim();
        // Publish the per-frame missing-glyph list for tests / diagnostics.
        // Done after submit so the value reflects what the user actually
        // saw on screen (not a partial work-in-progress list).
        self.last_missing_chars = missing_chars_this_frame;
        // Cache key only after a successful submit+present. Transient
        // surface states (Outdated/Lost/Timeout) that returned early
        // before this point will not cache, so the next redraw will
        // re-attempt rendering.
        self.last_frame_key = Some(key);
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

    #[allow(clippy::too_many_arguments)]
    fn prepare_quick_select_overlay(
        &mut self,
        quick_select: &QuickSelectState,
        origin_x: f32,
        origin_y: f32,
        scrollback_len: usize,
        visible_rows: usize,
        theme: &Theme,
        sw: f32,
        sh: f32,
        quads_overlay: &mut Vec<QuadInstance>,
    ) {
        let mut overlay = String::new();
        for hint in &quick_select.hints {
            let Some(visible_row) = hint.row.checked_sub(scrollback_len) else { continue };
            if visible_row >= visible_rows {
                continue;
            }
            let x = origin_x + hint.col_start as f32 * self.cell_w;
            let y = origin_y + visible_row as f32 * self.cell_h;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x, y, self.cell_w, self.cell_h, sw, sh),
                color: self.cursor_color,
                ..Default::default()
            });
            for _ in 0..hint.col_start {
                overlay.push(' ');
            }
            overlay.push(hint.hint);
            overlay.push('\n');
        }
        self.quick_select_buffer.set_text(
            &mut self.font_system,
            &overlay,
            &terminal_font_attrs(&self.font_family)
                .color(hex_to_glyphon(theme.colors.background.0.as_str())),
            Shaping::Advanced,
            None,
        );
        self.quick_select_buffer.shape_until_scroll(&mut self.font_system, false);
    }

    /// Shape a single style-run worth of cells and append the
    /// resulting glyph instances + missing-glyph tofus to the frame's
    /// queues. Factored out of the per-row loop so the loop body stays
    /// readable; otherwise it would inline ~80 lines of placement +
    /// fallback handling four times (run start, mid-row flush, end of
    /// row, etc.).
    // Hot inner-loop helper called per shaped run per row. Every
    // argument is an exclusive `&mut` borrow of a *different* field of
    // `GpuRenderer` (atlas, rasterizer, shape cache, instance buffers,
    // missing-glyph trackers) — bundling them into a struct would force
    // a single `&mut Ctx` that conflicts with the surrounding loop's
    // own borrows. Suppression stays with this explanatory comment.
    #[allow(clippy::too_many_arguments)]
    fn flush_shape_run(
        glyph_atlas: &mut GlyphAtlas,
        font_family: &str,
        font_size: f32,
        scale_factor: f32,
        rasterizer: &mut SwashRasterizer,
        shape_cache: &mut ShapeCache,
        glyph_instances: &mut Vec<GlyphInstance>,
        missing_tofu: &mut Vec<(f32, f32, f32, f32, GColor)>,
        missing_chars_this_frame: &mut Vec<char>,
        row: u16,
        _run_first_col: u16,
        style: RunStyle,
        cells: &[(u16, Cell)],
        theme: &Theme,
        fg_default: GColor,
        cell_w: f32,
        cell_h: f32,
        top_inset: f32,
        pad: f32,
        sw: f32,
        sh: f32,
        baseline_y_in_cell: f32,
    ) {
        if cells.is_empty() {
            return;
        }

        // ASCII fast path: every cell is printable-ASCII with no
        // cluster extras, so the shaper would emit a 1:1 mapping
        // anyway. Skip cosmic-text entirely and drive the glyph atlas
        // straight from each cell's GlyphKey.
        if run_is_ascii_fast(cells) {
            for (col, cell) in cells {
                let key = sonic_core::glyph_key::GlyphKey {
                    ch: cell.ch,
                    font_slot: 0,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                };
                let Some(info) = glyph_atlas.get_or_insert(key, rasterizer) else {
                    if !cell.ch.is_whitespace() {
                        missing_chars_this_frame.push(cell.ch);
                    }
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    continue;
                }
                let cx = pad + f32::from(*col) * cell_w;
                let cy = top_inset + f32::from(row) * cell_h;
                let inv_s = 1.0 / scale_factor;
                let gx = cx + info.px_offset[0] as f32 * inv_s;
                let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
                let gw = info.px_size[0] as f32 * inv_s;
                let gh = info.px_size[1] as f32 * inv_s;
                let color = cell_fg(cell, theme, fg_default);
                let rgba = glyphon_color_to_linear_rgba(color);
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: [0.0, 0.0, 0.0, 0.0],
                });
            }
            return;
        }

        let shaped = shape_cache.get_or_shape(rasterizer, font_family, font_size, style, cells);

        // Build a lookup from col → cell so we can recover per-cell
        // attributes (color, WIDE flag, the actual codepoint for tofu
        // diagnostics) from the shaped output's `lead_col`.
        let mut cell_by_col: std::collections::HashMap<u16, Cell> =
            std::collections::HashMap::with_capacity(cells.len());
        for (col, c) in cells {
            cell_by_col.insert(*col, c.clone());
        }

        for g in shaped {
            let lead_cell = cell_by_col.get(&g.lead_col).cloned().unwrap_or_default();
            let is_wide = lead_cell.flags.contains(CellFlags::WIDE);
            let cell_pixel_width = if is_wide { cell_w * 2.0 } else { cell_w };

            // glyph_id == 0 from the shaper means one of two things:
            //   (a) true notdef — cosmic-text couldn't shape it at all
            //       (lead_cell.ch is '\0' or whitespace), OR
            //   (b) cosmic-text shaped through an OS font outside our
            //       fallback chain, so `shape_run` zeroed the glyph_id
            //       to fall back to the char-based path (see comment in
            //       shape.rs). In that case lead_cell.ch is a real
            //       printable codepoint and we should resolve a slot
            //       via the rasterizer's charmap walk and rasterize
            //       through the char path instead of drawing tofu.
            //
            // Regression target: CJK + emoji mangled to wrong glyphs in
            // production (PR fix/cjk-render-mangled-v2). The old
            // unwrap_or(0) in shape.rs caused '中' to render as '臭'
            // because the shaped id was sent to the primary font.
            if g.glyph_id == 0 {
                let ch = lead_cell.ch;
                if ch == '\0' || ch.is_whitespace() {
                    continue;
                }
                // Try char-based fallback resolution.
                let resolved = rasterizer.resolve_slot(ch, style.bold, style.italic);
                let Some(slot) = resolved else {
                    // Every face in the chain lacks this codepoint —
                    // genuine tofu.
                    let cx = pad + f32::from(g.lead_col) * cell_w;
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
                let key = sonic_core::glyph_key::GlyphKey {
                    ch,
                    font_slot: slot,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                };
                let Some(info) = glyph_atlas.get_or_insert(key, rasterizer) else {
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    continue;
                }
                let cx = pad + f32::from(g.lead_col) * cell_w;
                let cy = top_inset + f32::from(row) * cell_h;
                // Atlas tiles are rasterized at `font_size * scale_factor`
                // physical pixels, but GPU output is in *logical* units —
                // we MUST scale back by `inv_s`. The shaped path below
                // applies this; the char-based fallback used to omit it,
                // producing CJK + emoji glyphs at 2x size on Retina that
                // overflowed into the next cell horizontally and stomped
                // neighbouring Latin text. Regression target:
                // `wide_cell_glyph_width_does_not_exceed_two_cells`.
                let inv_s = 1.0 / scale_factor;
                let gx = cx + info.px_offset[0] as f32 * inv_s;
                let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
                let mut gw = info.px_size[0] as f32 * inv_s;
                let mut gh = info.px_size[1] as f32 * inv_s;
                // Clamp tile to the cell box the codepoint reserves
                // (1 cell for narrow, 2 for WIDE). Some fallback faces
                // (notably Apple Color Emoji at small sizes, certain CJK
                // fonts) emit bitmaps slightly wider than the cell box;
                // unclamped they bleed into the following column.
                if gw > cell_pixel_width {
                    let ratio = cell_pixel_width / gw;
                    gw = cell_pixel_width;
                    gh *= ratio;
                }
                let color = cell_fg(&lead_cell, theme, fg_default);
                // Color glyphs (emoji) carry their own colour in the
                // BGRA atlas; the shader ignores `color` when
                // `flags.x >= 0.5`. Set `color` to white so that a
                // buggy shader fallback wouldn't tint the emoji red.
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    glyphon_color_to_linear_rgba(color)
                };
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
                });
                continue;
            }

            let key = sonic_core::glyph_key::GlyphKey::shaped(
                g.ch,
                g.font_slot,
                g.glyph_id,
                style.bold,
                style.italic,
            );
            let Some(info) = glyph_atlas.get_or_insert(key, rasterizer) else {
                continue;
            };
            if info.px_size[0] == 0 || info.px_size[1] == 0 {
                continue;
            }
            let cx = pad + f32::from(g.lead_col) * cell_w;
            let cy = top_inset + f32::from(row) * cell_h;
            let inv_s = 1.0 / scale_factor;
            let gx = cx + info.px_offset[0] as f32 * inv_s;
            let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
            let mut gw = info.px_size[0] as f32 * inv_s;
            let mut gh = info.px_size[1] as f32 * inv_s;
            // See the fallback path above for why we clamp to
            // `cell_pixel_width` — the same overflow class can occur on
            // shaped color emoji where the strike bitmap is slightly
            // wider than the reserved 2-cell box.
            if gw > cell_pixel_width {
                let ratio = cell_pixel_width / gw;
                gw = cell_pixel_width;
                gh *= ratio;
            }
            let color = cell_fg(&lead_cell, theme, fg_default);
            let rgba = if info.is_color {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                glyphon_color_to_linear_rgba(color)
            };
            glyph_instances.push(GlyphInstance {
                rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                uv: info.uv,
                color: rgba,
                flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            });
        }
    }
}

fn cell_fg(cell: &Cell, theme: &Theme, default: GColor) -> GColor {
    match cell.fg {
        Color::Default => default,
        Color::Rgb(r, g, b) => GColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
    }
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
    let (r, g, b) = match cell.bg {
        Color::Default => return None,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i) => match i {
            0..=15 => {
                let gc = indexed(i, theme)?;
                (gc.r(), gc.g(), gc.b())
            }
            16..=231 => {
                let v = i - 16;
                let r = v / 36;
                let g = (v / 6) % 6;
                let b = v % 6;
                let to8bit = |c: u8| if c == 0 { 0 } else { c * 40 + 55 };
                (to8bit(r), to8bit(g), to8bit(b))
            }
            232..=255 => {
                let g = (i - 232) * 10 + 8;
                (g, g, g)
            }
        },
    };
    let lut = super::color::srgb_u8_to_linear_lut();
    Some([lut[r as usize], lut[g as usize], lut[b as usize], 1.0])
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
    for r in 0..max_rows {
        let row_abs = view_top_abs + r as u64;
        let Some(row) = grid.row_at_abs(row_abs) else {
            continue;
        };
        // Run-length encode adjacent same-bg cells into one quad.
        let mut run_start: Option<u16> = None;
        let mut run_color: Option<[f32; 4]> = None;
        let mut col: u16 = 0;
        let flush =
            |start: u16, end_exclusive: u16, color: [f32; 4], out: &mut Vec<QuadInstance>| {
                let x = pad + f32::from(start) * cell_w;
                let y = top_inset + f32::from(r) * cell_h;
                let clipped_end = end_exclusive.min(max_cols);
                if clipped_end <= start {
                    return;
                }
                let w = f32::from(clipped_end - start) * cell_w;
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

fn indexed(i: u8, theme: &Theme) -> Option<GColor> {
    let p = &theme.colors;
    let pick = |h: &str| hex_to_glyphon(h);
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
        _ => None,
    }
}

fn hex_to_glyphon(h: &str) -> GColor {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    if h.len() == 6 {
        GColor::rgb(parse(0), parse(2), parse(4))
    } else {
        GColor::rgb(0, 0, 0)
    }
}

/// Multiply the alpha channel of a [`glyphon::Color`] by `factor`
/// (clamped to `0.0..=1.0`) and return a fresh color with the same
/// RGB triplet. Used by the Phase D drag-feedback path (Epic #289) to
/// dim the source-tab title text and the ghost-chip title text to
/// match their corresponding body quads — without this helper, those
/// titles painted at full opacity on top of dimmed bodies (Haiku
/// reviewer finding on PR #298).
#[doc(hidden)]
#[must_use]
pub fn scale_glyphon_alpha(c: GColor, factor: f32) -> GColor {
    let f = factor.clamp(0.0, 1.0);
    let a = ((c.a() as f32) * f).round().clamp(0.0, 255.0) as u8;
    GColor::rgba(c.r(), c.g(), c.b(), a)
}

/// Single source of truth for the [`Attrs`] used by every text-rendering
/// site (terminal grid, tab titles, command palette, search status bar,
/// IME pre-edit). Pass the user-configured `font.family` here so all UI
/// chrome shares the EXACT same `Family::Name(...)` as grid cells —
/// avoiding the historical bug where tab titles silently fell through
/// to `Family::Monospace` and rendered with a different installed face.
///
/// The implementation now lives in `sonic-text` so the shape layer can
/// share it without a back-edge into `sonic-shared`. Re-exported here so
/// every existing `crate::render::terminal_font_attrs` call site keeps
/// compiling unchanged.
pub use sonic_text::terminal_font_attrs;

/// Colors used when rendering the `+` new-tab button (extracted for testing).
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct NewTabButtonColors {
    pub hover_bg: [f32; 4],
    pub primary: [f32; 4],
    pub secondary: [f32; 4],
}

/// Emit the quads for the `+` new-tab button. When `hover` is true a
/// rounded (radius 8) `hover_bg` background is drawn underneath the
/// plus glyph and the glyph switches to `primary`; otherwise only the
/// `secondary`-colored plus is drawn.
///
/// Exposed for unit tests in `tests/tab_new_tab_button.rs`.
#[doc(hidden)]
pub fn build_new_tab_button_quads(
    nt: crate::tabbar_view::Rect,
    hover: bool,
    colors: NewTabButtonColors,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    if hover {
        out.push(QuadInstance {
            rect: px_to_ndc(nt.x, nt.y, nt.w, nt.h, sw, sh),
            color: colors.hover_bg,
            size_px: [nt.w, nt.h],
            radius_px: 8.0,
            ..Default::default()
        });
    }
    let plus_color = if hover { colors.primary } else { colors.secondary };
    let plus_thick = 2.0_f32;
    let plus_len = 12.0_f32;
    let pcx = nt.x + nt.w / 2.0;
    let pcy = nt.y + nt.h / 2.0;
    out.push(QuadInstance {
        rect: px_to_ndc(pcx - plus_len / 2.0, pcy - plus_thick / 2.0, plus_len, plus_thick, sw, sh),
        color: plus_color,
        ..Default::default()
    });
    out.push(QuadInstance {
        rect: px_to_ndc(pcx - plus_thick / 2.0, pcy - plus_len / 2.0, plus_thick, plus_len, sw, sh),
        color: plus_color,
        ..Default::default()
    });
}

/// Emit the diagonal × glyph for a tab's close button.
///
/// Parameters for [`build_close_x_quads`]. Grouping these in a struct
/// avoids the 8-positional-argument footgun where adjacent `f32`s could be
/// swapped silently. All units are physical pixels except `color` (linear
/// RGBA) and `sw`/`sh` (surface dimensions used by `px_to_ndc`).
#[derive(Clone, Copy)]
pub struct CloseXQuadParams {
    /// Top-left x of the glyph's bounding box, in physical px.
    pub x: f32,
    /// Top-left y of the glyph's bounding box, in physical px.
    pub y: f32,
    /// Side length of the (square) glyph bounding box, in physical px.
    pub glyph: f32,
    /// Stroke thickness in physical px.
    pub thick: f32,
    /// Linear-space RGBA colour for every emitted quad.
    pub color: [f32; 4],
    /// Surface width in physical px (passed through to `px_to_ndc`).
    pub sw: f32,
    /// Surface height in physical px (passed through to `px_to_ndc`).
    pub sh: f32,
}

/// Push the quads that draw a small × close-button glyph into `out`.
///
/// See [`CloseXQuadParams`] for the geometry. Pushes ~`ceil(glyph/thick)*2`
/// axis-aligned squares stair-stepped along the ╲ and ╱ diagonals; every
/// emitted quad sits strictly inside `[x, x+glyph] × [y, y+glyph]`.
///
/// The wgpu quad pipeline does not support rotation, and PR #117 emitted a
/// horizontal+vertical pair that read as `+` instead of `×`. Stair-stepping
/// a small SDF-free square per step is the simplest and least-invasive fix;
/// visually indistinguishable from a rotated line at 14×14 px.
pub fn build_close_x_quads(params: CloseXQuadParams, out: &mut Vec<QuadInstance>) {
    let CloseXQuadParams { x, y, glyph, thick, color, sw, sh } = params;
    let steps = ((glyph / thick).ceil() as usize).max(2);
    let dot = thick;
    for s in 0..steps {
        let t_frac = (s as f32) / ((steps - 1) as f32);
        let along = t_frac * (glyph - dot);
        // ╲ diagonal: top-left → bottom-right
        out.push(QuadInstance {
            rect: px_to_ndc(x + along, y + along, dot, dot, sw, sh),
            color,
            ..Default::default()
        });
        // ╱ diagonal: top-right → bottom-left
        out.push(QuadInstance {
            rect: px_to_ndc(x + (glyph - dot - along), y + along, dot, dot, sw, sh),
            color,
            ..Default::default()
        });
    }
}

/// Walk the grid and collect runs of contiguous cells that share a hyperlink
/// id, per row. Wide-cell continuations don't break a run (they inherit the
/// lead cell's hyperlink). Returns `(row, col_start, col_end_inclusive)`.
#[doc(hidden)]
pub fn collect_hyperlink_runs(grid: &Grid) -> Vec<(u16, u16, u16)> {
    let mut runs = Vec::new();
    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut start: Option<u16> = None;
        let mut current: Option<sonic_core::hyperlink::HyperlinkId> = None;
        let mut last_col: u16 = 0;
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                if start.is_some() {
                    last_col = col as u16;
                }
                continue;
            }
            match (cell.hyperlink, current) {
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

/// Load any TTF/OTF files we ship in `assets/fonts/` into the cosmic-text
/// font database. Looks in two places, in this order:
///   1. `<exe-dir>/assets/fonts/` — what the .app/.msi bundles ship
///   2. `<workspace-root>/assets/fonts/` — dev (`cargo run`)
fn load_bundled_fonts(fs: &mut FontSystem) {
    sonic_text::swash_rasterizer::load_bundled_fonts(fs);
}

/// Stable fingerprint for command badges, including wall-clock buckets that
/// change when badge visibility can transition without a tab model mutation.
#[doc(hidden)]
pub fn command_status_hash(status: &sonic_ui::tabs::CommandStatus, now: Instant) -> u64 {
    match status {
        sonic_ui::tabs::CommandStatus::Idle => 0,
        sonic_ui::tabs::CommandStatus::Running(started_at) => {
            let elapsed_secs = now.duration_since(*started_at).as_secs().min(5);
            let badge_visible = u64::from(now.duration_since(*started_at).as_secs() > 5);
            1 | (elapsed_secs << 32) | (badge_visible << 40)
        }
        sonic_ui::tabs::CommandStatus::Done { exit, until } => {
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
pub fn selection_quad_rects(
    sel: &sonic_ui::selection::Selection,
    rows: u16,
    cols: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
) -> Vec<(f32, f32, f32, f32)> {
    if sel.is_empty() {
        return Vec::new();
    }
    let (a, b) = sel.normalized();
    let mut out = Vec::with_capacity(usize::from(b.0.saturating_sub(a.0)) + 1);
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
        let x = origin_x + f32::from(col_a) * cell_w;
        let y = origin_y + f32::from(r) * cell_h;
        let w = f32::from(col_b - col_a + 1) * cell_w;
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
