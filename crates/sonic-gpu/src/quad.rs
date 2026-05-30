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

/// One quad instance — what the vertex stage reads per draw — packing a
/// rectangle, color, and optional rounded-rect / line-segment SDF parameters
/// into the layout the WGSL shader expects.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
pub struct QuadInstance {
    /// Rectangle as `[x, y, w, h]` in NDC ([-1, 1]).
    pub rect: [f32; 4],
    /// Premultiplied-alpha RGBA fill color in linear space.
    pub color: [f32; 4],
    /// Rectangle width/height in physical pixels (used by the SDF path).
    /// `[0.0, 0.0]` (the default) keeps the sharp-rect path.
    pub size_px: [f32; 2],
    /// Corner radius in physical pixels. `0.0` (default) skips the SDF
    /// rounded-rect path and the quad renders sharp like before.
    pub radius_px: f32,
    /// Line-segment stroke thickness in physical pixels. When `> 0` the
    /// fragment shader takes the line-SDF path and renders an
    /// anti-aliased capsule between [`Self::line_a`] and [`Self::line_b`]
    /// instead of the rounded-rect path. `0.0` (default) keeps the
    /// legacy behaviour.
    pub line_thickness_px: f32,
    /// Line segment endpoint A, in local pixel coordinates **relative to
    /// the rect center** (same frame as the shader's `local` varying).
    /// Only consulted when `line_thickness_px > 0`.
    pub line_a: [f32; 2],
    /// Line segment endpoint B, in local pixel coordinates **relative to
    /// the rect center**. Only consulted when `line_thickness_px > 0`.
    pub line_b: [f32; 2],
}

impl Default for QuadInstance {
    fn default() -> Self {
        Self {
            rect: [0.0; 4],
            color: [0.0; 4],
            size_px: [0.0; 2],
            radius_px: 0.0,
            line_thickness_px: 0.0,
            line_a: [0.0; 2],
            line_b: [0.0; 2],
        }
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
        Self { rect, color, size_px, radius_px, ..Default::default() }
    }

    /// Anti-aliased line segment (rounded-cap capsule) inside the given
    /// bounding-box quad. `rect`/`size_px` describe a bounding box that
    /// fully contains the stroked segment (endpoints +/- thickness/2 +
    /// 1 px AA padding). `line_a` and `line_b` are pixel offsets from the
    /// rect's center. `thickness_px` is the stroke width.
    ///
    /// The fragment shader computes the signed distance to the segment
    /// and smoothsteps a 1-pixel AA band, so diagonals render smooth on
    /// HiDPI without the staircase artifacts a binary 8x8 mask produces.
    #[must_use]
    pub fn line(
        rect: [f32; 4],
        color: [f32; 4],
        size_px: [f32; 2],
        line_a: [f32; 2],
        line_b: [f32; 2],
        thickness_px: f32,
    ) -> Self {
        Self {
            rect,
            color,
            size_px,
            radius_px: 0.0,
            line_thickness_px: thickness_px,
            line_a,
            line_b,
        }
    }
}

/// wgpu render pipeline + a growable instance buffer for `QuadInstance`s.
/// Constructed once at GPU init, drawn one `draw()` call per frame.
pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_buf: wgpu::Buffer,
    capacity: u64,
}

const SHADER: &str = r#"
struct Instance {
    @location(0) rect:    vec4<f32>,
    @location(1) color:   vec4<f32>,
    @location(2) params:  vec4<f32>, // size_px.x, size_px.y, radius_px, line_thickness_px
    @location(3) line:    vec4<f32>, // line_a.x, line_a.y, line_b.x, line_b.y
}

struct VsOut {
    @builtin(position) pos:    vec4<f32>,
    @location(0)        color: vec4<f32>,
    @location(1)        local: vec2<f32>, // pixel offset from rect center
    @location(2)        params: vec4<f32>,
    @location(3)        line:   vec4<f32>,
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
    out.line = inst.line;
    return out;
}

// Signed distance from point p to segment a-b. Returns negative inside the
// "thickness" capsule when combined with a stroke half-width.
fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let thickness = in.params.w;
    if (thickness > 0.0) {
        // Anti-aliased line / capsule SDF. Used for the tab close ×,
        // which would otherwise stair-step on HiDPI as a binary 8x8 mask.
        let a = in.line.xy;
        let b = in.line.zw;
        let d = sd_segment(in.local, a, b) - thickness * 0.5;
        let w = fwidth(d);
        let aa = 1.0 - smoothstep(-w, w, d);
        return vec4<f32>(in.color.rgb, in.color.a * aa);
    }
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
    /// Build the pipeline against the swapchain `format` — call once at GPU
    /// init. The initial instance buffer holds 64 quads and grows on demand.
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
                        2 => Float32x4,
                        3 => Float32x4
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

    /// Upload `instances` (growing the GPU buffer if needed) and emit one
    /// instanced triangle-strip draw covering all of them in the given pass.
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
            // Power-of-two grow. Allocate the FULL capacity in bytes — not
            // just enough for the live prefix — otherwise a later draw with
            // needed <= self.capacity but > current_instance_count would
            // overrun the actual buffer size on write_buffer and trip wgpu
            // validation. (This was the P0 vim-scroll crash: the previous
            // code used `create_buffer_init(contents: instances)` which
            // sizes the buffer to instances.len(), then the next draw with
            // a few more instances slipped past the bounds check.)
            let mut cap = self.capacity.max(1);
            while cap < instances.len() as u64 {
                cap *= 2;
            }
            let stride = std::mem::size_of::<QuadInstance>() as u64;
            self.instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sonic-quad-instances"),
                size: cap * stride,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.capacity = cap;
            queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
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

