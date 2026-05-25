//! Instanced text pipeline for the GPU glyph atlas.
//!
//! Consumes one [`GlyphInstance`] per visible cell and draws a single
//! triangle-strip per instance, sampling the atlas alpha and modulating
//! by the per-instance color. This is the half of B3 that replaces
//! glyphon's per-frame text shape + atlas-rebuild on the terminal grid.
//!
//! Wiring into `render.rs` is staged: the module is published, its
//! shader compiles, and the data layout matches what the renderer
//! will hand it, but the renderer still calls glyphon today. The
//! cutover happens behind a follow-up PR that needs the GUI bench
//! loop to verify pixel parity — that loop is not available headlessly.

use wgpu::util::DeviceExt;
use wgpu::{
    BindGroup, BindGroupLayout, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer,
    BufferUsages, ColorTargetState, ColorWrites, Device, FragmentState, MultisampleState,
    PipelineLayoutDescriptor, PrimitiveState, PrimitiveTopology, Queue, RenderPass, RenderPipeline,
    RenderPipelineDescriptor, ShaderModuleDescriptor, ShaderSource, TextureFormat, VertexAttribute,
    VertexBufferLayout, VertexFormat, VertexState, VertexStepMode,
};

/// One drawable glyph in NDC space with its atlas UV rect and color.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphInstance {
    /// `[x, y, w, h]` in NDC (–1..1). `w`/`h` are signed because the
    /// Y axis flips between screen and NDC.
    pub rect: [f32; 4],
    /// `[u0, v0, u1, v1]` normalized atlas coordinates from
    /// `GlyphInfo::uv`.
    pub uv: [f32; 4],
    /// `[r, g, b, a]` foreground color the alpha is modulated by.
    pub color: [f32; 4],
}

/// WGSL for the text pass. The vertex shader builds a quad from a
/// triangle-strip's vertex_index, mapping (0,1,2,3) -> the four
/// corners of `rect` and corresponding `uv` corners. The fragment
/// shader samples the alpha and outputs `color.rgb * coverage,
/// color.a * coverage` — premultiplied so the standard "src1, 1-srcA"
/// blend produces correct text-on-background.
const SHADER: &str = r#"
struct InstanceIn {
    @location(0) rect:  vec4<f32>,
    @location(1) uv:    vec4<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv:    vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, inst: InstanceIn) -> VsOut {
    // Corner indices: 0=TL, 1=TR, 2=BL, 3=BR (triangle-strip order).
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vid];
    let x = inst.rect.x + c.x * inst.rect.z;
    let y = inst.rect.y + c.y * inst.rect.w;
    let u = mix(inst.uv.x, inst.uv.z, c.x);
    let v = mix(inst.uv.y, inst.uv.w, c.y);
    var out: VsOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(u, v);
    out.color = inst.color;
    return out;
}

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_smp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_smp, in.uv).r;
    return vec4<f32>(in.color.rgb * cov, in.color.a * cov);
}
"#;

/// GPU pipeline + instance buffer for the text pass. Created once at
/// startup; per-frame the caller writes new instances and issues a
/// single `draw(0..4, 0..N)`.
pub struct TextPipeline {
    pipeline: RenderPipeline,
    pub bind_group_layout: BindGroupLayout,
    instances: Buffer,
    capacity: u64,
}

impl TextPipeline {
    /// Build the pipeline against the given color target format.
    /// `initial_capacity` is the number of `GlyphInstance` slots
    /// preallocated; the buffer grows on demand.
    pub fn new(device: &Device, format: TextureFormat, initial_capacity: u64) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("sonic-text-pipeline"),
            source: ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sonic-text-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("sonic-text-pl"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("sonic-text-pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                    step_mode: VertexStepMode::Instance,
                    attributes: &INSTANCE_ATTRS,
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState {
                        color: BlendComponent {
                            src_factor: BlendFactor::One,
                            dst_factor: BlendFactor::OneMinusSrcAlpha,
                            operation: BlendOperation::Add,
                        },
                        alpha: BlendComponent {
                            src_factor: BlendFactor::One,
                            dst_factor: BlendFactor::OneMinusSrcAlpha,
                            operation: BlendOperation::Add,
                        },
                    }),
                    write_mask: ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-text-instances"),
            size: initial_capacity.max(1) * std::mem::size_of::<GlyphInstance>() as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, bind_group_layout, instances, capacity: initial_capacity.max(1) }
    }

    /// Upload + draw. Reallocates the instance buffer if it's too
    /// small. `bind_group` must wrap `bind_group_layout` and bind the
    /// atlas texture + sampler.
    pub fn draw<'p>(
        &'p mut self,
        device: &Device,
        queue: &Queue,
        pass: &mut RenderPass<'p>,
        bind_group: &'p BindGroup,
        instances: &[GlyphInstance],
    ) {
        if instances.is_empty() {
            return;
        }
        let needed = instances.len() as u64;
        if needed > self.capacity {
            // Power-of-two grow.
            let mut cap = self.capacity.max(1);
            while cap < needed {
                cap *= 2;
            }
            self.instances = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sonic-text-instances"),
                contents: bytemuck::cast_slice(instances),
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            });
            self.capacity = cap;
        } else {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(instances));
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, 0..instances.len() as u32);
    }

    /// Current instance buffer capacity (for diagnostics + tests).
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
}

const INSTANCE_ATTRS: [VertexAttribute; 3] = [
    VertexAttribute { format: VertexFormat::Float32x4, offset: 0, shader_location: 0 },
    VertexAttribute { format: VertexFormat::Float32x4, offset: 16, shader_location: 1 },
    VertexAttribute { format: VertexFormat::Float32x4, offset: 32, shader_location: 2 },
];
