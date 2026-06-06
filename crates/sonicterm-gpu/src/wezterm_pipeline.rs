//! WezTerm-style WebGPU presentation pipeline.
//!
//! Upstream WezTerm's GUI renderer is not exposed as a library, but its
//! WebGPU backend uses a single vertex format + shader branch model for
//! glyphs, color glyphs, and solid UI quads. This module adapts that final
//! presentation model to SonicTerm's wgpu 29 surface while keeping the
//! already-WezTerm-backed shaping/rasterization/atlas path intact.

use wgpu::util::DeviceExt;

use crate::quad::QuadInstance;
use sonicterm_text::GlyphInstance;

const VERTICES_PER_QUAD: usize = 4;
const INDICES_PER_QUAD: usize = 6;

const V_TOP_LEFT: u32 = 0;
const V_TOP_RIGHT: u32 = 1;
const V_BOT_LEFT: u32 = 2;
const V_BOT_RIGHT: u32 = 3;

const IS_GLYPH: f32 = 0.0;
const IS_COLOR_EMOJI: f32 = 1.0;
const IS_SOLID_COLOR: f32 = 3.0;
const IS_ROUNDED_RECT: f32 = 5.0;
const IS_LINE: f32 = 6.0;

#[repr(C)]
#[derive(Copy, Clone, Default, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    tex: [f32; 2],
    fg_color: [f32; 4],
    alt_color: [f32; 4],
    hsv: [f32; 3],
    has_color: f32,
    mix_value: f32,
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x4,
        3 => Float32x4,
        4 => Float32x3,
        5 => Float32,
        6 => Float32,
    ];

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ShaderUniform {
    foreground_text_hsb: [f32; 3],
    milliseconds: u32,
    projection: [[f32; 4]; 4],
}

/// Single final presentation pipeline for all atlas glyphs and colored
/// geometry. Replaces the separate SonicTerm text/quad render pipelines at
/// the final draw boundary.
pub struct WeztermPipeline {
    pipeline: wgpu::RenderPipeline,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    vertex_capacity: u64,
    index_capacity: u64,
}

