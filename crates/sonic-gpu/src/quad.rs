//! Minimal wgpu quad pipeline. Draws axis-aligned colored rectangles for
//! the cursor and selection highlight, in normalized device coordinates.
//!
//! Each instance is a `rect` in NDC plus an RGBA color. The pipeline also
//! supports a per-instance SDF rounded-rect cutoff: when `radius_px > 0`
//! the fragment shader computes a signed distance against the rounded
//! interior and smoothsteps the edge for 1 px of AA. `size_px` is the
//! rectangle's size in **physical pixels** (needed because NDC alone has
//! no notion of "1 pixel"). For sharp rectangles set `radius_px = 0` and
//! `size_px = [0, 0]` and the shader skips the SDF math.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
pub struct QuadInstance {
    pub rect: [f32; 4], // x, y, w, h in NDC ([-1,1])
    pub color: [f32; 4],
    /// Rectangle width/height in physical pixels (used by the SDF path).
    /// `[0.0, 0.0]` (the default) keeps the sharp-rect path.
    pub size_px: [f32; 2],
    /// Corner radius in physical pixels. `0.0` (default) skips the SDF
    /// rounded-rect path and the quad renders sharp like before.
    pub radius_px: f32,
    /// Padding so the layout stays 16-byte aligned for WGSL `vec4` ergonomics.
    pub _pad: f32,
}

impl Default for QuadInstance {
    fn default() -> Self {
        Self { rect: [0.0; 4], color: [0.0; 4], size_px: [0.0; 2], radius_px: 0.0, _pad: 0.0 }
    }
}

impl QuadInstance {
    /// Sharp-edged rectangle (the legacy default).
    #[must_use]
    pub fn sharp(rect: [f32; 4], color: [f32; 4]) -> Self {
        Self { rect, color, ..Default::default() }
    }

    /// Rounded rectangle in physical pixels. `size_px` is the rect's size
    /// in physical pixels (must match the NDC `rect` size).
    #[must_use]
    pub fn rounded(rect: [f32; 4], color: [f32; 4], size_px: [f32; 2], radius_px: f32) -> Self {
        Self { rect, color, size_px, radius_px, _pad: 0.0 }
    }
}

pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_buf: wgpu::Buffer,
    capacity: u64,
}

const SHADER: &str = r#"
struct Instance {
    @location(0) rect:    vec4<f32>,
    @location(1) color:   vec4<f32>,
    @location(2) params:  vec4<f32>, // size_px.x, size_px.y, radius_px, pad
}

struct VsOut {
    @builtin(position) pos:    vec4<f32>,
    @location(0)        color: vec4<f32>,
    @location(1)        local: vec2<f32>, // pixel offset from rect center
    @location(2)        params: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, inst: Instance) -> VsOut {
    // Triangle-strip unit quad: (0,0)(1,0)(0,1)(1,1)
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vid];
    let ndc = vec2<f32>(inst.rect.x + c.x * inst.rect.z,
                        inst.rect.y + c.y * inst.rect.w);
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = inst.color;
    // Local coord in pixels, from the rect center, used for the SDF path.
    let size = inst.params.xy;
    out.local = (c - vec2<f32>(0.5, 0.5)) * size;
    out.params = inst.params;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = in.params.z;
    if (r <= 0.0) {
        return in.color;
    }
    let half_size = in.params.xy * 0.5;
    // Signed distance to a rounded rect centered at origin.
    let q = abs(in.local) - (half_size - vec2<f32>(r, r));
    let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
    // 1-pixel antialias band: alpha = 1 inside, 0 outside, smooth in between.
    let w = fwidth(d);
    let aa = 1.0 - smoothstep(-w, w, d);
    return vec4<f32>(in.color.rgb, in.color.a * aa);
}
"#;

impl QuadPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sonic-quad-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sonic-quad-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sonic-quad-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4,
                        1 => Float32x4,
                        2 => Float32x4
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 64;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-quad-instances"),
            size: capacity * std::mem::size_of::<QuadInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, instance_buf, capacity }
    }

    pub fn draw<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
        instances: &[QuadInstance],
    ) {
        if instances.is_empty() {
            return;
        }
        if instances.len() as u64 > self.capacity {
            self.capacity = (instances.len() as u64).next_power_of_two();
            self.instance_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sonic-quad-instances"),
                contents: bytemuck::cast_slice(instances),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
        } else {
            queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.instance_buf.slice(..));
        pass.draw(0..4, 0..instances.len() as u32);
    }
}

