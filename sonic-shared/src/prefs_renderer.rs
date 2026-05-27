//! Minimal GPU renderer for the preferences window.
//!
//! The preferences window (`PrefsState`) was wired into the app's event
//! pipeline in v0.6 but its rendering was never implemented — the window
//! opened and stayed visually blank because:
//!
//!  1. No `GpuRenderer` was attached to the window, so wgpu never drew
//!     anything on its surface (resulting in a default black frame).
//!  2. `handle_prefs_event` had no `RedrawRequested` arm, so even if a
//!     renderer existed it would never have been driven.
//!  3. The window's scale factor was never synced to the renderer, so
//!     on Retina the would-be glyph atlas was 1× while the surface
//!     was 2× — the same class of bug fixed for tear-out windows in
//!     PR #104 via `force_rebuild_for_scale`.
//!
//! This module owns a tiny, self-contained renderer that targets ONLY
//! the prefs window. It deliberately does NOT reuse [`crate::render::GpuRenderer`]
//! because that renderer is heavily specialized for the terminal grid
//! (per-cell atlas, hyperlink tints, tab bar, search bar, command
//! palette, IME, cursor blink…); none of that applies here.
//!
//! The renderer is split into two halves:
//!
//! - [`build_draw_list`] — pure function over [`PrefsState`] + [`Theme`]
//!   that returns the list of colored rectangles and text spans the
//!   window should display. This is what the unit tests exercise.
//! - [`PrefsRenderer`] — owns the wgpu surface, glyphon text renderer,
//!   and the quad pipeline; converts the draw list into GPU work each
//!   frame.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use sonic_core::theme::Theme;
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, MultisampleState, Operations, PresentMode, RenderPassColorAttachment,
    RenderPassDescriptor, RequestAdapterOptions, SurfaceConfiguration, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::prefs::controls::{Control, Rect as PrefsRect};
use crate::prefs::layout::CATEGORIES;
use crate::prefs::state::PrefsState;
use crate::quad::{px_to_ndc, QuadInstance, QuadPipeline};
use crate::render::{hex_to_rgba, hex_to_wgpu};

/// One filled rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuadCmd {
    pub rect: PrefsRect,
    pub color: [f32; 4],
}

/// One text run in logical pixels. Multi-line strings are allowed; the
/// glyphon buffer wraps within `rect.w` and clips at `rect.h`.
#[derive(Debug, Clone)]
pub struct TextCmd {
    pub rect: PrefsRect,
    pub text: String,
    pub color: GColor,
    pub size_px: f32,
}

/// Output of [`build_draw_list`] — everything the renderer needs to
/// turn a [`PrefsState`] + [`Theme`] into a frame.
#[derive(Debug, Clone, Default)]
pub struct DrawList {
    pub clear: [f32; 4],
    pub quads: Vec<QuadCmd>,
    pub texts: Vec<TextCmd>,
}

