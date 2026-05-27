//! GPU renderer for the terminal grid using wgpu 29 + glyphon 0.11.
#![allow(deprecated)] // PR #119 deprecated literal `color::*` helpers — one residual site (drop-line indicator) pending migration.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use sonic_core::{
    grid::{Cell, CellFlags, Color, Grid},
    theme::Theme,
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, MultisampleState, Operations, PresentMode, RenderPassColorAttachment,
    RenderPassDescriptor, RequestAdapterOptions, SurfaceConfiguration, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::{
    command_palette::CommandPalette,
    cursor::{self, CursorShape},
    glyph_atlas::{AtlasUpload, GlyphAtlas},
    ime::ImeState,
    overlays::{
        search_bar_label, ImePreeditLayout, PaletteLayout, SearchBarLayout, PALETTE_BORDER,
        PALETTE_INNER_PAD, PALETTE_PANEL_RADIUS, PALETTE_QUERY_RADIUS, PALETTE_ROW_RADIUS,
    },
    pane::Rect as PaneRect,
    quad::{px_to_ndc, QuadInstance, QuadPipeline},
    search::SearchState,
    selection::Selection,
    shape::{run_is_ascii_fast, RunStyle, ShapeCache},
    swash_rasterizer::{self, SwashRasterizer},
    tabbar_view::{tab_bar_height, TabBarLayout, TAB_BAR_HEIGHT, TAB_GAP},
    tabs::TabBar,
    text_pipeline::{GlyphInstance, TextPipeline},
};

// (Per-row cache + grid SpanDesc removed in the B3 cutover — the GPU
// atlas does an O(1) lookup per cell, so the bookkeeping is wasted
// work. Walking 80×40 ≈ 3 200 cells per frame stays well under a
// millisecond on the renderer thread.)

/// Pure helper computing the top inset reserved above the grid for both
/// the OS titlebar band (when an integrated titlebar pushes the content
/// view under the native chrome) and the tab bar. Returns the titlebar
/// inset alone when the tab bar is hidden, so the grid recovers the row
/// the bar used to take. Exposed so tests can validate visibility wiring
/// without needing a live GPU context.
pub fn tab_bar_top_inset(visible: bool, padding: f32) -> f32 {
    tab_bar_top_inset_with_titlebar(visible, padding, 0.0)
}

/// Same as [`tab_bar_top_inset`] but adds a reserved titlebar band on top.
/// `titlebar_inset` is the height in logical pixels the OS reserves at the
/// top of the content view (e.g. macOS traffic-lights strip when
/// `with_fullsize_content_view(true)`). Pass 0 when the OS already keeps
/// our content below its chrome.
pub fn tab_bar_top_inset_with_titlebar(visible: bool, padding: f32, titlebar_inset: f32) -> f32 {
    let bar = if visible { TAB_BAR_HEIGHT + padding } else { padding };
    titlebar_inset + bar
}

/// One inactive pane's cursor: the cell coordinates inside that pane
/// plus the pane's rectangle in window pixels. Carried as a flat
/// struct (rather than a tuple) so the renderer can extend the
/// payload (e.g. with the pane's bg color) without ripple changes.
#[derive(Clone, Debug, PartialEq)]
pub struct InactivePaneCursor {
    pub row: u16,
    pub col: u16,
    pub rect: PaneRect,
}

#[allow(dead_code)]
pub struct GpuRenderer {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: SurfaceConfiguration,
    window: Arc<Window>,

    font_system: FontSystem,
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
    pub cell_w: f32,
    pub cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    padding_top: f32,
    padding_bottom: f32,
    bg: wgpu::Color,
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
    hyperlink_tint: [f32; 4],
    search_highlight: [f32; 4],
    search_highlight_current: [f32; 4],
    search_fg: GColor,
    search_bg: [f32; 4],
    search_buffer: Buffer,
    palette_query_buffer: Buffer,
    palette_rows_buffer: Buffer,
    palette_footer_buffer: Buffer,
    ime_buffer: Buffer,
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
    /// Monotonic counter bumped on theme / default-fg / default-bg
    /// changes. Folded into every `row_hash` so palette swaps
    /// invalidate cached colours without iterating the cache.
    style_rev: u64,
    /// Active drag-chip overlay: translucent rect drawn at the cursor
    /// while a tab is held. Cleared on release.
    drag_chip: Option<DragChipOverlay>,
}

/// Translucent ~120x24 quad drawn at the cursor while a tab is held.
#[derive(Debug, Clone)]
pub struct DragChipOverlay {
    /// Top-left of the chip rect in physical pixels.
    pub top_left: (f32, f32),
    /// Title text of the dragged tab.
    pub title: String,
    /// When `Some`, draw a 2-3px vertical accent bar (the "drop line")
    /// at this logical-pixel X coordinate, spanning the tab bar's
    /// vertical range. This indicates the insertion slot the dragged
    /// tab would land in if released right now. `None` when the cursor
    /// has left the bar Y range (tear-out armed).
    pub drop_line_x: Option<f32>,
    /// Vertical span `(top, bottom)` of the drop-line accent in
    /// logical pixels — matches the tab bar's Y range so the line is
    /// flush with the bar chrome.
    pub drop_line_y: (f32, f32),
    /// Multiplicative scale applied to the chip when rendered, used to
    /// give a subtle 1.0 → 1.02 ease on tear-out arm. `1.0` is the
    /// in-bar resting state; the renderer interpolates around this.
    pub scale: f32,
}