/// Built-in 8x8 alpha mask for the minimize chrome icon, derived from SVG.
pub const ICON_MINIMIZE_8: &[u8; 64] =
    &mask8_from_rows([0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00]);
/// Built-in 8x8 alpha mask for the maximize chrome icon, derived from SVG.
pub const ICON_MAXIMIZE_8: &[u8; 64] =
    &mask8_from_rows([0xff, 0xff, 0xc3, 0xc3, 0xc3, 0xc3, 0xff, 0xff]);
/// Built-in 8x8 alpha mask for the close chrome icon, derived from SVG.
pub const ICON_CLOSE_8: &[u8; 64] =
    &mask8_from_rows([0xc3, 0xe7, 0x7e, 0x3c, 0x3c, 0x7e, 0xe7, 0xc3]);
/// Built-in 8x8 alpha mask for the plus/new-tab chrome icon, derived from SVG.
pub const ICON_PLUS_8: &[u8; 64] =
    &mask8_from_rows([0x00, 0x18, 0x18, 0xff, 0xff, 0x18, 0x18, 0x00]);

const fn mask8_from_rows(rows: [u8; 8]) -> [u8; 64] {
    let mut out = [0; 64];
    let mut y = 0;
    while y < 8 {
        let mut x = 0;
        while x < 8 {
            out[y * 8 + x] = if rows[y] & (1 << (7 - x)) != 0 { 255 } else { 0 };
            x += 1;
        }
        y += 1;
    }
    out
}

/// Parameters for [`push_close_x_quads`].
#[derive(Debug, Clone, Copy)]
pub struct CloseXParams {
    /// Top-left x of the icon bounding box in physical pixels.
    pub x: f32,
    /// Top-left y of the icon bounding box in physical pixels.
    pub y: f32,
    /// Width / height of the (square) icon bounding box in physical pixels.
    pub size: f32,
    /// Stroke thickness in physical pixels. Clamped to >= 1.0.
    pub thickness: f32,
    /// Premultiplied-straight RGBA stroke color.
    pub color: [f32; 4],
    /// Surface width in physical pixels.
    pub sw: f32,
    /// Surface height in physical pixels.
    pub sh: f32,
}

/// Render a tab "close ×" as two anti-aliased SVG-style diagonal strokes,
/// using the QuadPipeline's line-SDF path. Replaces the legacy 8x8 binary
/// mask whose diagonals stair-stepped visibly on macOS Retina.
///
/// Each stroke is a capsule (rounded caps) inside a quad whose bounding box
/// fully contains it (segment + half-thickness + 1 px AA padding). The
/// fragment shader does the actual anti-aliasing via `fwidth(d)`, so the
/// edge stays a clean 1-pixel band at any DPI.
pub fn push_close_x_quads(out: &mut Vec<QuadInstance>, params: CloseXParams) {
    let CloseXParams { x, y, size, thickness, color, sw, sh } = params;
    let t = thickness.max(1.0);
    // The icon's drawing-space spans [0, size] on each axis; segment
    // endpoints land at the icon corners with a half-thickness padding
    // so the rounded cap stays inside the bounding box.
    let pad = t * 0.5;
    // Bounding box covers the full square. `local` in the shader is in
    // pixels from the rect's center, so put the segment endpoints
    // relative to (size/2, size/2).
    let half = size * 0.5;
    let a1 = [-half + pad, -half + pad];
    let b1 = [half - pad, half - pad];
    let a2 = [half - pad, -half + pad];
    let b2 = [-half + pad, half - pad];
    let rect_ndc = px_to_ndc(x, y, size, size, sw, sh);
    let size_px = [size, size];
    out.push(QuadInstance::line(rect_ndc, color, size_px, a1, b1, t));
    out.push(QuadInstance::line(rect_ndc, color, size_px, a2, b2, t));
}

/// Parameters for [`push_mask_icon_quads`].
#[derive(Debug, Clone, Copy)]
pub struct MaskIconParams<'a> {
    /// Alpha mask in row-major 8x8 order.
    pub mask: &'a [u8; 64],
    /// Top-left x in physical pixels.
    pub x: f32,
    /// Top-left y in physical pixels.
    pub y: f32,
    /// Target icon size in physical pixels.
    pub size: f32,
    /// Minimum emitted cell size in physical pixels.
    pub min_cell: f32,
    /// Linear RGBA color multiplied by each mask alpha.
    pub color: [f32; 4],
    /// Surface width in physical pixels.
    pub sw: f32,
    /// Surface height in physical pixels.
    pub sh: f32,
}

/// Rasterize an 8x8 alpha-mask icon into the quad list. Used by app chrome so
/// icons are data-driven masks instead of text glyphs or Nerd Font codepoints.
pub fn push_mask_icon_quads(out: &mut Vec<QuadInstance>, params: MaskIconParams<'_>) {
    let MaskIconParams { mask, x, y, size, min_cell, color, sw, sh } = params;
    let cell = (size / 8.0).max(min_cell.max(0.5));
    for row in 0..8 {
        for col in 0..8 {
            let alpha = f32::from(mask[row * 8 + col]) / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let mut c = color;
            c[3] *= alpha;
            out.push(QuadInstance::sharp(
                px_to_ndc(x + col as f32 * cell, y + row as f32 * cell, cell, cell, sw, sh),
                c,
            ));
        }
    }
}
