//! GPU renderer for the terminal grid using wgpu + glyphon.
//!
//! Each frame we:
//! 1. Walk the [`Grid`] producing the text buffer.
//! 2. Clear the surface with the theme background.
//! 3. Draw selection highlight + cursor as quads.
//! 4. Draw the buffer via `glyphon::TextRenderer`.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use glyphon::{
    Attrs, Buffer, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer,
};
use sonic_core::{
    grid::{Cell, CellFlags, Color, Grid},
    theme::Theme,
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Features, Instance,
    InstanceDescriptor, Limits, LoadOp, MultisampleState, Operations, PresentMode,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, SurfaceConfiguration,
    TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::window::Window;

use crate::{
    quad::{px_to_ndc, QuadInstance, QuadPipeline},
    selection::Selection,
};

/// Owns every GPU resource. Built once per window.
#[allow(dead_code)] // cell_fg/indexed used when per-cell color spans land (v0.3b)
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: SurfaceConfiguration,

    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    buffer: Buffer,
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
}

impl GpuRenderer {
    pub fn new(
        window: Arc<Window>,
        theme: &Theme,
        font_family: &str,
        font_size: f32,
        line_height_mult: f32,
        padding: f32,
    ) -> Result<Self> {
        pollster::block_on(Self::new_async(
            window,
            theme,
            font_family,
            font_size,
            line_height_mult,
            padding,
        ))
    }

    async fn new_async(
        window: Arc<Window>,
        theme: &Theme,
        font_family: &str,
        font_size: f32,
        line_height_mult: f32,
        padding: f32,
    ) -> Result<Self> {
        let size = window.inner_size();
        let instance = Instance::new(InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).context("create surface")?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow!("no suitable GPU adapter"))?;
        let (device, queue) = adapter
            .request_device(
                &DeviceDescriptor {
                    label: Some("sonic-device"),
                    required_features: Features::empty(),
                    required_limits: Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .context("request device")?;

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
        let mut atlas = TextAtlas::new(&device, &queue, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let quad = QuadPipeline::new(&device, format);

        let line_height = font_size * line_height_mult;
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_size(&mut font_system, size.width as f32, size.height as f32);

        let (cell_w, cell_h) = measure_cell(&mut font_system, font_family, font_size, line_height);

        let bg = hex_to_wgpu(theme.colors.background.0.as_str());
        let fg_default = hex_to_glyphon(theme.colors.foreground.0.as_str());
        let cursor_color = hex_to_rgba(theme.colors.cursor.0.as_str(), 0.6);
        let selection_color = hex_to_rgba(theme.colors.selection_bg.0.as_str(), 0.5);

        Ok(Self {
            device,
            queue,
            surface,
            config,
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            buffer,
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
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        self.buffer.set_size(
            &mut self.font_system,
            self.config.width as f32,
            self.config.height as f32,
        );
    }

    /// How many (cols, rows) of cells fit in the current surface.
    pub fn cells(&self) -> (u16, u16) {
        let inner_w = (self.config.width as f32 - self.padding * 2.0).max(self.cell_w);
        let inner_h = (self.config.height as f32 - self.padding * 2.0).max(self.cell_h);
        let cols = (inner_w / self.cell_w).floor() as u16;
        let rows = (inner_h / self.cell_h).floor() as u16;
        (cols.max(1), rows.max(1))
    }

    /// Convert pixel coordinates relative to the window to a grid cell.
    /// Returns `None` if the point is outside the text area.
    pub fn pixel_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let x = px - self.padding;
        let y = py - self.padding;
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

    /// Draw one frame: text + cursor + optional selection highlight.
    pub fn render(
        &mut self,
        grid: &Grid,
        _theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
    ) -> Result<()> {
        // ---- text buffer ----
        let mut text = String::with_capacity((grid.cols as usize + 1) * grid.rows as usize);
        for r in 0..grid.rows {
            for cell in grid.row(r).iter() {
                if cell.flags.contains(CellFlags::WIDE_CONT) {
                    continue;
                }
                text.push(cell.ch);
            }
            text.push('\n');
        }

        self.buffer.set_text(
            &mut self.font_system,
            &text,
            Attrs::new().family(Family::Monospace).color(self.fg_default),
            Shaping::Advanced,
        );
        self.buffer.shape_until_scroll(&mut self.font_system);

        // ---- quads (selection then cursor) ----
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
                    let y = self.padding + f32::from(r) * self.cell_h;
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
            let cy = self.padding + f32::from(grid.cursor.row) * self.cell_h;
            quads.push(QuadInstance {
                rect: px_to_ndc(cx, cy, self.cell_w, self.cell_h, sw, sh),
                color: self.cursor_color,
            });
        }

        // ---- prepare text ----
        let area = TextArea {
            buffer: &self.buffer,
            left: self.padding,
            top: self.padding,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: self.config.width as i32,
                bottom: self.config.height as i32,
            },
            default_color: self.fg_default,
        };
        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            Resolution { width: self.config.width, height: self.config.height },
            [area],
            &mut self.swash_cache,
        )?;

        // ---- submit ----
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder =
            self.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("sonic") });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("sonic-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(self.bg), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.quad.draw(&self.device, &self.queue, &mut pass, &quads);
            self.text_renderer.render(&self.atlas, &mut pass)?;
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.atlas.trim();
        Ok(())
    }
}

#[allow(dead_code)]
fn cell_fg(cell: &Cell, theme: &Theme, default: GColor) -> GColor {
    match cell.fg {
        Color::Default => default,
        Color::Rgb(r, g, b) => GColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
    }
}

#[allow(dead_code)]
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
        _ => None, // 16..=255 palette deferred
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
    buf.set_size(fs, 1000.0, 1000.0);
    buf.set_text(fs, "M", Attrs::new().family(Family::Name(family)), Shaping::Advanced);
    buf.shape_until_scroll(fs);
    let w =
        buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(size * 0.6);
    (w, line_h)
}