/// Convert a pixel rect to NDC (Y-flipped: pixel y=0 is top, NDC y=1 is top).
pub fn px_to_ndc(x: f32, y: f32, w: f32, h: f32, sw: f32, sh: f32) -> [f32; 4] {
    let nx = (x / sw) * 2.0 - 1.0;
    let ny = 1.0 - (y / sh) * 2.0 - (h / sh) * 2.0;
    let nw = (w / sw) * 2.0;
    let nh = (h / sh) * 2.0;
    [nx, ny, nw, nh]
}

/// Hover state for the caption-button strip. `None` when no caption
/// button is under the cursor. Encoded as an index `0/1/2 = min/max/close`
/// so callers don't have to depend on `sonic-ui` from the GPU crate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CaptionHover {
    #[default]
    None,
    Min,
    Max,
    Close,
}

/// Theme colors used by [`paint_caption_buttons`]. Kept as a small POD
/// struct so the caller (renderer) can derive them from its theme/UI
/// tokens once and pass them through without sonic-gpu needing to know
/// about themes.
#[derive(Copy, Clone, Debug)]
pub struct CaptionColors {
    /// Strip background (no-hover state) — usually the same as the bar bg.
    pub bg: [f32; 4],
    /// Hover-plate color for minimize / maximize.
    pub hover_bg: [f32; 4],
    /// Hover-plate color for close (the canonical Win11 red `#E81123`).
    pub close_hover_bg: [f32; 4],
    /// Foreground glyph color (─ □ ✕) when not hovered.
    pub fg: [f32; 4],
    /// Foreground glyph color while the close button is hovered (white
    /// on top of the red plate for legibility).
    pub close_hover_fg: [f32; 4],
}

/// Paint the three Win11-style caption buttons (min / max / close) into
/// the given quad list — full visuals: hover-state background plate +
/// minimal geometric glyph for each button. The glyphs are drawn as
/// thin axis-aligned and rotated quads instead of going through the
/// text pipeline, both to avoid a font dependency for chrome-only
/// symbols and so that the icons render even before the glyph atlas
/// has warmed up.
///
/// Callers on platforms without an integrated titlebar inset (macOS /
/// Linux) should early-return without ever invoking this helper — the
/// function itself is portable but the caption strip only exists on
/// Windows. The single existing caller (`sonic-shared::render`) already
/// gates on `app::integrated_titlebar_inset_px() > 0`.
///
/// `rects` is `[min, max, close]` as `(x, y, w, h)` in physical pixels
/// (see `sonic_ui::tabbar_view::caption_button_rects`); `surface` is
/// `(w, h)` in the same units used by [`px_to_ndc`]. `hover` tints the
/// button under the cursor (close → red, others → muted surface).
pub fn paint_caption_buttons(
    out: &mut Vec<QuadInstance>,
    rects: &[(f32, f32, f32, f32); 3],
    surface: (f32, f32),
    colors: CaptionColors,
    hover: CaptionHover,
) {
    let (sw, sh) = surface;
    let hover_idx = match hover {
        CaptionHover::None => usize::MAX,
        CaptionHover::Min => 0,
        CaptionHover::Max => 1,
        CaptionHover::Close => 2,
    };
    // 1) Background plates.
    for (i, &(x, y, w, h)) in rects.iter().enumerate() {
        let plate_color = if i == hover_idx {
            if i == 2 {
                colors.close_hover_bg
            } else {
                colors.hover_bg
            }
        } else {
            colors.bg
        };
        let ndc = px_to_ndc(x, y, w, h, sw, sh);
        out.push(QuadInstance::sharp(ndc, plate_color));
    }
    // 2) Glyphs — single thin centered shapes drawn with the sharp
    //    quad path. Geometry is intentionally minimal (Win11 caption
    //    icons are 10×10 outlined). Sizes are physical pixels and
    //    assume the surface size is already physical too.
    const ICON_PX: f32 = 10.0;
    const STROKE_PX: f32 = 1.0;
    // Minimize — horizontal dash centered in the button.
    {
        let (bx, by, bw, bh) = rects[0];
        let cx = bx + bw * 0.5;
        let cy = by + bh * 0.5;
        let fg = colors.fg;
        let dash = (cx - ICON_PX * 0.5, cy - STROKE_PX * 0.5, ICON_PX, STROKE_PX);
        out.push(QuadInstance::sharp(px_to_ndc(dash.0, dash.1, dash.2, dash.3, sw, sh), fg));
    }
    // Maximize — 10×10 square outline (4 strokes).
    {
        let (bx, by, bw, bh) = rects[1];
        let cx = bx + bw * 0.5;
        let cy = by + bh * 0.5;
        let fg = colors.fg;
        let left = cx - ICON_PX * 0.5;
        let top = cy - ICON_PX * 0.5;
        // top edge
        out.push(QuadInstance::sharp(px_to_ndc(left, top, ICON_PX, STROKE_PX, sw, sh), fg));
        // bottom edge
        out.push(QuadInstance::sharp(
            px_to_ndc(left, top + ICON_PX - STROKE_PX, ICON_PX, STROKE_PX, sw, sh),
            fg,
        ));
        // left edge
        out.push(QuadInstance::sharp(px_to_ndc(left, top, STROKE_PX, ICON_PX, sw, sh), fg));
        // right edge
        out.push(QuadInstance::sharp(
            px_to_ndc(left + ICON_PX - STROKE_PX, top, STROKE_PX, ICON_PX, sw, sh),
            fg,
        ));
    }
    // Close — `+`-rotated "X". Drawn as a small square of rotated
    //   single-pixel rows; cheap proxy for the ✕ glyph that's
    //   readable at the 32px button height. Implemented as a step
    //   pattern (two diagonals built from short axis-aligned quads)
    //   since the quad pipeline doesn't do rotated rects directly.
    {
        let (bx, by, bw, bh) = rects[2];
        let cx = bx + bw * 0.5;
        let cy = by + bh * 0.5;
        let fg = if hover_idx == 2 { colors.close_hover_fg } else { colors.fg };
        let half = (ICON_PX * 0.5).floor() as i32;
        // Two diagonals: y = cy + dx (down-right) and y = cy - dx (up-right).
        // Each "pixel" of the diagonal is a 1.5×1.5 quad so the X
        // reads cleanly at DPI=1.0 without going through SDF.
        let s = STROKE_PX.max(1.0);
        for dx in -half..=half {
            let fx = cx + dx as f32 - s * 0.5;
            let fy_dr = cy + dx as f32 - s * 0.5; // down-right diagonal
            let fy_ur = cy - dx as f32 - s * 0.5; // up-right diagonal
            out.push(QuadInstance::sharp(px_to_ndc(fx, fy_dr, s, s, sw, sh), fg));
            out.push(QuadInstance::sharp(px_to_ndc(fx, fy_ur, s, s, sw, sh), fg));
        }
    }
}

