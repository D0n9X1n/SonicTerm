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
    pane::Rect as PaneRect,
    quad::{px_to_ndc, QuadInstance, QuadPipeline},
    search::SearchState,
    selection::Selection,
    tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT},
    tabs::TabBar,
};

/// Internal: a contiguous run of cells that share text-attributes.
struct SpanDesc {
    range: std::ops::Range<usize>,
    fg: GColor,
    weight: glyphon::Weight,
    italic: bool,
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
    buffer: Buffer,
    tab_buffer: Buffer,
    quad: QuadPipeline,

    font_size: f32,
    line_height: f32,
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
    search_fg: GColor,
    search_bg: [f32; 4],
    search_buffer: Buffer,
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
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let quad = QuadPipeline::new(&device, format);

        let line_height = font_size * line_height_mult;
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_size(&mut font_system, Some(size.width as f32), Some(size.height as f32));

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
        let search_fg = hex_to_glyphon(theme.colors.foreground.0.as_str());
        let search_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        let search_metrics = Metrics::new(font_size * 0.85, font_size * 0.85 * 1.2);
        let mut search_buffer = Buffer::new(&mut font_system, search_metrics);
        search_buffer.set_size(
            &mut font_system,
            Some(size.width as f32),
            Some(font_size * 0.85 * 1.2),
        );

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
            buffer,
            tab_buffer,
            quad,
            font_size,
            line_height,
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
            search_fg,
            search_bg,
            search_buffer,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        self.buffer.set_size(
            &mut self.font_system,
            Some(self.config.width as f32),
            Some(self.config.height as f32),
        );
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

