//! Minimal wgpu quad pipeline. Draws axis-aligned colored rectangles for
//! the cursor and selection highlight, in normalized device coordinates.
//!
//! Each instance is 4 floats (x, y, w, h) in NDC plus an RGBA color.
//! Vertex shader expands a unit triangle strip per instance.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct QuadInstance {
    pub rect: [f32; 4], // x, y, w, h in NDC ([-1,1])
    pub color: [f32; 4],
}

pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_buf: wgpu::Buffer,
    capacity: u64,
}

const SHADER: &str = r#"
struct Instance {
    @location(0) rect:  vec4<f32>,
    @location(1) color: vec4<f32>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
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
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
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
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_to_ndc_full_screen_covers_whole_quad() {
        let q = px_to_ndc(0.0, 0.0, 100.0, 100.0, 100.0, 100.0);
        assert!((q[0] - -1.0).abs() < 1e-5);
        assert!((q[1] - -1.0).abs() < 1e-5);
        assert!((q[2] - 2.0).abs() < 1e-5);
        assert!((q[3] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn px_to_ndc_top_left_pixel() {
        let q = px_to_ndc(0.0, 0.0, 10.0, 10.0, 100.0, 100.0);
        // top-left pixel: x=-1, top of quad at y=1, height=0.2 → y_bottom = 0.8
        assert!((q[0] - -1.0).abs() < 1e-5);
        assert!((q[1] - 0.8).abs() < 1e-5);
    }
}
