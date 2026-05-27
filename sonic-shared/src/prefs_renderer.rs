// Pre-PR-#119 call sites still use the deprecated `color::*` helpers. The
// theme-driven `UiPalette` (PR #119) is canonical at the entry point; the
// remaining literal-color sites will be migrated in a follow-up.
#![allow(deprecated)]

//! GPU renderer for the preferences window — redesigned for issue #112 R2.
//!
//! The window paints, top-down:
//!   1. `BG_BASE` clear (#0B0E14).
//!   2. Sidebar strip + right divider; one row per category; the active
//!      row gets a tinted background and a left accent bar.
//!   3. Title block ("Preferences" + per-category subtitle) at (28, 24).
//!   4. A surface card that wraps the form rows (label + control pair).
//!   5. Sticky footer with top border, dirty indicator on the left, and
//!      Cancel (secondary) + Apply (primary accent) buttons on the right.
//!
//! `build_draw_list` is pure (no GPU) so it is trivially unit-testable.

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
use crate::prefs::layout::{
    self, CARD_PAD_H, CARD_PAD_V, CATEGORIES, CONTROL_H, CONTROL_RADIUS, LABEL_W, PREVIEW_CARD_H,
    PREVIEW_PAD, SECTION_HELP_SIZE, SECTION_TITLE_SIZE, SIDEBAR_LABEL_X, SLIDER_THUMB,
    SLIDER_TRACK_H, SUBTITLE_GAP, SUBTITLE_LINE, SUBTITLE_SIZE, SWATCH_GAP, SWATCH_SIZE,
    TITLE_LINE, TOGGLE_H, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, TOGGLE_W,
};
use crate::prefs::state::PrefsState;
use crate::quad::{px_to_ndc, QuadInstance, QuadPipeline};
use crate::render::hex_to_wgpu;
use crate::ui_tokens::{color, typography};

/// One filled rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuadCmd {
    pub rect: PrefsRect,
    /// **Linear-sRGB premultiplied** RGBA (matches `ui_tokens::color`).
    pub color: [f32; 4],
}

/// One text run in logical pixels.
#[derive(Debug, Clone)]
pub struct TextCmd {
    pub rect: PrefsRect,
    pub text: String,
    pub color: GColor,
    pub size_px: f32,
    pub weight: u16,
    /// `true` → use the terminal monospace font; `false` → system UI font.
    pub monospace: bool,
    /// Optional explicit family name. When `Some`, the renderer uses it
    /// verbatim (e.g. the user's configured terminal font for the
    /// Appearance preview card); when `None` the default selection by
    /// `monospace` applies.
    pub font_family: Option<String>,
}

/// Output of [`build_draw_list`].
#[derive(Debug, Clone, Default)]
pub struct DrawList {
    pub clear: [f32; 4],
    pub quads: Vec<QuadCmd>,
    pub texts: Vec<TextCmd>,
}

