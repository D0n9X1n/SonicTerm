//! GPU renderer for the terminal grid using wgpu 29 + glyphon 0.11.

use std::sync::Arc;

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
    glyph_atlas::{AtlasUpload, GlyphAtlas},
    ime::ImeState,
    overlays::{search_bar_label, ImePreeditLayout, PaletteLayout, SearchBarLayout},
    pane::Rect as PaneRect,
    quad::{px_to_ndc, QuadInstance, QuadPipeline},
    search::SearchState,
    selection::Selection,
    shape::{run_is_ascii_fast, RunStyle, ShapeCache},
    swash_rasterizer::SwashRasterizer,
    tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT},
    tabs::TabBar,
    text_pipeline::{GlyphInstance, TextPipeline},
};

// (Per-row cache + grid SpanDesc removed in the B3 cutover — the GPU
// atlas does an O(1) lookup per cell, so the bookkeeping is wasted
// work. Walking 80×40 ≈ 3 200 cells per frame stays well under a
// millisecond on the renderer thread.)

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
    padding: f32,
    bg: wgpu::Color,
    fg_default: GColor,
    cursor_color: [f32; 4],
    selection_color: [f32; 4],
    tab_bar_bg: [f32; 4],
    tab_active_bg: [f32; 4],
    tab_inactive_bg: [f32; 4],
    tab_active_fg: GColor,
    tab_inactive_fg: GColor,
    tab_close_fg: [f32; 4],
    hyperlink_underline: [f32; 4],
    hyperlink_tint: [f32; 4],
    search_highlight: [f32; 4],
    search_highlight_current: [f32; 4],
    search_fg: GColor,
    search_bg: [f32; 4],
    search_buffer: Buffer,
    palette_query_buffer: Buffer,
    palette_rows_buffer: Buffer,
    ime_buffer: Buffer,
    /// Last rendered frame key — when the next frame would produce an
    /// identical key, render() short-circuits before any GPU work.
    last_frame_key: Option<FrameKey>,
    /// Cumulative count of frames skipped via the FrameKey fast-path.
    /// Exposed via tracing::trace for `RUST_LOG=trace` hit-rate dashboards.
    skipped_frames: u64,
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
}