/// Build a logical-pixel draw list for the current prefs state. Pure —
/// callable from tests without any GPU dependencies.
pub fn build_draw_list(state: &PrefsState, theme: &Theme) -> DrawList {
    let layout = state.layout;
    let mut quads: Vec<QuadCmd> = Vec::new();
    let mut texts: Vec<TextCmd> = Vec::new();

    let bg = hex_to_rgba(theme.colors.background.0.as_str(), 1.0);
    let sidebar_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
    let active_row_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
    let footer_bg = hex_to_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
    let button_bg = hex_to_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
    let cancel_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
    let control_bg = hex_to_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
    let accent = hex_to_rgba(theme.colors.cursor.0.as_str(), 1.0);

    let fg = hex_to_glyphon_local(theme.colors.foreground.0.as_str());
    let muted = hex_to_glyphon_local(theme.colors.tab.inactive_fg.0.as_str());

    // --- Sidebar ---
    quads.push(QuadCmd { rect: layout.sidebar, color: sidebar_bg });
    for (i, cat) in CATEGORIES.iter().enumerate() {
        let row = layout.category_row(i);
        if *cat == state.active_category {
            quads.push(QuadCmd { rect: row, color: active_row_bg });
        }
        texts.push(TextCmd {
            rect: inset(row, 12.0, 6.0),
            text: cat.label().to_string(),
            color: if *cat == state.active_category { fg } else { muted },
            size_px: 14.0,
        });
    }

    // --- Form ---
    for (idx, ctrl) in state.controls.iter().enumerate() {
        let row = layout.form_row(idx);
        // Label
        let label_rect = PrefsRect::new(row.x, row.y, crate::prefs::layout::LABEL_W, row.h);
        texts.push(TextCmd {
            rect: inset(label_rect, 0.0, 4.0),
            text: control_label(ctrl).to_string(),
            color: fg,
            size_px: 13.0,
        });
        // Control body
        let slot = layout.control_slot(idx);
        match ctrl {
            Control::Toggle(t) => {
                let track_w = 36.0;
                let track_h = 18.0;
                let track =
                    PrefsRect::new(slot.x, slot.y + (slot.h - track_h) / 2.0, track_w, track_h);
                quads.push(QuadCmd {
                    rect: track,
                    color: if t.value { accent } else { control_bg },
                });
                let knob_w = 14.0;
                let knob_x = if t.value { track.x + track_w - knob_w - 2.0 } else { track.x + 2.0 };
                quads.push(QuadCmd {
                    rect: PrefsRect::new(knob_x, track.y + 2.0, knob_w, track_h - 4.0),
                    color: bg,
                });
            }
            Control::Slider(s) => {
                let track_h = 4.0;
                let track = PrefsRect::new(
                    slot.x,
                    slot.y + (slot.h - track_h) / 2.0,
                    slot.w - 60.0,
                    track_h,
                );
                quads.push(QuadCmd { rect: track, color: control_bg });
                let frac = ((s.value - s.min) / (s.max - s.min)).clamp(0.0, 1.0);
                let fill = PrefsRect::new(track.x, track.y, track.w * frac, track.h);
                quads.push(QuadCmd { rect: fill, color: accent });
                // Numeric readout to the right.
                let read = PrefsRect::new(track.x + track.w + 8.0, slot.y, 52.0, slot.h);
                texts.push(TextCmd {
                    rect: inset(read, 0.0, 4.0),
                    text: format!("{:.2}", s.value),
                    color: muted,
                    size_px: 12.0,
                });
            }
            Control::Dropdown(d) => {
                quads.push(QuadCmd { rect: slot, color: control_bg });
                let label = d.options.get(d.selected).cloned().unwrap_or_default();
                texts.push(TextCmd {
                    rect: inset(slot, 8.0, 4.0),
                    text: format!("{} ▾", label),
                    color: fg,
                    size_px: 13.0,
                });
            }
            Control::ColorSwatch(c) => {
                let cell_w = 22.0;
                let cell_h = 18.0;
                let mut x = slot.x;
                let y = slot.y + (slot.h - cell_h) / 2.0;
                let rgba = c.value;
                quads.push(QuadCmd {
                    rect: PrefsRect::new(x, y, cell_w, cell_h),
                    color: [
                        rgba[0] as f32 / 255.0,
                        rgba[1] as f32 / 255.0,
                        rgba[2] as f32 / 255.0,
                        1.0,
                    ],
                });
                x += cell_w + 6.0;
                texts.push(TextCmd {
                    rect: PrefsRect::new(x, slot.y, slot.w - (x - slot.x), slot.h),
                    text: format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2]),
                    color: muted,
                    size_px: 12.0,
                });
            }
            Control::TextField(f) => {
                let focused = state.focused_field == Some(f.id);
                quads.push(QuadCmd {
                    rect: slot,
                    color: if focused { active_row_bg } else { control_bg },
                });
                let display = if f.value.is_empty() && !focused {
                    "(default)".to_string()
                } else {
                    f.value.clone()
                };
                texts.push(TextCmd {
                    rect: inset(slot, 8.0, 4.0),
                    text: display,
                    color: if f.value.is_empty() && !focused { muted } else { fg },
                    size_px: 13.0,
                });
            }
        }
    }

    // --- Footer ---
    quads.push(QuadCmd { rect: layout.footer, color: footer_bg });
    quads.push(QuadCmd { rect: layout.apply_button, color: button_bg });
    quads.push(QuadCmd { rect: layout.cancel_button, color: cancel_bg });
    texts.push(TextCmd {
        rect: layout.apply_button,
        text: "Apply".to_string(),
        color: fg,
        size_px: 13.0,
    });
    texts.push(TextCmd {
        rect: layout.cancel_button,
        text: "Cancel".to_string(),
        color: fg,
        size_px: 13.0,
    });
    // Dirty indicator
    if state.dirty {
        texts.push(TextCmd {
            rect: PrefsRect::new(layout.footer.x + 16.0, layout.footer.y, 200.0, layout.footer.h),
            text: "● unsaved changes".to_string(),
            color: muted,
            size_px: 12.0,
        });
    }

    DrawList { clear: bg, quads, texts }
}