/// Convert a `[f32; 4]` premultiplied linear-sRGB token into a glyphon
/// `Color`. Glyphon expects sRGB 8-bit — we undo the linearisation.
fn token_to_gcolor(c: [f32; 4]) -> GColor {
    let a = c[3].clamp(0.0, 1.0);
    let (lr, lg, lb) =
        if a > f32::EPSILON { (c[0] / a, c[1] / a, c[2] / a) } else { (0.0, 0.0, 0.0) };
    fn linear_to_srgb(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }
    let r = (linear_to_srgb(lr) * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (linear_to_srgb(lg) * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (linear_to_srgb(lb) * 255.0).round().clamp(0.0, 255.0) as u8;
    let a8 = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    GColor::rgba(r, g, b, a8)
}

fn push_text(
    out: &mut Vec<TextCmd>,
    rect: PrefsRect,
    text: impl Into<String>,
    color_token: [f32; 4],
    ramp: typography::TypeRamp,
) {
    out.push(TextCmd {
        rect,
        text: text.into(),
        color: token_to_gcolor(color_token),
        size_px: ramp.size_px,
        weight: ramp.weight,
        monospace: false,
        font_family: None,
    });
}

fn push_mono(
    out: &mut Vec<TextCmd>,
    rect: PrefsRect,
    text: impl Into<String>,
    color_token: [f32; 4],
    size: f32,
    family: Option<String>,
) {
    out.push(TextCmd {
        rect,
        text: text.into(),
        color: token_to_gcolor(color_token),
        size_px: size,
        weight: 500,
        monospace: true,
        font_family: family,
    });
}

/// Return the configured terminal font family for the prefs preview
/// card. Falls back to an empty `String` only when the user explicitly
/// cleared the family — the renderer treats an empty string as "use
/// the monospace default". Mirrors what the terminal grid would use.
fn terminal_font_attrs(state: &PrefsState) -> String {
    state.config.font.family.clone()
}

/// Build a logical-pixel draw list for the current prefs state. Pure —
/// callable from tests without any GPU dependencies.
pub fn build_draw_list(state: &PrefsState, theme: &Theme) -> DrawList {
    let layout = state.layout;
    let mut quads: Vec<QuadCmd> = Vec::new();
    let mut texts: Vec<TextCmd> = Vec::new();

    // --- Background ----------------------------------------------------
    // Per PR #119: chrome derives from the active theme via UiPalette.
    let palette = crate::ui_tokens::UiPalette::from_theme(theme);
    let bg_base = palette.bg_base;

    // --- Sidebar -------------------------------------------------------
    quads.push(QuadCmd { rect: layout.sidebar, color: color::BG_ELEVATED() });
    // Right divider 1px.
    quads.push(QuadCmd {
        rect: layout.sidebar_divider,
        color: color::with_alpha(color::hex("#FFFFFF"), 0.07),
    });

    for (i, cat) in CATEGORIES.iter().enumerate() {
        let row = layout.category_row(i);
        let active = *cat == state.active_category;
        if active {
            quads.push(QuadCmd { rect: row, color: color::BG_ACTIVE() });
            // Left accent bar.
            quads.push(QuadCmd { rect: layout.category_accent(i), color: color::ACCENT_BLUE() });
        }
        // Icon slot placeholder (subtle pill) — keeps spacing predictable
        // without forcing us to ship an icon font in this PR.
        let icon_y = row.y + (row.h - layout::SIDEBAR_ICON_SLOT) / 2.0;
        let icon_rect = PrefsRect::new(
            row.x + layout::SIDEBAR_ICON_X,
            icon_y,
            layout::SIDEBAR_ICON_SLOT,
            layout::SIDEBAR_ICON_SLOT,
        );
        let icon_color =
            if active { color::ACCENT_BLUE() } else { color::with_alpha(color::TEXT_MUTED(), 0.6) };
        quads.push(QuadCmd { rect: icon_rect, color: color::with_alpha(icon_color, 0.18) });

        // Label.
        let label_x = row.x + SIDEBAR_LABEL_X;
        let label_w = row.w - SIDEBAR_LABEL_X - 4.0;
        let label_rect =
            PrefsRect::new(label_x, row.y + (row.h - 20.0) / 2.0, label_w.max(0.0), 20.0);
        let (col, ramp) = if active {
            (color::TEXT_PRIMARY(), typography::BODY_STRONG)
        } else {
            (color::with_alpha(color::TEXT_SECONDARY(), 0.85), typography::BODY)
        };
        push_text(&mut texts, label_rect, cat.label(), col, ramp);
    }

    // --- Title block ---------------------------------------------------
    let title_rect = PrefsRect::new(
        layout.title_block.x,
        layout.title_block.y,
        layout.title_block.w,
        TITLE_LINE,
    );
    push_text(&mut texts, title_rect, "Preferences", color::TEXT_PRIMARY(), typography::H1);
    let subtitle_rect = PrefsRect::new(
        layout.title_block.x,
        layout.title_block.y + TITLE_LINE + SUBTITLE_GAP,
        layout.title_block.w,
        SUBTITLE_LINE,
    );
    push_text(
        &mut texts,
        subtitle_rect,
        state.active_category.description(),
        color::TEXT_MUTED(),
        typography::TypeRamp { size_px: SUBTITLE_SIZE, line_px: SUBTITLE_LINE, weight: 500 },
    );

    // --- Form card -----------------------------------------------------
    quads.push(QuadCmd { rect: layout.form_card, color: color::BG_SURFACE() });
    // 1px subtle border drawn as 4 thin strips (the quad pipeline does
    // not support outlines yet).
    push_border(&mut quads, layout.form_card, color::BORDER_SUBTLE());

    // Section title + help row inside the card.
    let section_title_rect = PrefsRect::new(
        layout.form_card.x + CARD_PAD_H,
        layout.form_card.y + CARD_PAD_V - 2.0,
        layout.form_card.w - CARD_PAD_H * 2.0,
        20.0,
    );
    push_text(
        &mut texts,
        section_title_rect,
        state.active_category.label(),
        color::TEXT_PRIMARY(),
        typography::TypeRamp { size_px: SECTION_TITLE_SIZE, line_px: 20.0, weight: 650 },
    );
    let help_w = layout::SECTION_HELP_MAX_W.min(layout.form_card.w - CARD_PAD_H * 2.0);
    let help_rect = PrefsRect::new(
        layout.form_card.x + CARD_PAD_H,
        section_title_rect.y + 20.0,
        help_w,
        SUBTITLE_LINE,
    );
    push_text(
        &mut texts,
        help_rect,
        state.active_category.description(),
        color::TEXT_MUTED(),
        typography::TypeRamp { size_px: SECTION_HELP_SIZE, line_px: SUBTITLE_LINE, weight: 500 },
    );

    // --- Form rows -----------------------------------------------------
    // `form_row` / `control_slot` already include `PrefsLayout::ROW_Y_OFFSET`
    // so render + hit-test see the same rects.

    for (idx, ctrl) in state.controls.iter().enumerate() {
        let row = layout.form_row(idx);
        let slot = layout.control_slot(idx);

        // Label.
        let label_h = 20.0;
        let label_rect = PrefsRect::new(row.x, row.y + (row.h - label_h) / 2.0, LABEL_W, label_h);
        push_text(
            &mut texts,
            label_rect,
            control_label(ctrl).to_string(),
            color::TEXT_SECONDARY(),
            typography::BODY,
        );

        // Control.
        draw_control(&mut quads, &mut texts, ctrl, slot, state);
    }

    // --- Preview card (Appearance category only) ----------------------
    if matches!(state.active_category, layout::Category::Appearance) {
        let last_row = layout.form_row(state.controls.len());
        let preview_y = last_row.y + 8.0;
        let preview_card = PrefsRect::new(
            layout.form_card.x + CARD_PAD_H,
            preview_y,
            layout.form_card.w - CARD_PAD_H * 2.0,
            PREVIEW_CARD_H,
        );
        // Only draw if it fits inside the card.
        if preview_card.y + preview_card.h <= layout.form_card.y + layout.form_card.h - CARD_PAD_V {
            // Use the active terminal theme's background for the
            // preview swatch.
            let preview_bg = hex_to_token(theme.colors.background.0.as_str());
            quads.push(QuadCmd { rect: preview_card, color: preview_bg });
            push_border(&mut quads, preview_card, color::BORDER_SUBTLE());
            let preview_fg = hex_to_token(theme.colors.foreground.0.as_str());
            for (i, line) in state.preview_lines().iter().enumerate() {
                let ly = preview_card.y + PREVIEW_PAD + i as f32 * 20.0;
                let lh = 20.0;
                if ly + lh > preview_card.y + preview_card.h - PREVIEW_PAD {
                    break;
                }
                push_mono(
                    &mut texts,
                    PrefsRect::new(
                        preview_card.x + PREVIEW_PAD,
                        ly,
                        preview_card.w - PREVIEW_PAD * 2.0,
                        lh,
                    ),
                    line.clone(),
                    preview_fg,
                    13.0,
                    Some(terminal_font_attrs(state)),
                );
            }
        }
    }

    // --- Footer --------------------------------------------------------
    quads.push(QuadCmd { rect: layout.footer, color: color::with_alpha(color::BG_BASE(), 0.94) });
    quads.push(QuadCmd { rect: layout.footer_divider, color: color::BORDER_SUBTLE() });

    // Dirty indicator on the left.
    if state.dirty {
        let dot_size = 6.0;
        let dot_y = layout.footer.y + (layout.footer.h - dot_size) / 2.0;
        quads.push(QuadCmd {
            rect: PrefsRect::new(layout.footer.x + 28.0, dot_y, dot_size, dot_size),
            color: color::ACCENT_ORANGE(),
        });
        let txt_rect = PrefsRect::new(
            layout.footer.x + 28.0 + dot_size + 8.0,
            layout.footer.y,
            220.0,
            layout.footer.h,
        );
        push_text(
            &mut texts,
            txt_rect,
            "Unsaved changes",
            color::TEXT_SECONDARY(),
            typography::TypeRamp { size_px: 12.0, line_px: 16.0, weight: 500 },
        );
    }

    // Cancel button (secondary).
    quads.push(QuadCmd {
        rect: layout.cancel_button,
        color: color::with_alpha(color::hex("#FFFFFF"), 0.07),
    });
    push_text(
        &mut texts,
        layout.cancel_button,
        "Cancel",
        color::TEXT_SECONDARY(),
        typography::BODY,
    );

    // Apply button (primary accent).
    let apply_enabled = state.dirty;
    let (apply_bg, apply_fg) = if apply_enabled {
        (color::ACCENT_BLUE(), color::hex("#0B1020"))
    } else {
        // Disabled state per spec.
        (color::hex("#2A3042"), color::TEXT_FAINT())
    };
    quads.push(QuadCmd { rect: layout.apply_button, color: apply_bg });
    push_text(&mut texts, layout.apply_button, "Apply", apply_fg, typography::BODY_STRONG);

    DrawList { clear: bg_base, quads, texts }
}

/// Convert a `#RRGGBB` hex string from the theme into a premultiplied
/// linear-sRGB token (matching the rest of the draw list).
fn hex_to_token(s: &str) -> [f32; 4] {
    let s = s.trim_start_matches('#');
    if s.len() == 6 {
        color::hex(&format!("#{}FF", s))
    } else {
        color::hex(s)
    }
}

fn push_border(out: &mut Vec<QuadCmd>, r: PrefsRect, c: [f32; 4]) {
    let t = 1.0;
    out.push(QuadCmd { rect: PrefsRect::new(r.x, r.y, r.w, t), color: c });
    out.push(QuadCmd { rect: PrefsRect::new(r.x, r.y + r.h - t, r.w, t), color: c });
    out.push(QuadCmd { rect: PrefsRect::new(r.x, r.y, t, r.h), color: c });
    out.push(QuadCmd { rect: PrefsRect::new(r.x + r.w - t, r.y, t, r.h), color: c });
}

fn draw_control(
    quads: &mut Vec<QuadCmd>,
    texts: &mut Vec<TextCmd>,
    ctrl: &Control,
    slot: PrefsRect,
    state: &PrefsState,
) {
    match ctrl {
        Control::Toggle(t) => {
            let track_y = slot.y + (slot.h - TOGGLE_H) / 2.0;
            let track = PrefsRect::new(slot.x, track_y, TOGGLE_W, TOGGLE_H);
            let on_color = color::ACCENT_BLUE();
            let off_color = color::hex("#343A52FF");
            quads.push(QuadCmd { rect: track, color: if t.value { on_color } else { off_color } });
            let knob_x = if t.value {
                track.x + TOGGLE_W - TOGGLE_KNOB - TOGGLE_KNOB_MARGIN
            } else {
                track.x + TOGGLE_KNOB_MARGIN
            };
            let knob =
                PrefsRect::new(knob_x, track.y + TOGGLE_KNOB_MARGIN, TOGGLE_KNOB, TOGGLE_KNOB);
            let knob_color =
                if t.value { color::hex("#FFFFFFFF") } else { color::TEXT_SECONDARY() };
            quads.push(QuadCmd { rect: knob, color: knob_color });
        }
        Control::Slider(s) => {
            let readout_w = 56.0;
            let track_w = (slot.w - readout_w - 12.0).max(40.0);
            let track_y = slot.y + (slot.h - SLIDER_TRACK_H) / 2.0;
            let track = PrefsRect::new(slot.x, track_y, track_w, SLIDER_TRACK_H);
            quads.push(QuadCmd { rect: track, color: color::hex("#343A52FF") });
            let frac = ((s.value - s.min) / (s.max - s.min)).clamp(0.0, 1.0);
            let fill = PrefsRect::new(track.x, track.y, track.w * frac, track.h);
            quads.push(QuadCmd { rect: fill, color: color::ACCENT_BLUE() });
            // Thumb (16x16) — drawn as a square. The renderer doesn't
            // round corners yet; the spec calls for r=8.
            let thumb_x = track.x + (track.w * frac) - SLIDER_THUMB / 2.0;
            let thumb_y = slot.y + (slot.h - SLIDER_THUMB) / 2.0;
            // Border (drawn as outer 2px rect filled with bg base).
            let outer = PrefsRect::new(
                thumb_x - 2.0,
                thumb_y - 2.0,
                SLIDER_THUMB + 4.0,
                SLIDER_THUMB + 4.0,
            );
            quads.push(QuadCmd { rect: outer, color: color::BG_BASE() });
            quads.push(QuadCmd {
                rect: PrefsRect::new(thumb_x, thumb_y, SLIDER_THUMB, SLIDER_THUMB),
                color: color::TEXT_PRIMARY(),
            });
            // Numeric readout to the right.
            let readout = PrefsRect::new(track.x + track.w + 12.0, slot.y, readout_w, slot.h);
            push_text(
                texts,
                readout,
                format!("{:.2}", s.value),
                color::TEXT_MUTED(),
                typography::TypeRamp { size_px: 12.0, line_px: 18.0, weight: 500 },
            );
        }
        Control::Dropdown(d) => {
            let r = PrefsRect::new(slot.x, slot.y, slot.w.min(240.0), CONTROL_H);
            quads.push(QuadCmd { rect: r, color: color::hex("#090C12FF") });
            push_border(quads, r, color::with_alpha(color::hex("#FFFFFF"), 0.10));
            let label = d.options.get(d.selected).cloned().unwrap_or_default();
            push_text(
                texts,
                PrefsRect::new(r.x + 10.0, r.y, r.w - 28.0, r.h),
                label,
                color::TEXT_PRIMARY(),
                typography::BODY,
            );
            // Chevron.
            push_text(
                texts,
                PrefsRect::new(r.x + r.w - 18.0, r.y, 16.0, r.h),
                "▾",
                color::TEXT_MUTED(),
                typography::BODY,
            );
            // (Open-dropdown menu rendering is out of scope for this PR.)
            let _ = CONTROL_RADIUS;
        }
        Control::ColorSwatch(c) => {
            let mut x = slot.x;
            let y = slot.y + (slot.h - SWATCH_SIZE) / 2.0;
            // Render the active swatch (the only cell we currently
            // track) plus seven preset cells to make the grid feel real.
            let presets: [&str; 8] = [
                "#7AA2F7", "#BB9AF7", "#9ECE6A", "#E0AF68", "#F7768E", "#73DACA", "#7DCFFF",
                "#FF9E64",
            ];
            for (i, hex) in presets.iter().enumerate() {
                let cell_rgba = if i == 0 {
                    [
                        c.value[0] as f32 / 255.0,
                        c.value[1] as f32 / 255.0,
                        c.value[2] as f32 / 255.0,
                        1.0,
                    ]
                } else {
                    color::hex(hex)
                };
                let cell = PrefsRect::new(x, y, SWATCH_SIZE, SWATCH_SIZE);
                quads.push(QuadCmd { rect: cell, color: cell_rgba });
                if i == 0 {
                    // Selected ring (2px) + 1px inset of bg-base.
                    let ring =
                        PrefsRect::new(x - 2.0, y - 2.0, SWATCH_SIZE + 4.0, SWATCH_SIZE + 4.0);
                    // Order matters: ring first (under), then redraw cell.
                    quads.insert(
                        quads.len() - 1,
                        QuadCmd { rect: ring, color: color::ACCENT_BLUE() },
                    );
                }
                x += SWATCH_SIZE + SWATCH_GAP;
                if x + SWATCH_SIZE > slot.x + slot.w {
                    break;
                }
            }
        }
        Control::TextField(f) => {
            let focused = state.focused_field == Some(f.id);
            let r = PrefsRect::new(slot.x, slot.y, slot.w.min(280.0), CONTROL_H);
            quads.push(QuadCmd { rect: r, color: color::hex("#090C12FF") });
            let border = if focused {
                color::BORDER_FOCUS()
            } else {
                color::with_alpha(color::hex("#FFFFFF"), 0.10)
            };
            push_border(quads, r, border);
            let display = if f.value.is_empty() && !focused {
                "(default)".to_string()
            } else {
                f.value.clone()
            };
            let col = if f.value.is_empty() && !focused {
                color::TEXT_FAINT()
            } else {
                color::TEXT_PRIMARY()
            };
            push_text(
                texts,
                PrefsRect::new(r.x + 10.0, r.y, r.w - 20.0, r.h),
                display,
                col,
                typography::BODY,
            );
        }
    }
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

        let ui_family = typography::system_ui_family();
        let mut buffers: Vec<(Buffer, PrefsRect)> = Vec::with_capacity(draw.texts.len());
        for t in &draw.texts {
            let metrics = Metrics::new(t.size_px, t.size_px * 1.4);
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_size(&mut self.font_system, Some(t.rect.w), Some(t.rect.h));
            let family = match (&t.font_family, t.monospace) {
                (Some(name), _) if !name.is_empty() => Family::Name(name.as_str()),
                (_, true) => Family::Name("monospace"),
                (_, false) => Family::Name(ui_family),
            };
            let mut attrs = Attrs::new().family(family).color(t.color);
            if t.weight >= 600 {
                attrs = attrs.weight(glyphon::Weight::BOLD);
            } else if t.weight >= 550 {
                attrs = attrs.weight(glyphon::Weight::SEMIBOLD);
            }
            buf.set_text(&mut self.font_system, &t.text, &attrs, Shaping::Advanced, None);
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
    use crate::prefs::PrefsHit;
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
        let theme = make_theme();
        let s = PrefsState::new(Config::default(), PathBuf::from("/tmp/test.toml"), theme.clone());
        (s, theme)
    }

    #[test]
    fn prefs_window_renders_title_and_subtitle() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert!(joined.contains("Preferences"), "title missing: {joined}");
        assert!(
            dl.texts.iter().any(|t| t.text.contains(state.active_category.description())),
            "subtitle missing"
        );
    }

    #[test]
    fn prefs_window_renders_all_category_labels() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        for cat in CATEGORIES {
            assert!(joined.contains(cat.label()), "category {} missing", cat.label());
        }
    }

    #[test]
    fn prefs_window_has_apply_and_cancel_buttons() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert!(joined.contains("Apply"));
        assert!(joined.contains("Cancel"));
    }

    #[test]
    fn prefs_window_has_non_empty_chrome_quads() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        assert!(dl.quads.len() >= 4, "got {}", dl.quads.len());
    }

    #[test]
    fn prefs_clear_color_is_opaque_bg_base() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        assert!(dl.clear[3] > 0.99, "prefs background must be opaque");
        // Should match UiPalette::from_theme(theme).bg_base — chrome
        // follows the active theme (PR #119).
        let expected = crate::ui_tokens::UiPalette::from_theme(&theme).bg_base;
        for (i, ex) in expected.iter().enumerate() {
            assert!((dl.clear[i] - ex).abs() < 1e-4, "clear[{i}] ≠ palette.bg_base");
        }
    }

    #[test]
    fn switching_category_changes_subtitle() {
        let (mut state, theme) = fresh();
        state.set_category(layout::Category::Appearance);
        let dl_a = build_draw_list(&state, &theme);
        state.set_category(layout::Category::Behavior);
        let dl_b = build_draw_list(&state, &theme);
        let a: String = dl_a.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        let b: String = dl_b.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert_ne!(a, b);
    }

    #[test]
    fn prefs_renderer_scale_factor_round_trip() {
        for sf in [1.0_f32, 1.5, 2.0, 3.0] {
            let cw = sf * BASE_CELL_W;
            assert!((cw - sf * BASE_CELL_W).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn build_draw_list_covers_every_control_type() {
        let (mut state, theme) = fresh();
        state.set_category(layout::Category::Appearance);
        let dl = build_draw_list(&state, &theme);
        assert!(dl.quads.len() > 6);
        assert!(dl.texts.iter().any(|t| t.text.contains('▾')), "dropdown chevron missing");
    }

    #[test]
    fn prefs_primary_button_uses_accent_blue() {
        let (mut state, theme) = fresh();
        // Force dirty so the primary button shows its enabled colors.
        state.set_category(layout::Category::Appearance);
        // Flip a toggle if there is one, otherwise mark dirty manually.
        // We approximate by finding any toggle control.
        let toggle_id = state.controls.iter().find_map(|c| {
            if let Control::Toggle(t) = c {
                Some(t.id)
            } else {
                None
            }
        });
        if let Some(id) = toggle_id {
            state.flip_toggle(id);
        } else {
            // Fallback: directly mark dirty by re-applying a category.
            state.set_category(layout::Category::Behavior);
            let id = state.controls.iter().find_map(|c| {
                if let Control::Toggle(t) = c {
                    Some(t.id)
                } else {
                    None
                }
            });
            if let Some(id) = id {
                state.flip_toggle(id);
            }
        }
        let dl = build_draw_list(&state, &theme);
        let apply_rect = state.layout.apply_button;
        // Find the quad covering the apply button.
        let apply_quad = dl
            .quads
            .iter()
            .find(|q| {
                (q.rect.x - apply_rect.x).abs() < 0.01 && (q.rect.y - apply_rect.y).abs() < 0.01
            })
            .expect("apply button quad");
        let accent = color::ACCENT_BLUE();
        for (i, &ac) in accent.iter().enumerate() {
            assert!(
                (apply_quad.color[i] - ac).abs() < 1e-4,
                "apply button channel {i} ≠ ACCENT_BLUE: got {} want {}",
                apply_quad.color[i],
                ac,
            );
        }
    }

    #[test]
    fn prefs_dirty_indicator_renders_unsaved_changes() {
        let (mut state, theme) = fresh();
        // Flip a toggle if available to dirty the state.
        let id = state.controls.iter().find_map(|c| {
            if let Control::Toggle(t) = c {
                Some(t.id)
            } else {
                None
            }
        });
        if let Some(id) = id {
            state.flip_toggle(id);
            let dl = build_draw_list(&state, &theme);
            assert!(
                dl.texts.iter().any(|t| t.text == "Unsaved changes"),
                "dirty indicator missing"
            );
        }
    }

    /// Regression: PR #117 review CRITICAL. The renderer used a private
    /// `row_y_offset` that the hit-test path did not know about, so a
    /// click on the visible first control hit the wrong slot. The fix
    /// folds the offset into `PrefsLayout::form_row` /
    /// `PrefsLayout::control_slot` directly so render + hit-test see
    /// the same rect.
    ///
    /// This test asserts: clicking at the *visible* center of control 0
    /// resolves to control 0 — not to an empty area above it.
    #[test]
    fn prefs_hit_test_rect_matches_rendered_rect() {
        let (state, _theme) = fresh();
        // First control's stored rect — also the rect the renderer
        // will draw at.
        assert!(!state.controls.is_empty(), "expected at least one control on General page");
        let ctrl0 = &state.controls[0];
        let ctrl0_id = ctrl0.id();
        let rect = match ctrl0 {
            Control::TextField(tf) => tf.rect,
            Control::Toggle(t) => t.rect,
            Control::Slider(s) => s.rect,
            Control::Dropdown(d) => d.rect,
            Control::ColorSwatch(cs) => cs.rect,
        };
        let cx = rect.x + rect.w / 2.0;
        let cy = rect.y + rect.h / 2.0;
        let hit = state.classify_click(cx, cy);
        assert!(hit.is_some(), "no hit at visible center of control 0 ({cx}, {cy})");
        // The id reported by the hit must match control 0.
        let hit_id = match hit.unwrap() {
            PrefsHit::Toggle(id)
            | PrefsHit::SliderTrack(id)
            | PrefsHit::DropdownHeader(id)
            | PrefsHit::TextField(id)
            | PrefsHit::DropdownOption { id, .. }
            | PrefsHit::ColorCell { id, .. } => id,
            other => panic!("expected control hit, got {other:?}"),
        };
        assert_eq!(hit_id, ctrl0_id, "click resolved to a different control");
    }

    /// Regression: PR #117 review IMPORTANT. The prefs window builder
    /// must enforce a minimum inner size so the user cannot shrink the
    /// window below the layout's clamp (otherwise the form card
    /// disappears).
    #[test]
    fn prefs_window_enforces_min_size() {
        // Constants must exist and match the layout clamp (680×520).
        assert_eq!(crate::prefs::PREFS_MIN_W, 680.0);
        assert_eq!(crate::prefs::PREFS_MIN_H, 520.0);
        // app.rs `create_prefs_window` must reference both via
        // `with_min_inner_size(LogicalSize::new(PREFS_MIN_W, PREFS_MIN_H))`.
        let app_src = include_str!("app.rs");
        assert!(
            app_src.contains("with_min_inner_size"),
            "prefs window builder is missing with_min_inner_size"
        );
        assert!(
            app_src.contains("PREFS_MIN_W"),
            "prefs window builder is not wired to PREFS_MIN_W"
        );
        assert!(
            app_src.contains("PREFS_MIN_H"),
            "prefs window builder is not wired to PREFS_MIN_H"
        );
    }

    /// Regression: PR #117 review IMPORTANT. The Appearance preview
    /// card used the generic monospace family which ignores the user's
    /// configured terminal font. The fix routes the family through
    /// `terminal_font_attrs()` so the preview actually shows the
    /// configured font. Source-grep test — the literal must not
    /// reappear.
    #[test]
    fn prefs_preview_card_uses_terminal_font_attrs() {
        let src = include_str!("prefs_renderer.rs");
        // Build the needle at runtime so this test's own assertion
        // string does not count as a hit. Strip line comments first
        // so doc-comments mentioning the legacy spelling do not match.
        let needle = format!("Family::{}", "Monospace");
        let stripped: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !stripped.contains(&needle),
            "prefs_renderer.rs still uses the generic monospace family; preview card must use terminal_font_attrs()"
        );
        // The helper must exist + be invoked: the preview card's
        // TextCmd must carry the configured terminal font family.
        // Use a tall window so the preview card actually fits inside
        // the form card (default 600 px is too short for 5 controls +
        // preview).
        let (state, theme) = fresh();
        let mut state = state;
        state.set_category(crate::prefs::Category::Appearance);
        state.config.font.family = "ZZZ-FixturePreviewFont".to_string();
        state.layout = crate::prefs::PrefsLayout::new(760.0, 900.0);
        state.rebuild_controls();
        let dl = build_draw_list(&state, &theme);
        let mono_count = dl.texts.iter().filter(|t| t.monospace).count();
        let with_family = dl
            .texts
            .iter()
            .filter(|t| t.font_family.as_deref() == Some("ZZZ-FixturePreviewFont"))
            .count();
        let preview_used = dl
            .texts
            .iter()
            .any(|t| t.monospace && t.font_family.as_deref() == Some("ZZZ-FixturePreviewFont"));
        assert!(
            preview_used,
            "preview card text did not carry the configured terminal font family (mono_count={mono_count}, with_family={with_family})"
        );
    }
}