#[cfg(test)]
mod caption_paint_tests {
    use super::*;

    fn dummy_colors() -> CaptionColors {
        CaptionColors {
            bg: [0.1, 0.1, 0.1, 1.0],
            hover_bg: [0.2, 0.2, 0.2, 1.0],
            close_hover_bg: [0.91, 0.07, 0.14, 1.0], // #E81123
            fg: [0.9, 0.9, 0.9, 1.0],
            close_hover_fg: [1.0, 1.0, 1.0, 1.0],
        }
    }

    fn rects_1000x32() -> [(f32, f32, f32, f32); 3] {
        [(862.0, 0.0, 46.0, 32.0), (908.0, 0.0, 46.0, 32.0), (954.0, 0.0, 46.0, 32.0)]
    }

    #[test]
    fn paint_emits_plates_and_glyphs() {
        let mut quads = Vec::new();
        paint_caption_buttons(
            &mut quads,
            &rects_1000x32(),
            (1000.0, 700.0),
            dummy_colors(),
            CaptionHover::None,
        );
        // 3 plates + 1 (min dash) + 4 (max square) + 11×2 (close X, dx ∈ -5..=5)
        // = 3 + 1 + 4 + 22 = 30.
        assert_eq!(quads.len(), 30);
        // Plates use the no-hover bg color.
        for plate in &quads[..3] {
            assert_eq!(plate.color, [0.1, 0.1, 0.1, 1.0]);
        }
    }

    #[test]
    fn paint_close_hover_uses_red_plate_and_white_glyph() {
        let mut quads = Vec::new();
        paint_caption_buttons(
            &mut quads,
            &rects_1000x32(),
            (1000.0, 700.0),
            dummy_colors(),
            CaptionHover::Close,
        );
        // Plate index 2 (close) should now be red, the other two unchanged.
        assert_eq!(quads[0].color, [0.1, 0.1, 0.1, 1.0]);
        assert_eq!(quads[1].color, [0.1, 0.1, 0.1, 1.0]);
        assert_eq!(quads[2].color, [0.91, 0.07, 0.14, 1.0]);
        // The X glyphs (quads 8..) should be white (close_hover_fg).
        for q in &quads[8..] {
            assert_eq!(q.color, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn paint_min_hover_tints_only_min_plate() {
        let mut quads = Vec::new();
        paint_caption_buttons(
            &mut quads,
            &rects_1000x32(),
            (1000.0, 700.0),
            dummy_colors(),
            CaptionHover::Min,
        );
        assert_eq!(quads[0].color, [0.2, 0.2, 0.2, 1.0]); // hover_bg
        assert_eq!(quads[1].color, [0.1, 0.1, 0.1, 1.0]);
        assert_eq!(quads[2].color, [0.1, 0.1, 0.1, 1.0]);
    }
}