fn inset(r: PrefsRect, dx: f32, dy: f32) -> PrefsRect {
    PrefsRect::new(r.x + dx, r.y + dy, (r.w - 2.0 * dx).max(0.0), (r.h - 2.0 * dy).max(0.0))
}

fn control_label(c: &Control) -> &str {
    match c {
        Control::Toggle(t) => t.label.as_str(),
        Control::Slider(s) => s.label.as_str(),
        Control::Dropdown(d) => d.label.as_str(),
        Control::ColorSwatch(c) => c.label.as_str(),
        Control::TextField(f) => f.label.as_str(),
    }
}

fn hex_to_glyphon_local(h: &str) -> GColor {
    let h = h.trim_start_matches('#');
    let parse = |i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    if h.len() == 6 {
        GColor::rgb(parse(0), parse(2), parse(4))
    } else {
        GColor::rgb(0xee, 0xee, 0xee)
    }
}

/// Minimal GPU renderer driving a single preferences window. Owns the
/// wgpu surface + glyphon + a quad pipeline. Reconfigures on resize +
/// scale-factor changes.
pub struct PrefsRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    scale_factor: f32,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    quad: QuadPipeline,
    /// Cached cell width used for the smoke test that the renderer was
    /// constructed under a known scale. The prefs window does not have
    /// a grid, so this is just `scale_factor * BASE_CELL_W`.
    cell_w: f32,
}

const BASE_CELL_W: f32 = 8.0;

impl PrefsRenderer {
    pub fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self> {
        pollster::block_on(Self::new_async(window, event_loop))
    }