impl WeztermPipeline {
    /// Build the pipeline against the swapchain format.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, initial_quads: u64) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sonic-wezterm-present-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sonic-wezterm-uniform-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sonic-wezterm-texture-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sonic-wezterm-present-layout"),
            bind_group_layouts: &[
                Some(&uniform_bind_group_layout),
                Some(&texture_bind_group_layout),
                Some(&texture_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sonic-wezterm-present-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::desc()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(premultiplied_alpha_blend()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let initial_quads = initial_quads.max(1);
        let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sonic-wezterm-uniform"),
            contents: bytemuck::cast_slice(&[ShaderUniform::default()]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sonic-wezterm-uniform-bg"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        let vertex_capacity = initial_quads * VERTICES_PER_QUAD as u64;
        let index_capacity = initial_quads * INDICES_PER_QUAD as u64;
        let vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-wezterm-present-vertices"),
            size: vertex_capacity * std::mem::size_of::<Vertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-wezterm-present-indices"),
            size: index_capacity * std::mem::size_of::<u32>() as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            texture_bind_group_layout,
            uniform_buf,
            uniform_bind_group,
            vertex_buf,
            index_buf,
            vertex_capacity,
            index_capacity,
        }
    }

    /// Bind-group layout consumed by [`crate::atlas_upload::AtlasUpload`].
    pub fn texture_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.texture_bind_group_layout
    }

    /// Upload and draw all layers in final painter order.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_frame<'p>(
        &'p mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'p>,
        atlas_bind_group: &'p wgpu::BindGroup,
        surface_w: f32,
        surface_h: f32,
        quads: &[QuadInstance],
        glyphs: &[GlyphInstance],
        overlay_quads: &[QuadInstance],
        overlay_glyphs: &[GlyphInstance],
    ) {
        let total_quads = quads.len() + glyphs.len() + overlay_quads.len() + overlay_glyphs.len();
        if total_quads == 0 {
            return;
        }

        let mut vertices = Vec::with_capacity(total_quads * VERTICES_PER_QUAD);
        push_quad_instances(&mut vertices, quads, surface_w, surface_h);
        push_glyph_instances(&mut vertices, glyphs, surface_w, surface_h);
        push_quad_instances(&mut vertices, overlay_quads, surface_w, surface_h);
        push_glyph_instances(&mut vertices, overlay_glyphs, surface_w, surface_h);

        let indices = build_indices(vertices.len() / VERTICES_PER_QUAD);
        self.ensure_capacity(device, vertices.len() as u64, indices.len() as u64);

        queue.write_buffer(&self.vertex_buf, 0, bytemuck::cast_slice(&vertices));
        queue.write_buffer(&self.index_buf, 0, bytemuck::cast_slice(&indices));

        let uniform = ShaderUniform {
            foreground_text_hsb: [1.0, 1.0, 1.0],
            milliseconds: 0,
            projection: orthographic_projection(surface_w, surface_h),
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&[uniform]));

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_bind_group(1, atlas_bind_group, &[]);
        pass.set_bind_group(2, atlas_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        pass.set_index_buffer(self.index_buf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
    }

    fn ensure_capacity(&mut self, device: &wgpu::Device, vertices: u64, indices: u64) {
        if vertices > self.vertex_capacity {
            let mut cap = self.vertex_capacity.max(1);
            while cap < vertices {
                cap *= 2;
            }
            self.vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sonic-wezterm-present-vertices"),
                size: cap * std::mem::size_of::<Vertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vertex_capacity = cap;
        }
        if indices > self.index_capacity {
            let mut cap = self.index_capacity.max(1);
            while cap < indices {
                cap *= 2;
            }
            self.index_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sonic-wezterm-present-indices"),
                size: cap * std::mem::size_of::<u32>() as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.index_capacity = cap;
        }
    }
}

fn push_glyph_instances(out: &mut Vec<Vertex>, glyphs: &[GlyphInstance], sw: f32, sh: f32) {
    for g in glyphs {
        let Some((x, y, w, h)) = ndc_rect_to_pixels(g.rect, sw, sh) else { continue };
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let color = g.color;
        let has_color = if g.flags[0] >= 0.5 { IS_COLOR_EMOJI } else { IS_GLYPH };
        let [u0, v0, u1, v1] = g.uv;
        let tex = [[u0, v0], [u1, v0], [u0, v1], [u1, v1]];
        push_rect_vertices(out, x, y, w, h, sw, sh, color, has_color, tex, [[0.0; 4]; 4]);
    }
}

fn push_quad_instances(out: &mut Vec<Vertex>, quads: &[QuadInstance], sw: f32, sh: f32) {
    for q in quads {
        let Some((x, y, w, h)) = ndc_rect_to_pixels(q.rect, sw, sh) else { continue };
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let kind = if q.line_thickness_px > 0.0 {
            IS_LINE
        } else if q.radius_px > 0.0 {
            IS_ROUNDED_RECT
        } else {
            IS_SOLID_COLOR
        };
        let size = if q.size_px[0] > 0.0 && q.size_px[1] > 0.0 { q.size_px } else { [w, h] };
        let local = [
            [-size[0] * 0.5, -size[1] * 0.5],
            [size[0] * 0.5, -size[1] * 0.5],
            [-size[0] * 0.5, size[1] * 0.5],
            [size[0] * 0.5, size[1] * 0.5],
        ];
        let params = [
            [size[0], size[1], q.radius_px, q.line_thickness_px],
            [size[0], size[1], q.radius_px, q.line_thickness_px],
            [size[0], size[1], q.radius_px, q.line_thickness_px],
            [size[0], size[1], q.radius_px, q.line_thickness_px],
        ];
        push_rect_vertices(out, x, y, w, h, sw, sh, q.color, kind, local, params);
        if kind == IS_LINE {
            let n = out.len();
            for v in &mut out[n - VERTICES_PER_QUAD..n] {
                v.hsv = [q.line_a[0], q.line_a[1], q.line_b[0]];
                v.mix_value = q.line_b[1];
            }
        }
    }
}

fn push_rect_vertices(
    out: &mut Vec<Vertex>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
    has_color: f32,
    tex: [[f32; 2]; 4],
    params: [[f32; 4]; 4],
) {
    let left = x - sw * 0.5;
    let right = x + w - sw * 0.5;
    let top = y - sh * 0.5;
    let bottom = y + h - sh * 0.5;
    let positions = [[left, top], [right, top], [left, bottom], [right, bottom]];
    for i in 0..VERTICES_PER_QUAD {
        out.push(Vertex {
            position: positions[i],
            tex: tex[i],
            fg_color: color,
            alt_color: params[i],
            hsv: [1.0, 1.0, 1.0],
            has_color,
            mix_value: 0.0,
        });
    }
}

fn ndc_rect_to_pixels(rect: [f32; 4], sw: f32, sh: f32) -> Option<(f32, f32, f32, f32)> {
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    let x = (rect[0] + 1.0) * 0.5 * sw;
    let w = rect[2] * 0.5 * sw;
    let h = rect[3] * 0.5 * sh;
    let top_ndc = rect[1] + rect[3];
    let y = (1.0 - top_ndc) * 0.5 * sh;
    Some((x, y, w, h))
}

fn build_indices(quad_count: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(quad_count * INDICES_PER_QUAD);
    for q in 0..quad_count {
        let base = (q * VERTICES_PER_QUAD) as u32;
        out.extend_from_slice(&[
            base + V_TOP_LEFT,
            base + V_TOP_RIGHT,
            base + V_BOT_LEFT,
            base + V_TOP_RIGHT,
            base + V_BOT_LEFT,
            base + V_BOT_RIGHT,
        ]);
    }
    out
}

fn orthographic_projection(sw: f32, sh: f32) -> [[f32; 4]; 4] {
    // Matches WezTerm's pixel-space projection:
    // left=-w/2, right=w/2, bottom=h/2, top=-h/2.
    [
        [2.0 / sw, 0.0, 0.0, 0.0],
        [0.0, -2.0 / sh, 0.0, 0.0],
        [0.0, 0.0, -1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn premultiplied_alpha_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

const SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex: vec2<f32>,
    @location(2) fg_color: vec4<f32>,
    @location(3) alt_color: vec4<f32>,
    @location(4) hsv: vec3<f32>,
    @location(5) has_color: f32,
    @location(6) mix_value: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex: vec2<f32>,
    @location(1) fg_color: vec4<f32>,
    @location(2) alt_color: vec4<f32>,
    @location(3) hsv: vec3<f32>,
    @location(4) has_color: f32,
    @location(5) mix_value: f32,
};

const IS_GLYPH: f32 = 0.0;
const IS_COLOR_EMOJI: f32 = 1.0;
const IS_SOLID_COLOR: f32 = 3.0;
const IS_ROUNDED_RECT: f32 = 5.0;
const IS_LINE: f32 = 6.0;

struct ShaderUniform {
    foreground_text_hsb: vec3<f32>,
    milliseconds: u32,
    projection: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> uniforms: ShaderUniform;

@group(1) @binding(0) var atlas_linear_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_linear_sampler: sampler;

@group(2) @binding(0) var atlas_nearest_tex: texture_2d<f32>;
@group(2) @binding(1) var atlas_nearest_sampler: sampler;

fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.bg, K.wz), vec4<f32>(c.gb, K.xy), step(c.b, c.g));
    let q = mix(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, vec3(0.0), vec3(1.0)), c.y);
}