impl GpuRenderer {
    pub fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        font_family: &str,
        font_size: f32,
        line_height_mult: f32,
        padding: f32,
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
        padding: f32,
    ) -> Result<Self> {
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
        let glyph_atlas =
            GlyphAtlas::new(atlas_dim_for_scale(scale_factor), atlas_dim_for_scale(scale_factor));
        let text_pipeline = TextPipeline::new(&device, format, 4096);
        let glyph_upload =
            AtlasUpload::new(&device, &queue, &glyph_atlas, &text_pipeline.bind_group_layout);

        let line_height = font_size * line_height_mult;
        let metrics = Metrics::new(font_size, line_height);
        // Grid no longer uses a glyphon Buffer — the atlas-backed
        // text_pipeline draws it directly. We still construct
        // `metrics` to share it with measure_cell below.
        let _ = metrics;

        // A second buffer is used for the tab-bar titles. Tab titles use a
        // tighter line height than the terminal grid; one buffer per bar
        // means we only re-shape titles when the tab set changes.
        let tab_metrics = Metrics::new(font_size * 0.85, font_size * 0.85 * 1.2);
        let mut tab_buffer = Buffer::new(&mut font_system, tab_metrics);
        tab_buffer.set_size(&mut font_system, Some(size.width as f32), Some(TAB_BAR_HEIGHT));

        let (cell_w, cell_h) = measure_cell(&mut font_system, font_family, font_size, line_height);

        let bg = hex_to_wgpu(theme.colors.background.0.as_str());
        let fg_default = hex_to_glyphon(theme.colors.foreground.0.as_str());
        let cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.6);
        let selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        let tab_bar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        let tab_active_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        let tab_inactive_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        let tab_active_fg = hex_to_glyphon(theme.colors.tab.active_fg.0.as_str());
        let tab_inactive_fg = hex_to_glyphon(theme.colors.tab.inactive_fg.0.as_str());
        let tab_close_fg = hex_to_rgba(theme.colors.tab.close_button_fg.0.as_str(), 1.0);
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
        let mut palette_rows_buffer = Buffer::new(&mut font_system, palette_metrics);
        palette_rows_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(size.height as f32),
        );
        let ime_metrics = Metrics::new(font_size, font_size * 1.25);
        let mut ime_buffer = Buffer::new(&mut font_system, ime_metrics);
        ime_buffer.set_size(&mut font_system, Some(size.width as f32), Some(font_size * 1.5));

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
            padding,
            bg,
            fg_default,
            cursor_color,
            selection_color,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            tab_active_fg,
            tab_inactive_fg,
            tab_close_fg,
            hyperlink_underline,
            hyperlink_tint,
            search_highlight,
            search_highlight_current,
            search_fg,
            search_bg,
            search_buffer,
            palette_query_buffer,
            palette_rows_buffer,
            ime_buffer,
            last_frame_key: None,
            skipped_frames: 0,
            last_missing_chars: Vec::new(),
            shape_cache: ShapeCache::new(),
            drag_chip: None,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        // Geometry change → force the next frame to actually render.
        self.last_frame_key = None;
        self.tab_buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(TAB_BAR_HEIGHT),
        );
        self.search_buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.font_size * 0.85 * 1.2),
        );
        self.palette_query_buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.font_size * 1.25),
        );
        self.palette_rows_buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.config.height as f32),
        );
        self.ime_buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.font_size * 1.5),
        );
    }

    /// Top inset reserved for the tab bar.
    pub fn top_inset(&self) -> f32 {
        TAB_BAR_HEIGHT + self.padding
    }

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn padding(&self) -> f32 {
        self.padding
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
        let inner_w = (self.config.width as f32 - self.padding * 2.0).max(self.cell_w);
        let inner_h =
            (self.config.height as f32 - self.top_inset() - self.padding).max(self.cell_h);
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

    /// the next `render()` call cannot short-circuit through the
    /// fast-path against a now-stale frame.
    pub fn set_font(&mut self, family: &str, size: f32, line_height_mult: f32) {
        let new_line_h = size * line_height_mult;
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
        self.scale_factor = sf;
        let dim = atlas_dim_for_scale(sf);
        self.glyph_atlas = GlyphAtlas::new(dim, dim);
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
        self.cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.6);
        self.selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        self.tab_bar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        self.tab_active_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        self.tab_inactive_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        self.tab_active_fg = hex_to_glyphon(theme.colors.tab.active_fg.0.as_str());
        self.tab_inactive_fg = hex_to_glyphon(theme.colors.tab.inactive_fg.0.as_str());
        self.tab_close_fg = hex_to_rgba(theme.colors.tab.close_button_fg.0.as_str(), 1.0);
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
        tracing::info!("renderer.set_theme: {}", theme.name);
    }

    pub fn pixel_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let x = px - self.padding;
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
        palette: Option<&CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
    ) -> Result<()> {
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
        };
        if Some(key) == self.last_frame_key {
            self.skipped_frames = self.skipped_frames.wrapping_add(1);
            tracing::trace!(skipped = self.skipped_frames, "renderer: skipped unchanged frame");
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
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        let top_inset = self.top_inset();
        let pad = self.padding;
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
            for r in 0..grid.rows {
                let row_abs = view_top_abs + r as u64;
                let Some(row) = grid.row_at_abs(row_abs) else {
                    continue;
                };
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
                        underlines.push((r, s, end));
                    }
                }
                if let Some(s) = ul_start.take() {
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
                    let x = self.padding + f32::from(col_a) * self.cell_w;
                    let y = self.top_inset() + f32::from(r) * self.cell_h;
                    let w = f32::from(col_b - col_a + 1) * self.cell_w;
                    quads.push(QuadInstance {
                        rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                        color: self.selection_color,
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
                let cx = self.padding + f32::from(grid.cursor.col) * self.cell_w;
                let cy = self.top_inset() + f32::from(grid.cursor.row) * self.cell_h;
                quads.push(QuadInstance {
                    rect: px_to_ndc(cx, cy, self.cell_w, self.cell_h, sw, sh),
                    color: self.cursor_color,
                });
            }
        }

        // OSC 133 shell-integration: draw a small left-edge marker on every
        // row whose absolute position matches a recorded prompt-start. The
        // marker is rendered inside the left padding so it never overlaps
        // text. Color matches the cursor accent at half alpha — distinctive
        // but not noisy.
        let marker_w = (self.padding * 0.35).max(2.0).min(self.cell_w * 0.25);
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
            let mx = (self.padding - marker_w - 1.0).max(0.0);
            let my =
                self.top_inset() + f32::from(row) * self.cell_h + (self.cell_h - marker_h) * 0.5;
            quads.push(QuadInstance {
                rect: px_to_ndc(mx, my, marker_w, marker_h, sw, sh),
                color: marker_color,
            });
        }

        // Hyperlink visuals: a translucent tint quad under the run plus an
        // underline quad on top. Coalesce contiguous hyperlinked cells per
        // row, mirroring the UNDERLINE pass below.
        let hl_runs = collect_hyperlink_runs(grid);
        let hl_thickness = (self.cell_h * 0.08).max(1.0);
        for (row, col_a, col_b) in &hl_runs {
            let x = self.padding + f32::from(*col_a) * self.cell_w;
            let y = self.top_inset() + f32::from(*row) * self.cell_h;
            let w = f32::from(*col_b - *col_a + 1) * self.cell_w;
            quads.push(QuadInstance {
                rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                color: self.hyperlink_tint,
            });
            quads.push(QuadInstance {
                rect: px_to_ndc(x, y + self.cell_h - hl_thickness, w, hl_thickness, sw, sh),
                color: self.hyperlink_underline,
            });
        }

        // Underline quads — drawn last so they appear on top of the text.
        // Color: foreground default at full alpha.
        let underline_color = [
            f32::from(self.fg_default.r()) / 255.0,
            f32::from(self.fg_default.g()) / 255.0,
            f32::from(self.fg_default.b()) / 255.0,
            1.0,
        ];
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        for (row, col_a, col_b) in &underlines {
            let x = self.padding + f32::from(*col_a) * self.cell_w;
            let y = self.top_inset() + f32::from(*row) * self.cell_h + self.cell_h
                - underline_thickness;
            let w = f32::from(*col_b - *col_a + 1) * self.cell_w;
            quads.push(QuadInstance {
                rect: px_to_ndc(x, y, w, underline_thickness, sw, sh),
                color: underline_color,
            });
        }

        // -------- Missing-glyph tofu fallback ------------------------------
        // For cells whose rasterizer returned no tile (and char isn't
        // whitespace), draw a thin outlined rectangle so the gap is
        // visible. Helps catch font-fallback misses (emoji etc.).
        for (x, y, w, h, col) in &missing_tofu {
            let rgba = [
                f32::from(col.r()) / 255.0,
                f32::from(col.g()) / 255.0,
                f32::from(col.b()) / 255.0,
                0.55,
            ];
            let t = 1.0_f32; // border thickness
                             // Top
            quads.push(QuadInstance { rect: px_to_ndc(*x, *y, *w, t, sw, sh), color: rgba });
            // Bottom
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y + *h - t, *w, t, sw, sh),
                color: rgba,
            });
            // Left
            quads.push(QuadInstance { rect: px_to_ndc(*x, *y, t, *h, sw, sh), color: rgba });
            // Right
            quads.push(QuadInstance {
                rect: px_to_ndc(*x + *w - t, *y, t, *h, sw, sh),
                color: rgba,
            });
        }

        // -------- Pane split borders ---------------------------------------
        // Each pane in the tab gets a thin border outlining its rectangle so
        // splits are visible; the focused pane gets a brighter, thicker one.
        // v0.3d only renders the active pane's grid (above) inside the full
        // content rect — per-pane glyphon Buffer rendering is v0.4 work.
        if pane_rects.len() > 1 {
            let border = [
                f32::from(self.fg_default.r()) / 255.0 * 0.5,
                f32::from(self.fg_default.g()) / 255.0 * 0.5,
                f32::from(self.fg_default.b()) / 255.0 * 0.5,
                1.0,
            ];
            let focus_border = [
                f32::from(self.fg_default.r()) / 255.0,
                f32::from(self.fg_default.g()) / 255.0,
                f32::from(self.fg_default.b()) / 255.0,
                1.0,
            ];
            for (id, r) in pane_rects {
                let is_active = *id == active_pane;
                let color = if is_active { focus_border } else { border };
                let t = if is_active { 2.0_f32 } else { 1.0_f32 };
                quads.push(QuadInstance { rect: px_to_ndc(r.x, r.y, r.w, t, sw, sh), color });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y + r.h - t, r.w, t, sw, sh),
                    color,
                });
                quads.push(QuadInstance { rect: px_to_ndc(r.x, r.y, t, r.h, sw, sh), color });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x + r.w - t, r.y, t, r.h, sw, sh),
                    color,
                });
            }
        }

        // -------- Tab bar ---------------------------------------------------
        let layout = TabBarLayout::compute(tabs, sw);
        quads.push(QuadInstance {
            rect: px_to_ndc(layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h, sw, sh),
            color: self.tab_bar_bg,
        });
        for t in &layout.tabs {
            let is_active = layout.active == Some(t.index);
            let bg_color = if is_active { self.tab_active_bg } else { self.tab_inactive_bg };
            quads.push(QuadInstance {
                rect: px_to_ndc(t.bg.x, t.bg.y, t.bg.w, t.bg.h, sw, sh),
                color: bg_color,
            });
            // Close button × drawn as two crossing thin quads.
            let cx = t.close.x;
            let cy = t.close.y;
            let cs = t.close.w;
            let thick = 1.5_f32;
            quads.push(QuadInstance {
                rect: px_to_ndc(
                    cx + cs * 0.25,
                    cy + cs * 0.5 - thick / 2.0,
                    cs * 0.5,
                    thick,
                    sw,
                    sh,
                ),
                color: self.tab_close_fg,
            });
            quads.push(QuadInstance {
                rect: px_to_ndc(
                    cx + cs * 0.5 - thick / 2.0,
                    cy + cs * 0.25,
                    thick,
                    cs * 0.5,
                    sw,
                    sh,
                ),
                color: self.tab_close_fg,
            });
        }
        // `+` new-tab button
        let nt = layout.new_tab;
        let plus_thick = 2.0_f32;
        let plus_len = nt.w.min(nt.h) * 0.4;
        let pcx = nt.x + nt.w / 2.0;
        let pcy = nt.y + nt.h / 2.0;
        quads.push(QuadInstance {
            rect: px_to_ndc(
                pcx - plus_len / 2.0,
                pcy - plus_thick / 2.0,
                plus_len,
                plus_thick,
                sw,
                sh,
            ),
            color: self.tab_close_fg,
        });
        quads.push(QuadInstance {
            rect: px_to_ndc(
                pcx - plus_thick / 2.0,
                pcy - plus_len / 2.0,
                plus_thick,
                plus_len,
                sw,
                sh,
            ),
            color: self.tab_close_fg,
        });

        // Tab titles: render as a single rich-text line where each tab title
        // is positioned by inserting padding spaces. This is approximate but
        // readable; precise per-tab text layout is a v0.4 polish item.
        let avg_glyph_w = (self.cell_w * 0.85).max(1.0);
        let mut title_text = String::new();
        let mut tab_spans: Vec<(std::ops::Range<usize>, GColor)> = Vec::new();
        for t in &layout.tabs {
            let tab = &tabs.tabs()[t.index];
            let is_active = layout.active == Some(t.index);
            let color = if is_active { self.tab_active_fg } else { self.tab_inactive_fg };
            let max_chars = ((t.title.w / avg_glyph_w).floor() as usize).max(1);
            let raw: String = tab.title.chars().take(max_chars).collect();
            let target_col = (t.title.x / avg_glyph_w).floor() as usize;
            while title_text.chars().count() < target_col {
                title_text.push(' ');
            }
            let start = title_text.len();
            title_text.push_str(&raw);
            let end = title_text.len();
            tab_spans.push((start..end, color));
        }
        let mut spans2: Vec<(&str, Attrs<'_>)> = Vec::new();
        let mut tcur = 0usize;
        for (range, color) in &tab_spans {
            if range.start > tcur {
                spans2.push((
                    &title_text[tcur..range.start],
                    Attrs::new().family(Family::Monospace).color(self.tab_inactive_fg),
                ));
            }
            spans2.push((
                &title_text[range.start..range.end],
                Attrs::new().family(Family::Monospace).color(*color),
            ));
            tcur = range.end;
        }
        if tcur < title_text.len() {
            spans2.push((
                &title_text[tcur..],
                Attrs::new().family(Family::Monospace).color(self.tab_inactive_fg),
            ));
        }
        self.tab_buffer.set_rich_text(
            &mut self.font_system,
            spans2,
            &Attrs::new().family(Family::Monospace).color(self.tab_inactive_fg),
            Shaping::Advanced,
            None,
        );
        self.tab_buffer.shape_until_scroll(&mut self.font_system, false);

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
                let x = self.padding + f32::from(m.col_start) * self.cell_w;
                let y = self.top_inset() + f32::from(visible_row) * self.cell_h;
                let w = f32::from(m.col_end - m.col_start) * self.cell_w;
                let color = if Some(i) == cur_idx {
                    self.search_highlight_current
                } else {
                    self.search_highlight
                };
                quads.push(QuadInstance { rect: px_to_ndc(x, y, w, self.cell_h, sw, sh), color });
            }
            // Status bar background pinned to bottom edge.
            search_bar_top = sh - search_bar_h;
            have_search_bar = true;
            quads.push(QuadInstance {
                rect: px_to_ndc(0.0, search_bar_top, sw, search_bar_h, sw, sh),
                color: self.search_bg,
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
                &Attrs::new().family(Family::Monospace).color(self.search_fg),
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
            });
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: self.search_bg,
            });
            let label = search_bar_label(s);
            self.search_buffer.set_text(
                &mut self.font_system,
                &label,
                &Attrs::new().family(Family::Monospace).color(self.search_fg),
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
            // Outer 1px border (accent color).
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
            });
            // Dark modal background.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: [0.08, 0.09, 0.12, 0.96],
            });
            // Selected row highlight.
            if let Some(sel) = layout.selected_row {
                if let Some(row) = layout.rows.get(sel) {
                    quads_overlay.push(QuadInstance {
                        rect: px_to_ndc(row.rect.x, row.rect.y, row.rect.w, row.rect.h, sw, sh),
                        color: self.selection_color,
                    });
                }
            }
            // Shape the query row text.
            self.palette_query_buffer.set_text(
                &mut self.font_system,
                &layout.query_label,
                &Attrs::new().family(Family::Monospace).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.palette_query_buffer.shape_until_scroll(&mut self.font_system, false);

            // Shape the action-list as one multi-line buffer; the renderer
            // positions it at the first row's y and lets glyphon stack
            // lines at the buffer's line height.
            let mut rows_text = String::new();
            for (i, label) in layout.row_labels.iter().enumerate() {
                if i > 0 {
                    rows_text.push('\n');
                }
                rows_text.push_str(label);
            }
            self.palette_rows_buffer.set_text(
                &mut self.font_system,
                &rows_text,
                &Attrs::new().family(Family::Monospace).color(self.search_fg),
                Shaping::Advanced,
                None,
            );
            self.palette_rows_buffer.shape_until_scroll(&mut self.font_system, false);
        }

        // -------- IME preedit overlay --------------------------------------
        let ime_layout = ime.and_then(|i| {
            let cursor_x = self.padding + f32::from(grid.cursor.col) * self.cell_w;
            let cursor_y = self.top_inset() + f32::from(grid.cursor.row) * self.cell_h;
            ImePreeditLayout::compute(i, cursor_x, cursor_y, self.cell_w, self.cell_h, sw, sh)
        });
        if let (Some(state), Some(layout)) = (ime, &ime_layout) {
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: [0.10, 0.11, 0.14, 0.95],
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
            });
            self.ime_buffer.set_text(
                &mut self.font_system,
                state.preedit(),
                &Attrs::new().family(Family::Monospace).color(self.search_fg),
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
            // Subtle drop-shadow, then the chip on top.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    chip.top_left.0 + 2.0,
                    chip.top_left.1 + 2.0,
                    CHIP_W,
                    CHIP_H,
                    sw,
                    sh,
                ),
                color: [0.0, 0.0, 0.0, 0.25],
            });
            let mut chip_color = self.tab_active_bg;
            chip_color[3] = 0.85;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(chip.top_left.0, chip.top_left.1, CHIP_W, CHIP_H, sw, sh),
                color: chip_color,
            });
            // Title is intentionally not drawn here — wiring a new
            // glyphon buffer for the chip would conflict with the
            // existing single-pass text shaping. The translucent quad
            // alone is the v1 visual; title rendering can land as a
            // small follow-up against the overlay text pipeline.
            let _ = chip.title;
        }

        self.viewport.update(
            &self.queue,
            Resolution { width: self.config.width, height: self.config.height },
        );

        let title_top = ((TAB_BAR_HEIGHT - self.font_size * 0.85 * 1.2) / 2.0).max(0.0);
        let tab_area = TextArea {
            buffer: &self.tab_buffer,
            left: 0.0,
            top: title_top,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: self.config.width as i32,
                bottom: TAB_BAR_HEIGHT as i32,
            },
            default_color: self.tab_inactive_fg,
            custom_glyphs: &[],
        };

        let search_area = if have_search_bar {
            Some(TextArea {
                buffer: &self.search_buffer,
                left: self.padding,
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
            layout.rows.first().map(|first| TextArea {
                buffer: &self.palette_rows_buffer,
                left: first.rect.x + 4.0,
                top: first.rect.y + 2.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: layout.bg.x as i32,
                    top: first.rect.y as i32,
                    right: (layout.bg.x + layout.bg.w) as i32,
                    bottom: (layout.bg.y + layout.bg.h) as i32,
                },
                default_color: self.search_fg,
                custom_glyphs: &[],
            })
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
        let mut areas: Vec<TextArea> = vec![tab_area];
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
        if let Some(a) = ime_area {
            overlay_areas.push(a);
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
                let rgba = [
                    f32::from(color.r()) / 255.0,
                    f32::from(color.g()) / 255.0,
                    f32::from(color.b()) / 255.0,
                    1.0,
                ];
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

            // Notdef from the shaper: cosmic-text couldn't resolve the
            // glyph even through its own fallback. Draw a tofu and
            // move on.
            if g.glyph_id == 0 {
                if !lead_cell.ch.is_whitespace() && lead_cell.ch != '\0' {
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
                    missing_chars_this_frame.push(lead_cell.ch);
                }
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
            let gw = info.px_size[0] as f32 * inv_s;
            let gh = info.px_size[1] as f32 * inv_s;
            let color = cell_fg(&lead_cell, theme, fg_default);
            let rgba = [
                f32::from(color.r()) / 255.0,
                f32::from(color.g()) / 255.0,
                f32::from(color.b()) / 255.0,
                1.0,
            ];
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
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    if h.len() == 6 {
        [
            srgb_channel_to_linear(parse(0)) as f32,
            srgb_channel_to_linear(parse(2)) as f32,
            srgb_channel_to_linear(parse(4)) as f32,
            alpha,
        ]
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
    buf.set_text(fs, "M", &Attrs::new().family(Family::Name(family)), Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
    let w =
        buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(size * 0.6);
    (w, line_h)
}

/// Walk the grid and collect runs of contiguous cells that share a hyperlink
/// id, per row. Wide-cell continuations don't break a run (they inherit the
/// lead cell's hyperlink). Returns `(row, col_start, col_end_inclusive)`.
#[doc(hidden)]
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