    pub fn cells(&self) -> (u16, u16) {
        let inner_w = (self.config.width as f32 - self.padding * 2.0).max(self.cell_w);
        let inner_h =
            (self.config.height as f32 - self.top_inset() - self.padding).max(self.cell_h);
        let cols = (inner_w / self.cell_w).floor() as u16;
        let rows = (inner_h / self.cell_h).floor() as u16;
        (cols.max(1), rows.max(1))
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
        grid: &Grid,
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        tabs: &TabBar,
        pane_rects: &[(u64, PaneRect)],
        active_pane: u64,
        search: Option<&SearchState>,
    ) -> Result<()> {
        // Walk the grid building (text, spans, underline cells) together.
        let mut text = String::with_capacity((grid.cols as usize + 1) * grid.rows as usize);
        // span_descriptors records (byte_range, fg, bg, weight, italic) so we
        // can build &str + Attrs pairs for set_rich_text after the String is
        // fully materialised (so &str borrows are stable).
        let mut span_descriptors: Vec<SpanDesc> = Vec::new();
        // Underline cells in (row, col_start, col_end_inclusive) form so we
        // can draw a quad pass beneath them after the text is laid out.
        let mut underlines: Vec<(u16, u16, u16)> = Vec::new();

        let fg_default = self.fg_default;
        for r in 0..grid.rows {
            let row = grid.row(r);
            // Open run state — flushed whenever a cell with different attrs
            // appears (or at end of row).
            let mut run_start_byte = text.len();
            let mut run_fg = fg_default;
            let mut run_weight = glyphon::Weight::NORMAL;
            let mut run_italic = false;
            let mut run_has_chars = false;
            // Underline run for this row.
            let mut ul_start: Option<u16> = None;
            let mut last_visible_col: u16 = 0;

            for (col, cell) in row.iter().enumerate() {
                if cell.flags.contains(CellFlags::WIDE_CONT) {
                    continue;
                }
                let cell_fg = cell_fg(cell, theme, fg_default);
                let cell_weight = if cell.flags.contains(CellFlags::BOLD) {
                    glyphon::Weight::BOLD
                } else {
                    glyphon::Weight::NORMAL
                };
                let cell_italic = cell.flags.contains(CellFlags::ITALIC);

                if run_has_chars
                    && (cell_fg != run_fg || cell_weight != run_weight || cell_italic != run_italic)
                {
                    span_descriptors.push(SpanDesc {
                        range: run_start_byte..text.len(),
                        fg: run_fg,
                        weight: run_weight,
                        italic: run_italic,
                    });
                    run_start_byte = text.len();
                    run_fg = cell_fg;
                    run_weight = cell_weight;
                    run_italic = cell_italic;
                    run_has_chars = false;
                }
                if !run_has_chars {
                    run_fg = cell_fg;
                    run_weight = cell_weight;
                    run_italic = cell_italic;
                }
                text.push(cell.ch);
                run_has_chars = true;
                last_visible_col = col as u16;

                // Underline tracking — coalesce contiguous underlined cells
                if cell.flags.contains(CellFlags::UNDERLINE) {
                    if ul_start.is_none() {
                        ul_start = Some(col as u16);
                    }
                } else if let Some(s) = ul_start.take() {
                    underlines.push((r, s, last_visible_col.saturating_sub(1)));
                }
            }
            // Flush trailing underline run on this row
            if let Some(s) = ul_start.take() {
                underlines.push((r, s, last_visible_col));
            }
            // Flush the row's last attr run before pushing \n
            if run_has_chars {
                span_descriptors.push(SpanDesc {
                    range: run_start_byte..text.len(),
                    fg: run_fg,
                    weight: run_weight,
                    italic: run_italic,
                });
            }
            text.push('\n');
        }

        // Now that `text` is stable, build the spans iterator. The newlines
        // between rows get a default-attrs single-char span so byte indices
        // stay aligned.
        let mut spans: Vec<(&str, Attrs<'_>)> = Vec::new();
        let mut cursor_byte: usize = 0;
        for d in &span_descriptors {
            // Emit any newlines between previous and this span's start
            if d.range.start > cursor_byte {
                spans.push((
                    &text[cursor_byte..d.range.start],
                    Attrs::new().family(Family::Monospace).color(fg_default),
                ));
            }
            let mut a = Attrs::new().family(Family::Monospace).color(d.fg).weight(d.weight);
            if d.italic {
                a = a.style(glyphon::Style::Italic);
            }
            spans.push((&text[d.range.start..d.range.end], a));
            cursor_byte = d.range.end;
        }
        if cursor_byte < text.len() {
            spans.push((
                &text[cursor_byte..],
                Attrs::new().family(Family::Monospace).color(fg_default),
            ));
        }

        self.buffer.set_rich_text(
            &mut self.font_system,
            spans,
            &Attrs::new().family(Family::Monospace).color(fg_default),
            Shaping::Advanced,
            None,
        );
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let mut quads: Vec<QuadInstance> = Vec::new();
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;

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
            let cx = self.padding + f32::from(grid.cursor.col) * self.cell_w;
            let cy = self.top_inset() + f32::from(grid.cursor.row) * self.cell_h;
            quads.push(QuadInstance {
                rect: px_to_ndc(cx, cy, self.cell_w, self.cell_h, sw, sh),
                color: self.cursor_color,
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
            for m in &s.matches {
                if m.row >= grid.rows || m.col_end <= m.col_start {
                    continue;
                }
                let x = self.padding + f32::from(m.col_start) * self.cell_w;
                let y = self.top_inset() + f32::from(m.row) * self.cell_h;
                let w = f32::from(m.col_end - m.col_start) * self.cell_w;
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                    color: self.search_highlight,
                });
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

        self.viewport.update(
            &self.queue,
            Resolution { width: self.config.width, height: self.config.height },
        );

        let area = TextArea {
            buffer: &self.buffer,
            left: self.padding,
            top: self.top_inset(),
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: TAB_BAR_HEIGHT as i32,
                right: self.config.width as i32,
                bottom: self.config.height as i32,
            },
            default_color: self.fg_default,
            custom_glyphs: &[],
        };
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

        let mut areas: Vec<TextArea> = vec![area, tab_area];
        if let Some(a) = search_area {
            areas.push(a);
        }

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
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
            self.text_renderer.render(&self.atlas, &self.viewport, &mut pass)?;
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.atlas.trim();
        Ok(())
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

fn hex_to_wgpu(h: &str) -> wgpu::Color {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    if h.len() == 6 {
        wgpu::Color { r: parse(0), g: parse(2), b: parse(4), a: 1.0 }
    } else {
        wgpu::Color::BLACK
    }
}

fn hex_to_rgba(h: &str, alpha: f32) -> [f32; 4] {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    if h.len() == 6 {
        [parse(0), parse(2), parse(4), alpha]
    } else {
        [0.0, 0.0, 0.0, alpha]
    }
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