/// Diagnostic snapshot of the most recently rendered drag chip.
/// Production code must not depend on it; tests read it via
/// [`GpuRenderer::last_drag_chip_visual`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragChipVisual {
    pub top_left: (f32, f32),
    pub size: (f32, f32),
}

/// A compact fingerprint of every input that can affect the rendered
/// frame. If two consecutive frames produce an equal key the second one
/// is a no-op for the user, so the renderer skips text shaping, quad
/// rebuild and GPU submission entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameKey {
    grid_revision: u64,
    selection: Option<Selection>,
    cursor_visible: bool,
    tab: u64,
    pane: u64,
    search_hash: u64,
    palette_hash: u64,
    ime_hash: u64,
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
}

impl GpuRenderer {
    pub fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        font_family: &str,
        font_size: f32,
        line_height_mult: f32,
        padding: [f32; 4],
    ) -> Result<Self> {
        pollster::block_on(Self::new_async(
            window,
            event_loop,
            theme,
            font_family,
            font_size,
            line_height_mult,
            padding,
        ))
    }

    async fn new_async(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        font_family: &str,
        font_size: f32,
        line_height_mult: f32,
        padding: [f32; 4],
    ) -> Result<Self> {
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
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: CompositeAlphaMode::Opaque,
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

        let bg = hex_to_wgpu(theme.colors.background.0.as_str());
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
        let ime_metrics = Metrics::new(font_size, font_size * 1.25);
        let mut ime_buffer = Buffer::new(&mut font_system, ime_metrics);
        ime_buffer.set_size(&mut font_system, Some(size.width as f32), Some(font_size * 1.5));
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
            hyperlink_tint,
            search_highlight,
            search_highlight_current,
            search_fg,
            search_bg,
            search_buffer,
            palette_query_buffer,
            palette_rows_buffer,
            palette_footer_buffer,
            ime_buffer,
            drag_chip_buffer,
            drag_chip_visual: None,
            last_frame_key: None,
            skipped_frames: 0,
            tab_bar_visible: true,
            titlebar_inset: 0.0,
            last_missing_chars: Vec::new(),
            shape_cache: ShapeCache::new(),
            row_glyph_cache: crate::row_glyph_cache::RowGlyphCache::new(),
            style_rev: 0,
            drag_chip: None,
        })
    }

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

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// Left padding (logical px). Kept for backward compatibility with
    /// callers that pre-date per-side padding; new code should prefer
    /// the per-side accessors below.
    pub fn padding(&self) -> f32 {
        self.padding_left
    }

    pub fn padding_left(&self) -> f32 {
        self.padding_left
    }
    pub fn padding_right(&self) -> f32 {
        self.padding_right
    }
    pub fn padding_top(&self) -> f32 {
        self.padding_top
    }
    pub fn padding_bottom(&self) -> f32 {
        self.padding_bottom
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
        self.bg = hex_to_wgpu(theme.colors.background.0.as_str());
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

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        grid: &mut Grid,
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        tabs: &TabBar,
        pane_rects: &[(u64, PaneRect)],
        active_pane: u64,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
    ) -> Result<()> {
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
            }
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
                    for t in &layout.tabs {
                        if t.bg.contains(cx, cy) {
                            idx = t.index as u32;
                            if t.close.contains(cx, cy) {
                                on_close = 1;
                            }
                            break;
                        }
                    }
                }
            }
            (idx, on_close)
        };
        let key = FrameKey {
            grid_revision: grid.revision(),
            selection: selection.copied(),
            cursor_visible,
            tab: tabs.active().map(|t| t.id.0).unwrap_or(0),
            pane: active_pane,
            search_hash,
            palette_hash,
            ime_hash,
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
        };
        if Some(key) == self.last_frame_key {
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
        let mut underlines: Vec<(u16, u16, u16)> = Vec::new();
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
        let top_inset = self.top_inset();
        let pad = self.padding_left;
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
            // Resolve which absolute row sits at the top of the rendered
            // viewport. When the user hasn't scrolled (or hasn't scrolled
            // past the visible bottom), this is the live-buffer top, i.e.
            // `scrollback_len()`. Otherwise it's the explicit absolute
            // index requested by the scroll action (e.g. a prompt row).
            let live_top_abs = grid.scrollback_len() as u64;
            let max_top_abs = live_top_abs; // never scroll below live
            let view_top_abs = viewport_top_abs.map(|v| v.min(max_top_abs)).unwrap_or(live_top_abs);
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
                self.row_glyph_cache.invalidate_row_abs(view_top_abs + r as u64);
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
                if let Some(cached) = self.row_glyph_cache.get(row_abs, key) {
                    glyph_instances.extend_from_slice(&cached.glyphs);
                    for (s, e) in &cached.underlines {
                        underlines.push((r, *s, *e));
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
                        underlines.push((r, s, end));
                    }
                }
                if let Some(s) = ul_start.take() {
                    row_underlines.push((s, last_visible_col));
                    underlines.push((r, s, last_visible_col));
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
        }

        let mut quads: Vec<QuadInstance> = Vec::new();
        // Overlay quads — drawn AFTER terminal text + main quads so that
        // palette / search-input / IME backgrounds visually cover the
        // terminal content underneath. (Regression caught in PR #45 review:
        // terminal glyphs were bleeding through overlay dialogs.)
        let mut quads_overlay: Vec<QuadInstance> = Vec::new();

        if let Some(sel) = selection {
            if !sel.is_empty() {
                let (a, b) = sel.normalized();
                for r in a.0..=b.0 {
                    if r >= grid.rows {
                        break;
                    }
                    let col_a = if r == a.0 { a.1 } else { 0 };
                    let col_b = if r == b.0 { b.1 } else { grid.cols.saturating_sub(1) };
                    if col_b < col_a {
                        continue;
                    }
                    let x = self.padding_left + f32::from(col_a) * self.cell_w;
                    let y = self.top_inset() + f32::from(r) * self.cell_h;
                    let w = f32::from(col_b - col_a + 1) * self.cell_w;
                    quads.push(QuadInstance {
                        rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                        color: self.selection_color,
                        ..Default::default()
                    });
                }
            }
        }

        if cursor_visible {
            // Hide the cursor when the viewport is scrolled away from the
            // live region — its absolute row is `scrollback_len + cursor.row`,
            // which sits below the bottom of a scrolled-back view.
            let live_top = grid.scrollback_len() as u64;
            let view_top = viewport_top_abs.map(|v| v.min(live_top)).unwrap_or(live_top);
            if view_top == live_top {
                let cx = self.padding_left + f32::from(grid.cursor.col) * self.cell_w;
                let cy = self.top_inset() + f32::from(grid.cursor.row) * self.cell_h;
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
                            quads.push(QuadInstance {
                                rect: px_to_ndc(cx, cy, self.cell_w, self.cell_h, sw, sh),
                                color,
                                ..Default::default()
                            });
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
                            push_hollow_rect(
                                &mut quads,
                                cx,
                                cy,
                                self.cell_w,
                                self.cell_h,
                                sw,
                                sh,
                                color,
                                2.0,
                            );
                        }
                    }
                    CursorShape::Bar => {
                        quads.push(QuadInstance {
                            rect: px_to_ndc(cx, cy, SUBSHAPE_PX, self.cell_h, sw, sh),
                            color,
                            ..Default::default()
                        });
                    }
                    CursorShape::Underline => {
                        quads.push(QuadInstance {
                            rect: px_to_ndc(
                                cx,
                                cy + self.cell_h - SUBSHAPE_PX,
                                self.cell_w,
                                SUBSHAPE_PX,
                                sw,
                                sh,
                            ),
                            color,
                            ..Default::default()
                        });
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
                // Clamp to the pane rect so a stale cursor position
                // from a pre-resize grid never bleeds onto a sibling.
                if icx + self.cell_w > ic.rect.x + ic.rect.w
                    || icy + self.cell_h > ic.rect.y + ic.rect.h
                {
                    continue;
                }
                push_hollow_rect(
                    &mut quads,
                    icx,
                    icy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    hollow_color,
                    2.0,
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
            let mx = (self.padding_left - marker_w - 1.0).max(0.0);
            let my =
                self.top_inset() + f32::from(row) * self.cell_h + (self.cell_h - marker_h) * 0.5;
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
            let x = self.padding_left + f32::from(*col_a) * self.cell_w;
            let y = self.top_inset() + f32::from(*row) * self.cell_h;
            let w = f32::from(*col_b - *col_a + 1) * self.cell_w;
            quads.push(QuadInstance {
                rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                color: self.hyperlink_tint,
                ..Default::default()
            });
            quads.push(QuadInstance {
                rect: px_to_ndc(x, y + self.cell_h - hl_thickness, w, hl_thickness, sw, sh),
                color: self.hyperlink_underline,
                ..Default::default()
            });
        }

        // Underline quads — drawn last so they appear on top of the text.
        // Color: foreground default at full alpha, linearized so the sRGB
        // surface format doesn't double-encode (matches the body glyph path).
        let underline_color = glyphon_color_to_linear_rgba(self.fg_default);
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        for (row, col_a, col_b) in &underlines {
            let x = self.padding_left + f32::from(*col_a) * self.cell_w;
            let y = self.top_inset() + f32::from(*row) * self.cell_h + self.cell_h
                - underline_thickness;
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

        // -------- Pane split borders ---------------------------------------
        // Each pane in the tab gets a thin border outlining its rectangle so
        // splits are visible; the focused pane gets a brighter, thicker one.
        // v0.3d only renders the active pane's grid (above) inside the full
        // content rect — per-pane glyphon Buffer rendering is v0.4 work.
        if pane_rects.len() > 1 {
            let focus_border = glyphon_color_to_linear_rgba(self.fg_default);
            let border = [focus_border[0] * 0.5, focus_border[1] * 0.5, focus_border[2] * 0.5, 1.0];
            for (id, r) in pane_rects {
                let is_active = *id == active_pane;
                let color = if is_active { focus_border } else { border };
                let t = if is_active { 2.0_f32 } else { 1.0_f32 };
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y, r.w, t, sw, sh),
                    color,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y + r.h - t, r.w, t, sw, sh),
                    color,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y, t, r.h, sw, sh),
                    color,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x + r.w - t, r.y, t, r.h, sw, sh),
                    color,
                    ..Default::default()
                });
            }
        }

        // -------- Tab bar ---------------------------------------------------
        if self.tab_bar_visible {
            let layout = TabBarLayout::compute_with_height(tabs, sw, self.tab_bar_logical_height())
                .with_top_offset(self.titlebar_inset);
            // Issue #112 Round 3 — premium browser-style chrome.
            // The structural colors come from `ui_tokens`, decoupled from
            // the terminal palette so every theme renders the same modern
            // tab bar. The theme.tab.* colors remain authoritative for
            // the title text (active vs inactive fg) so per-theme accents
            // still read through.
            use crate::tabbar_view::{ACTIVE_TOP_ACCENT_H, ACTIVE_TOP_ACCENT_INSET};
            use crate::ui_tokens::color as tok;
            let bar_bg = tok::BG_BASE();
            let active_bg = tok::BG_ELEVATED();
            let hover_bg = tok::BG_HOVER();
            // Theme-driven accent (was hardcoded ACCENT_BLUE — broke gruvbox/etc.).
            let accent_blue = crate::ui_tokens::UiPalette::from_theme(theme).accent;
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
            // Win11-style caption buttons (─ □ ✕) — only painted when the
            // integrated titlebar inset is non-zero (Windows). On macOS the
            // inset is 0 and `paint_caption_buttons` early-returns, so this
            // is a no-op there. See sonic-shared/src/quad.rs::paint_caption_buttons.
            if crate::app::integrated_titlebar_inset_px() > 0 {
                let rects = crate::tabbar_view::caption_button_rects(sw as u32, 1.0);
                crate::quad::paint_caption_buttons(&mut quads, &rects, (sw, sh), bar_bg);
            }
            for t in &layout.tabs {
                let is_active = layout.active == Some(t.index);
                let cursor_on_this_tab = hover_tab_idx == t.index as u32;
                if is_active {
                    // Elevated pill background.
                    // TODO: switch to rounded quad after #116.
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg.x, t.bg.y, t.bg.w, t.bg.h, sw, sh),
                        color: active_bg,
                        ..Default::default()
                    });
                    // 2px top accent bar, ACCENT_BLUE, inset on each side
                    // by ACTIVE_TOP_ACCENT_INSET → width = tab_w - 12.
                    let acc_x = t.bg.x + ACTIVE_TOP_ACCENT_INSET;
                    let acc_w = (t.bg.w - 2.0 * ACTIVE_TOP_ACCENT_INSET).max(0.0);
                    quads.push(QuadInstance {
                        rect: px_to_ndc(acc_x, t.bg.y, acc_w, ACTIVE_TOP_ACCENT_H, sw, sh),
                        color: accent_blue,
                        ..Default::default()
                    });
                } else if cursor_on_this_tab {
                    // Hover overlay on inactive tab — #FFFFFF/6%.
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg.x, t.bg.y, t.bg.w, t.bg.h, sw, sh),
                        color: hover_bg,
                        ..Default::default()
                    });
                }
                // 1px BORDER_SUBTLE separator between adjacent inactive
                // tabs (PR #109 dedup) — height bar_h - 16, centered.
                if t.index + 1 < tabs.tabs().len() {
                    let next_is_active = layout.active == Some(t.index + 1);
                    if !is_active && !next_is_active {
                        let sep_w = 1.0_f32;
                        let sep_h = (layout.bar.h - 16.0).max(1.0);
                        let sep_y = layout.bar.y + (layout.bar.h - sep_h) * 0.5;
                        let gap_mid = t.bg.x + t.bg.w + (TAB_GAP - sep_w) * 0.5;
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
                    let cx = t.close.x;
                    let cy = t.close.y;
                    // 14×14 hit, 8×8 glyph (inset 3px each side).
                    let inset = (t.close.w - 8.0) * 0.5;
                    let glyph = (t.close.w - inset * 2.0).max(1.0);
                    let thick = 1.5_f32;
                    // Diagonal × built from a stair-step of small squares
                    // along both diagonals (the wgpu quad pipeline has no
                    // rotation; PR #117 emitted a horizontal+vertical pair
                    // which read as a `+`, not a close icon — fixed here).
                    build_close_x_quads(
                        cx + inset,
                        cy + inset,
                        glyph,
                        thick,
                        close_color,
                        sw,
                        sh,
                        &mut quads,
                    );
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
                    index: t.index,
                    title: &tabs.tabs()[t.index].title,
                    title_x: t.title.x,
                    title_w: t.title.w,
                    is_active: layout.active == Some(t.index),
                })
                .collect();
            let (title_text, tab_spans) = build_tab_title_spans(
                &tab_inputs,
                avg_glyph_w,
                self.tab_active_fg,
                self.tab_inactive_fg,
            );
            let mut spans2: Vec<(&str, Attrs<'_>)> = Vec::new();
            let mut tcur = 0usize;
            for (range, color) in &tab_spans {
                if range.start > tcur {
                    spans2.push((
                        &title_text[tcur..range.start],
                        terminal_font_attrs(tab_family_name.as_str()).color(self.tab_inactive_fg),
                    ));
                }
                spans2.push((
                    &title_text[range.start..range.end],
                    terminal_font_attrs(tab_family_name.as_str()).color(*color),
                ));
                tcur = range.end;
            }
            if tcur < title_text.len() {
                spans2.push((
                    &title_text[tcur..],
                    terminal_font_attrs(tab_family_name.as_str()).color(self.tab_inactive_fg),
                ));
            }
            self.tab_buffer.set_rich_text(
                &mut self.font_system,
                spans2,
                &terminal_font_attrs(tab_family_name.as_str()).color(self.tab_inactive_fg),
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
                let x = self.padding_left + f32::from(m.col_start) * self.cell_w;
                let y = self.top_inset() + f32::from(visible_row) * self.cell_h;
                let w = f32::from(m.col_end - m.col_start) * self.cell_w;
                let color = if Some(i) == cur_idx {
                    self.search_highlight_current
                } else {
                    self.search_highlight
                };
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                    color,
                    ..Default::default()
                });
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

        // -------- IME preedit overlay --------------------------------------
        let ime_layout = ime.and_then(|i| {
            let cursor_x = self.padding_left + f32::from(grid.cursor.col) * self.cell_w;
            let cursor_y = self.top_inset() + f32::from(grid.cursor.row) * self.cell_h;
            ImePreeditLayout::compute(i, cursor_x, cursor_y, self.cell_w, self.cell_h, sw, sh)
        });
        if let (Some(state), Some(layout)) = (ime, &ime_layout) {
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: [0.10, 0.11, 0.14, 0.95],
                ..Default::default()
            });
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.underline.x,
                    layout.underline.y,
                    layout.underline.w,
                    layout.underline.h,
                    sw,
                    sh,
                ),
                color: self.hyperlink_underline,
                ..Default::default()
            });
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

            // Ghost body — semi-transparent (alpha 0.7 per spec) copy
            // of the active-tab style.
            let mut chip_color = self.tab_active_bg;
            chip_color[3] = 0.7;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x0, y0, w, h, sw, sh),
                color: chip_color,
                ..Default::default()
            });

            // Title text via glyphon: shape into the dedicated
            // drag-chip buffer so it composites on top of the ghost
            // body. Clipping is handled by TextBounds below.
            if !chip.title.is_empty() {
                let attrs = terminal_font_attrs(&self.font_family).color(self.tab_active_fg);
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
            left: layout.query_row.x + 4.0,
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
            // Vertically center the row text within its 40 px background
            // rect (was `row_y + 2` which sat the text at the top of the
            // row with ~18 px of dead space below — the bug from live
            // testing). glyphon places the baseline ~0.8 × font_size
            // below `top`, so centering by line-box height suffices.
            let text_top_offset =
                ((crate::overlays::PALETTE_ROW_HEIGHT - self.font_size) * 0.5).max(0.0);
            Some(TextArea {
                buffer: &self.palette_rows_buffer,
                left: row_x + 4.0,
                top: row_y + text_top_offset,
                scale: 1.0,
                bounds: TextBounds {
                    left: layout.bg.x as i32,
                    top: row_y as i32,
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
        if let Some(a) = ime_area {
            overlay_areas.push(a);
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
        // bitset so the next frame can re-use cached spans for the
        // (likely many) rows that didn't change. clear_dirty does NOT
        // bump grid.revision, so the FrameKey fast-path above still
        // works for truly unchanged frames.
        grid.clear_dirty();
        Ok(())
    }

    /// Shape a single style-run worth of cells and append the
    /// resulting glyph instances + missing-glyph tofus to the frame's
    /// queues. Factored out of the per-row loop so the loop body stays
    /// readable; otherwise it would inline ~80 lines of placement +
    /// fallback handling four times (run start, mid-row flush, end of
    /// row, etc.).
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

/// Convert an sRGB-encoded glyphon color into a linear-space `[r, g, b, a]`
/// suitable for the body text pipeline.
///
/// The body glyph pass writes into a `Bgra8UnormSrgb` surface, which applies
/// linear→sRGB encoding on store. The per-instance `color` therefore MUST be
/// in linear space, mirroring `hex_to_rgba` used by the quad pipeline. Without
/// linearization, raw sRGB bytes (e.g. wezterm foreground `#cfbc97`) are
/// double-encoded on the way to the framebuffer and brighten to `#e9dfca` —
/// the regression that motivated PR #92's follow-up.
///
/// glyphon's own text path is unaffected because cosmic-text + the glyphon
/// atlas swizzle through their own gamma-aware blend (see comment on
/// `hex_to_rgba`).
#[inline]
pub fn glyphon_color_to_linear_rgba(c: GColor) -> [f32; 4] {
    // Use the 256-entry u8 LUT — every input here is already an 8-bit
    // sRGB channel, and the per-glyph hot path called this once per
    // visible cell per frame, paying for two `powf(2.4)` evaluations
    // each time. The LUT collapses each conversion to a single load.
    let t = srgb_u8_to_linear_lut();
    [t[c.r() as usize], t[c.g() as usize], t[c.b() as usize], 1.0]
}

/// Borrow the process-wide sRGB→linear lookup table. Computed once on
/// first use via a `OnceLock`; the table maps each of the 256 possible
/// u8 sRGB channel values to its linear-light counterpart so the
/// per-glyph hot path never has to evaluate `powf(2.4)`.
///
/// Bit-exact with `srgb_channel_to_linear(x as f64 / 255.0) as f32` for
/// every `x in 0..=255` (verified by unit test).
#[inline]
pub fn srgb_u8_to_linear_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0f32; 256];
        let mut i = 0usize;
        while i < 256 {
            t[i] = srgb_channel_to_linear(i as f64 / 255.0) as f32;
            i += 1;
        }
        t
    })
}

