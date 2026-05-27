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

/// Paint the three Win11-style caption-button backgrounds (min / max /
/// close) into the given quad list. Glyph rendering (─ □ ✕) is handled
/// by the text pipeline; this helper only owns the background plates so
/// hover/press states can be styled by theme later.
///
/// Callers on platforms without an integrated titlebar inset (macOS /
/// Linux) should early-return without ever invoking this helper — the
/// function itself is portable but the caption strip only exists on
/// Windows. The previous in-function guard was removed when this code
/// moved into `sonic-gpu` (which cannot depend on `sonic-shared::app`);
/// the single existing caller (`sonic-shared::render`) already gates on
/// `app::integrated_titlebar_inset_px() > 0`, so behavior is unchanged.
///
/// `rects` is `[min, max, close]` as `(x, y, w, h)` in physical pixels
/// (see `sonic_ui::tabbar_view::caption_button_rects`); `surface` is
/// `(w, h)` in the same units used by [`px_rect_to_ndc`]. `bg` is the
/// plate background color (RGBA, premultiplied straight). The close
/// button gets no special tint here — hover-red is a future enhancement.
pub fn paint_caption_buttons(
    out: &mut Vec<QuadInstance>,
    rects: &[(f32, f32, f32, f32); 3],
    surface: (f32, f32),
    bg: [f32; 4],
) {
    let (sw, sh) = surface;
    for &(x, y, w, h) in rects {
        let ndc = px_to_ndc(x, y, w, h, sw, sh);
        out.push(QuadInstance::sharp(ndc, bg));
    }
}