fn apply_hsv(c: vec4<f32>, transform: vec3<f32>) -> vec4<f32> {
    let hsv = rgb2hsv(c.rgb) * transform;
    return vec4<f32>(hsv2rgb(hsv).rgb, c.a);
}

fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.tex = model.tex;
    out.fg_color = model.fg_color;
    out.alt_color = model.alt_color;
    out.hsv = model.hsv;
    out.has_color = model.has_color;
    out.mix_value = model.mix_value;
    out.clip_position = uniforms.projection * vec4<f32>(model.position, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var color: vec4<f32>;
    var hsv = in.hsv;

    if (in.has_color == IS_SOLID_COLOR) {
        color = in.fg_color;
    } else if (in.has_color == IS_ROUNDED_RECT) {
        let size = in.alt_color.xy;
        let radius = in.alt_color.z;
        let half_size = size * 0.5;
        let q = abs(in.tex) - (half_size - vec2<f32>(radius, radius));
        let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius;
        let w = fwidth(d);
        let aa = 1.0 - smoothstep(-w, w, d);
        color = in.fg_color * aa;
    } else if (in.has_color == IS_LINE) {
        let a = in.hsv.xy;
        let b = vec2<f32>(in.hsv.z, in.mix_value);
        let thickness = in.alt_color.w;
        let d = sd_segment(in.tex, a, b) - thickness * 0.5;
        let w = fwidth(d);
        let aa = 1.0 - smoothstep(-w, w, d);
        color = in.fg_color * aa;
    } else if (in.has_color == IS_COLOR_EMOJI) {
        color = textureSample(atlas_nearest_tex, atlas_nearest_sampler, in.tex);
    } else if (in.has_color == IS_GLYPH) {
        let sample = textureSample(atlas_nearest_tex, atlas_nearest_sampler, in.tex);
        let cov = sample.a;
        color = vec4<f32>(in.fg_color.rgb * cov, in.fg_color.a * cov);
        hsv *= uniforms.foreground_text_hsb;
    }

    color = apply_hsv(color, hsv);
    return color;
}
"#;