fn cell_fg(cell: &Cell, theme: &Theme, default: GColor) -> GColor {
    match cell.fg {
        Color::Default => default,
        Color::Rgb(r, g, b) => GColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
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

/// Convert one sRGB-encoded channel (0..=1) to linear-light space.
///
/// Standard sRGB EOTF (IEC 61966-2-1). Used because our wgpu surface is
/// `Bgra8UnormSrgb`, which performs linear→sRGB encoding on write — colors
/// the shader / clear-color sees must therefore be in linear space, or the
/// gamma is applied twice and the result looks washed out (e.g. Gruvbox Dark
/// Hard `#1d2021` rendering as mid-gray `~#6e6e6e`).
#[doc(hidden)]
pub fn srgb_channel_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Parse a `#rrggbb` hex string into a `wgpu::Color` in **linear** space,
/// suitable for use as a render-pass clear color on an sRGB surface format.
///
/// Alpha is left straight (no gamma curve applies to alpha).
#[doc(hidden)]
pub fn hex_to_wgpu(h: &str) -> wgpu::Color {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    if h.len() == 6 {
        wgpu::Color {
            r: srgb_channel_to_linear(parse(0)),
            g: srgb_channel_to_linear(parse(2)),
            b: srgb_channel_to_linear(parse(4)),
            a: 1.0,
        }
    } else {
        wgpu::Color::BLACK
    }
}

/// Parse a `#rrggbb` hex string + alpha into a `[r, g, b, a]` array in
/// **linear** RGB space, suitable for the quad pipeline which writes into
/// the same `Bgra8UnormSrgb` surface as the clear color above.
///
/// Alpha is passed through unchanged.
///
/// Note: glyphon's text path uses a separate `hex_to_glyphon` helper that
/// returns sRGB-encoded bytes, because glyphon / cosmic-text's atlas
/// expects sRGB input — the wgpu surface format performs the sRGB→linear
/// decode on sample, so glyph colors must NOT be pre-linearized.
#[doc(hidden)]
pub fn hex_to_rgba(h: &str, alpha: f32) -> [f32; 4] {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as usize;
    if h.len() == 6 {
        let t = srgb_u8_to_linear_lut();
        [t[parse(0)], t[parse(2)], t[parse(4)], alpha]
    } else {
        [0.0, 0.0, 0.0, alpha]
    }
}

/// Atlas dimension to allocate for a given DPI scale. On 2× screens we
/// roughly double-stack tiles, so a base 2048² atlas isn't enough room
/// for the same working set. We use `max(2048, base * ceil(scale))` to
/// keep the 1× footprint unchanged while reserving headroom on Retina.
pub fn atlas_dim_for_scale(scale_factor: f32) -> u32 {
    let base = crate::glyph_atlas::ATLAS_DIM;
    let s = scale_factor.max(1.0).ceil() as u32;
    base.saturating_mul(s).max(base)
}

fn measure_cell(fs: &mut FontSystem, family: &str, size: f32, line_h: f32) -> (f32, f32) {
    let mut buf = Buffer::new(fs, Metrics::new(size, line_h));
    buf.set_size(fs, Some(1000.0), Some(1000.0));
    buf.set_text(fs, "M", &terminal_font_attrs(family), Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
    let w =
        buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(size * 0.6);
    (w, line_h)
}

/// Compute the *natural* line height of `family` at `size` (logical px)
/// using the actual font's intrinsic vertical metrics — `ascent`,
/// `descent`, and `leading` (a.k.a. `line_gap`) — pulled from the
/// font's hhea/OS-2 tables via cosmic-text → skrifa.
///
/// This is the value WezTerm multiplies by `line_height` to derive its
/// cell pitch. Sonic prior to this change used `size * line_height`,
/// which silently dropped the font's intrinsic line gap and produced
/// cells that were ~88% of WezTerm's at otherwise-identical config
/// (font_size=14, line_height=1.1 on a typical monospace).
///
/// Falls back to `size` if the font can't be resolved (e.g. user
/// configured a family that isn't installed and we end up on a system
/// fallback that doesn't shape `"M"`).
pub fn natural_line_h_px(fs: &mut FontSystem, family: &str, size: f32) -> f32 {
    let mut buf = Buffer::new(fs, Metrics::new(size, size));
    buf.set_size(fs, Some(1000.0), Some(1000.0));
    buf.set_text(fs, "M", &terminal_font_attrs(family), Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
    let Some(font_id) = buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.font_id))
    else {
        return size;
    };
    // Default weight is fine — we only need vertical metrics, and these
    // are essentially weight-invariant for the families we care about.
    let Some(font) = fs.get_font(font_id, cosmic_text::fontdb::Weight::NORMAL) else {
        return size;
    };
    let m = font.metrics();
    let upem = f32::from(m.units_per_em).max(1.0);
    // skrifa's descent is typically negative (below baseline); leading
    // is the recommended gap between consecutive lines. Sum the
    // magnitudes — this matches the OpenType "ascent + |descent| +
    // line_gap" convention WezTerm uses.
    let natural_units = m.ascent + m.descent.abs() + m.leading;
    let natural_em = natural_units / upem;
    (natural_em * size).max(size)
}

/// Recolor every glyph instance whose center falls inside the cursor
/// cell to `bg_rgba`. Used to produce the wezterm-style "inverted"
/// block cursor: the foreground glyph is painted in the theme
/// background colour so it stays readable on top of the solid
/// cursor accent quad.
///
/// Walks the already-emitted instance list and rewrites their `color`
/// in place. Glyph rectangles are stored in NDC; we invert the
/// [`crate::quad::px_to_ndc`] mapping to test cell containment in
/// pixel space (cleaner than reasoning about NDC sign conventions).
///
/// O(N) over visible glyphs, with N being one frame's instance count.
/// In practice the cursor cell holds one glyph, so this is effectively
/// a single rewrite per frame.
/// Push four thin quad rects forming the outline of `(cell_x, cell_y,
/// cell_w, cell_h)` with thickness `t` in pixels. Used for the
/// unfocused/inactive hollow cursor — the interior stays empty so the
/// glyph underneath remains readable.
#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn push_hollow_rect(
    quads: &mut Vec<QuadInstance>,
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
    t: f32,
) {
    if sw <= 0.0 || sh <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return;
    }
    let t = t.min(cell_w * 0.5).min(cell_h * 0.5);
    // top
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x, cell_y, cell_w, t, sw, sh),
        color,
        ..Default::default()
    });
    // bottom
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x, cell_y + cell_h - t, cell_w, t, sw, sh),
        color,
        ..Default::default()
    });
    // left
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x, cell_y, t, cell_h, sw, sh),
        color,
        ..Default::default()
    });
    // right
    quads.push(QuadInstance {
        rect: px_to_ndc(cell_x + cell_w - t, cell_y, t, cell_h, sw, sh),
        color,
        ..Default::default()
    });
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn recolor_cursor_glyphs(
    glyphs: &mut [crate::text_pipeline::GlyphInstance],
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    bg_rgba: [f32; 4],
) {
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }
    let x_min = cell_x;
    let x_max = cell_x + cell_w;
    let y_min = cell_y;
    let y_max = cell_y + cell_h;
    for g in glyphs.iter_mut() {
        let [gx, gy, gw, gh] = g.rect;
        // Invert px_to_ndc: nx = (x/sw)*2 - 1 → x = (nx + 1) * sw / 2.
        // ny encodes the BOTTOM of the rect (after the +nh shift), so
        // y_top_px = (1 - gy - gh) * sh / 2.
        let px = (gx + 1.0) * sw * 0.5;
        let pw = gw * sw * 0.5;
        let py = (1.0 - gy - gh) * sh * 0.5;
        let ph = gh * sh * 0.5;
        let cx = px + pw * 0.5;
        let cy = py + ph * 0.5;
        if cx >= x_min && cx < x_max && cy >= y_min && cy < y_max {
            g.color = bg_rgba;
        }
    }
}