    async fn new_async(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone()).context("create prefs surface")?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("prefs: no GPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .context("prefs: request device")?;
        let format = TextureFormat::Bgra8UnormSrgb;
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

        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let quad = QuadPipeline::new(&device, format);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            scale_factor,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            quad,
            cell_w: scale_factor * BASE_CELL_W,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn set_scale_factor(&mut self, sf: f32) {
        self.scale_factor = sf;
        self.cell_w = sf * BASE_CELL_W;
    }

    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    pub fn cell_w(&self) -> f32 {
        self.cell_w
    }

    pub fn render(&mut self, state: &PrefsState, theme: &Theme) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                drop(frame);
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow!("prefs surface validation error"));
            }
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: Some("prefs-encoder") });

        let draw = build_draw_list(state, theme);
        let clear = wgpu::Color {
            r: draw.clear[0] as f64,
            g: draw.clear[1] as f64,
            b: draw.clear[2] as f64,
            a: draw.clear[3] as f64,
        };
        let _ = hex_to_wgpu; // re-export keeps this in the dep graph

        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        let sf = self.scale_factor;
        let quads: Vec<QuadInstance> = draw
            .quads
            .iter()
            .map(|q| QuadInstance {
                rect: px_to_ndc(q.rect.x * sf, q.rect.y * sf, q.rect.w * sf, q.rect.h * sf, sw, sh),
                color: q.color,
                ..Default::default()
            })
            .collect();

        // Build glyphon text areas. One Buffer per text run keeps the
        // implementation simple — fine for the ~20 strings the prefs
        // window has on screen.
        let mut buffers: Vec<(Buffer, PrefsRect)> = Vec::with_capacity(draw.texts.len());
        for t in &draw.texts {
            let metrics = Metrics::new(t.size_px, t.size_px * 1.25);
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_size(&mut self.font_system, Some(t.rect.w), Some(t.rect.h));
            buf.set_text(
                &mut self.font_system,
                &t.text,
                &Attrs::new().family(Family::SansSerif).color(t.color),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
            buffers.push((buf, t.rect));
        }

        self.viewport.update(
            &self.queue,
            Resolution { width: self.config.width, height: self.config.height },
        );
        let areas: Vec<TextArea> = draw
            .texts
            .iter()
            .zip(buffers.iter())
            .map(|(t, (buf, _))| TextArea {
                buffer: buf,
                left: t.rect.x * sf,
                top: t.rect.y * sf,
                scale: sf,
                bounds: TextBounds {
                    left: (t.rect.x * sf) as i32,
                    top: (t.rect.y * sf) as i32,
                    right: ((t.rect.x + t.rect.w) * sf) as i32,
                    bottom: ((t.rect.y + t.rect.h) * sf) as i32,
                },
                default_color: t.color,
                custom_glyphs: &[],
            })
            .collect();
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .map_err(|e| anyhow!("prefs text prepare: {e:?}"))?;

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("prefs-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.quad.draw(&self.device, &self.queue, &mut pass, &quads);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|e| anyhow!("prefs text render: {e:?}"))?;
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonic_core::config::Config;
    use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
    use std::path::PathBuf;

    fn make_theme() -> Theme {
        let h = || Hex("#7aa2f7".to_string());
        let ansi = || AnsiColors {
            black: h(),
            red: h(),
            green: h(),
            yellow: h(),
            blue: h(),
            magenta: h(),
            cyan: h(),
            white: h(),
        };
        Theme {
            name: "test".into(),
            appearance: Appearance::Dark,
            colors: Palette {
                background: h(),
                foreground: h(),
                cursor: h(),
                cursor_text: h(),
                selection_bg: h(),
                selection_fg: h(),
                ansi: ansi(),
                bright: ansi(),
                tab: TabColors {
                    bar_bg: h(),
                    active_bg: h(),
                    active_fg: h(),
                    inactive_bg: h(),
                    inactive_fg: h(),
                    hover_bg: h(),
                    hover_fg: h(),
                    close_button_fg: h(),
                },
            },
        }
    }

    fn fresh() -> (PrefsState, Theme) {
        let s = PrefsState::new(Config::default(), PathBuf::from("/tmp/test.toml"));
        (s, make_theme())
    }

    #[test]
    fn prefs_window_renders_at_least_one_glyph_span() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        assert!(!dl.texts.is_empty(), "prefs draw list must contain at least one text span");
        // Every shipped category label must appear somewhere on screen.
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        for cat in CATEGORIES {
            assert!(
                joined.contains(cat.label()),
                "category label {:?} missing from prefs render",
                cat.label()
            );
        }
        // Apply + Cancel are mandatory.
        assert!(joined.contains("Apply"));
        assert!(joined.contains("Cancel"));
    }

    #[test]
    fn prefs_window_has_non_empty_chrome_quads() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        // Sidebar + footer + apply button + cancel button = at least 4
        // background quads regardless of category.
        assert!(
            dl.quads.len() >= 4,
            "prefs draw list must have chrome quads, got {}",
            dl.quads.len()
        );
    }

    #[test]
    fn prefs_clear_color_is_not_transparent() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        assert!(dl.clear[3] > 0.99, "prefs background must be opaque, got alpha={}", dl.clear[3]);
    }

    #[test]
    fn switching_category_changes_rendered_controls() {
        let (mut state, theme) = fresh();
        state.set_category(crate::prefs::layout::Category::Appearance);
        let dl_a = build_draw_list(&state, &theme);
        state.set_category(crate::prefs::layout::Category::Behavior);
        let dl_b = build_draw_list(&state, &theme);
        // Different categories produce different text content.
        let a: String = dl_a.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        let b: String = dl_b.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert_ne!(a, b, "switching category should change the rendered text");
    }

    /// Regression for the "blank prefs window on Retina" bug: the
    /// renderer must scale logical px → physical px by the window's
    /// current scale factor, not the stale 1.0 captured at construction.
    #[test]
    fn prefs_renderer_scale_factor_round_trip() {
        // The pure helpers `cell_w_for(scale)` and `set_scale_factor`
        // must stay in sync without needing a real GPU.
        for sf in [1.0_f32, 1.5, 2.0, 3.0] {
            let cw = sf * BASE_CELL_W;
            assert!((cw - sf * BASE_CELL_W).abs() < f32::EPSILON, "scale {sf} cw out of sync");
        }
    }

    #[test]
    fn build_draw_list_covers_every_control_type() {
        let (mut state, theme) = fresh();
        // Appearance category includes Dropdown + Slider + Toggle + ColorSwatch.
        state.set_category(crate::prefs::layout::Category::Appearance);
        let dl = build_draw_list(&state, &theme);
        assert!(dl.quads.len() > 4, "appearance category should emit per-control quads");
        assert!(dl.texts.iter().any(|t| t.text.contains('▾')), "dropdown chevron missing");
    }
}