/// Input describing one tab for [`build_tab_title_spans`]: which slot it
/// occupies, its formatted title, its layout rect's x/width in logical
/// pixels, and whether it is the active tab.
#[doc(hidden)]
pub struct TabSpanInput<'a> {
    pub index: usize,
    pub title: &'a str,
    pub title_x: f32,
    pub title_w: f32,
    pub is_active: bool,
}

/// Build the rich-text title row for the tab bar — one rendered line per
/// frame — assigning each character a colour: gold (`active_fg`) for the
/// active tab's full visible region, dim (`inactive_fg`) for every
/// inactive tab title and every separator. The active tab's region is
/// padded with trailing spaces out to its full title-rect width so the
/// colour span covers every character, not just the leading icon /
/// `#N` digits. Pulled out of the render method so it can be unit-
/// tested without standing up wgpu / cosmic-text.
///
/// Returns `(text, spans)` where each span is `(byte_range, color)`.
/// Bytes between consecutive spans are filled by the caller with
/// `inactive_fg`.
/// Horizontal padding (in logical pixels) reserved on EACH side of a tab's
/// title region before truncation kicks in. 6px on each side = 12px total
/// of breathing room, matching the design polish requirement.
#[doc(hidden)]
pub const TAB_TITLE_PADDING_PX: f32 = 6.0;

/// Tab-title font size given the body terminal font size, in logical px.
/// Tab titles render exactly 1.0 pt larger than the body — see PR
/// "feat(tabbar): centered title with config font, larger size".
/// Picked the additive `+ 1.0` form over `* 1.0625` because it scales
/// consistently across user font-size choices (a hard-coded ratio
/// quickly drifts at extreme sizes: a 10pt body would gain ~0.6pt,
/// a 24pt body ~1.5pt, neither matching the user's intent of "one
/// step up").
#[must_use]
pub fn tab_title_font_size(body_font_size: f32) -> f32 {
    body_font_size + 1.0
}

/// Single source of truth for the [`Attrs`] used by every text-rendering
/// site (terminal grid, tab titles, command palette, search status bar,
/// IME pre-edit). Pass the user-configured `font.family` here so all UI
/// chrome shares the EXACT same `Family::Name(...)` as grid cells —
/// avoiding the historical bug where tab titles silently fell through
/// to `Family::Monospace` and rendered with a different installed face.
#[must_use]
pub fn terminal_font_attrs(family: &str) -> Attrs<'_> {
    Attrs::new().family(Family::Name(family))
}

#[doc(hidden)]
pub fn build_tab_title_spans(
    tabs: &[TabSpanInput<'_>],
    avg_glyph_w: f32,
    active_fg: GColor,
    inactive_fg: GColor,
) -> (String, Vec<(std::ops::Range<usize>, GColor)>) {
    let mut title_text = String::new();
    let mut spans: Vec<(std::ops::Range<usize>, GColor)> = Vec::new();
    for (i, t) in tabs.iter().enumerate() {
        let color = if t.is_active { active_fg } else { inactive_fg };
        // Reserve TAB_TITLE_PADDING_PX on each side before clipping.
        let usable_w = (t.title_w - 2.0 * TAB_TITLE_PADDING_PX).max(avg_glyph_w);
        let max_chars = ((usable_w / avg_glyph_w).floor() as usize).max(1);
        let full_chars = ((t.title_w / avg_glyph_w).floor() as usize).max(max_chars);

        // Truncate with `…` if the title overflows usable width.
        let title_chars: Vec<char> = t.title.chars().collect();
        let body: String = if title_chars.len() > max_chars {
            let keep = max_chars.saturating_sub(1);
            let mut s: String = title_chars.iter().take(keep).collect();
            s.push('…');
            s
        } else {
            title_chars.iter().collect()
        };
        let body_chars = body.chars().count();

        // Centering: text starts at title_x + (title_w - text_w)/2.
        // For ACTIVE tabs the leading & trailing pad spaces stay INSIDE
        // the colored span so the active tint covers the full rect
        // (preserves the pre-centering invariant). For INACTIVE tabs the
        // leading pad is plain prefix space — no need to tint empty cells.
        let text_w = body_chars as f32 * avg_glyph_w;
        let leading_px = t.title_x + ((t.title_w - text_w) / 2.0).max(0.0);
        let rect_left_col = (t.title_x / avg_glyph_w).floor() as usize;
        let center_col = (leading_px / avg_glyph_w).floor() as usize;
        let leading_pad = center_col.saturating_sub(rect_left_col);
        let trailing_pad = full_chars.saturating_sub(body_chars + leading_pad);

        let (anchor_col, raw) = if t.is_active {
            let mut s = String::with_capacity(leading_pad + body.len() + trailing_pad);
            s.extend(std::iter::repeat_n(' ', leading_pad));
            s.push_str(&body);
            s.extend(std::iter::repeat_n(' ', trailing_pad));
            (rect_left_col, s)
        } else {
            (center_col, body)
        };

        while title_text.chars().count() < anchor_col {
            title_text.push(' ');
        }
        // WezTerm-parity separator: the 1px vertical separator between
        // adjacent INACTIVE tabs is painted by the quad pipeline (see
        // the `tab_separator` block in `compute_quads`) — we MUST NOT
        // also inject a `│ ` text glyph here, or the user sees `| │`
        // doubled between every pair of inactive tabs. The quad alone
        // is the source of truth for tab separators.
        let _ = i;
        let start = title_text.len();
        title_text.push_str(&raw);
        let end = title_text.len();
        spans.push((start..end, color));
    }
    (title_text, spans)
}

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
/// `x`, `y` is the top-left of the glyph's bounding box; `glyph` is the
/// side length in physical px; `thick` is the stroke thickness.
///
/// Returns nothing; pushes ~`ceil(glyph/thick)*2` axis-aligned squares
/// stair-stepped along the ╲ and ╱ diagonals. Each emitted quad sits
/// strictly inside `[x, x+glyph] × [y, y+glyph]`.
///
/// The wgpu quad pipeline does not support rotation, and PR #117
/// emitted a horizontal+vertical pair that read as `+` instead of `×`.
/// Stair-stepping a small SDF-free square per step is the
/// simplest and least-invasive fix; visually indistinguishable from a
/// rotated line at 14×14 px.
#[allow(clippy::too_many_arguments)]
pub fn build_close_x_quads(
    x: f32,
    y: f32,
    glyph: f32,
    thick: f32,
    color: [f32; 4],
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
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
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            candidates.push(d.join("assets/fonts"));
            // .app bundle: <exe-dir is MacOS>/.. /Resources/assets/fonts
            if let Some(contents) = d.parent() {
                candidates.push(contents.join("Resources/assets/fonts"));
            }
        }
    }
    candidates.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts"));

    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut n = 0;
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                if let Ok(bytes) = std::fs::read(&p) {
                    fs.db_mut().load_font_data(bytes);
                    n += 1;
                }
            }
        }
        if n > 0 {
            tracing::info!("loaded {n} bundled font(s) from {dir:?}");
            return; // first dir that produced fonts wins
        }
    }
}
