//! WezTerm-style WebGPU presentation pipeline.
//!
//! Upstream WezTerm's GUI renderer is not exposed as a library, but its
//! WebGPU backend uses a single vertex format + shader branch model for
//! glyphs, color glyphs, and solid UI quads. This module adapts that final
//! presentation model to SonicTerm's wgpu 29 surface while keeping the
//! already-WezTerm-backed shaping/rasterization/atlas path intact.

use crate::quad::QuadInstance;
use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode;
use sonicterm_text::GlyphInstance;
use sonicterm_types::ResourceAmount;

const VERTICES_PER_QUAD: usize = 4;
const INDICES_PER_QUAD: usize = 6;

/// Index-buffer usage. Unit tests also read the buffer back to check its pattern.
#[cfg(not(test))]
const INDEX_BUFFER_USAGE: wgpu::BufferUsages =
    wgpu::BufferUsages::INDEX.union(wgpu::BufferUsages::COPY_DST);
#[cfg(test)]
const INDEX_BUFFER_USAGE: wgpu::BufferUsages = wgpu::BufferUsages::INDEX
    .union(wgpu::BufferUsages::COPY_DST)
    .union(wgpu::BufferUsages::COPY_SRC);

const V_TOP_LEFT: u32 = 0;
const V_TOP_RIGHT: u32 = 1;
const V_BOT_LEFT: u32 = 2;
const V_BOT_RIGHT: u32 = 3;

const IS_GLYPH: f32 = 0.0;
const IS_COLOR_EMOJI: f32 = 1.0;
const IS_IMAGE: f32 = 2.0;
const IS_SOLID_COLOR: f32 = 3.0;
const IS_ROUNDED_RECT: f32 = 5.0;
const IS_LINE: f32 = 6.0;
const IS_SUBPIXEL_RGB: f32 = 7.0;
const IS_SUBPIXEL_BGR: f32 = 8.0;

#[repr(C)]
#[derive(Copy, Clone, Default, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Vertex {
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

/// Create the shader-uniform buffer without initial contents.
///
/// `draw_frame` writes the uniform before every draw, so no initial contents
/// are needed. A plain `create_buffer` also keeps an invalid descriptor a
/// contained wgpu error: `create_buffer_init` maps the new buffer and panics
/// through `expect` when that buffer is invalid.
fn create_uniform_buffer(device: &wgpu::Device, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sonic-wezterm-uniform"),
        size: std::mem::size_of::<ShaderUniform>() as u64,
        usage,
        mapped_at_creation: false,
    })
}

/// Create an unmapped index buffer holding `index_capacity` indices.
///
/// Every index buffer goes through here, at creation and at growth. A plain
/// `create_buffer` keeps an invalid descriptor a contained wgpu error, where
/// `create_buffer_init` would map the new buffer and panic on an invalid one.
fn create_index_buffer(
    device: &wgpu::Device,
    index_capacity: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sonic-wezterm-present-indices"),
        size: index_capacity * std::mem::size_of::<u32>() as u64,
        usage,
        mapped_at_creation: false,
    })
}

/// The primitive layers of one frame, in painter order after the optional reset.
pub(crate) struct PipelineLayers<'frame> {
    /// Opaque terminal and chrome quads.
    pub quads: &'frame [QuadInstance],
    /// Inline images.
    pub images: &'frame [ImageInstance],
    /// Terminal and chrome glyphs.
    pub glyphs: &'frame [GlyphInstance],
    /// Modal backdrop quads.
    pub overlay_quads: &'frame [QuadInstance],
    /// Modal text glyphs.
    pub overlay_glyphs: &'frame [GlyphInstance],
}

/// Append one frame's vertices to `out` and return how many belong to the reset quad.
///
/// Degenerate primitives push nothing, so the returned count and `out.len()` are the
/// only sources for draw ranges.
pub(crate) fn assemble_vertices(
    out: &mut Vec<Vertex>,
    reset: Option<&QuadInstance>,
    layers: &PipelineLayers<'_>,
    surface_w: f32,
    surface_h: f32,
    subpixel_aa: SubpixelAaMode,
) -> usize {
    push_quad_instances(out, reset.map(std::slice::from_ref).unwrap_or(&[]), surface_w, surface_h);
    let reset_vertices = out.len();
    push_quad_instances(out, layers.quads, surface_w, surface_h);
    push_image_instances(out, layers.images, surface_w, surface_h);
    push_glyph_instances(out, layers.glyphs, surface_w, surface_h, subpixel_aa);
    push_quad_instances(out, layers.overlay_quads, surface_w, surface_h);
    push_glyph_instances(out, layers.overlay_glyphs, surface_w, surface_h, subpixel_aa);
    reset_vertices
}

/// Index ranges for the reset draw and the main draw, from emitted vertex counts only.
pub(crate) fn draw_index_ranges(
    reset_vertices: usize,
    total_vertices: usize,
) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
    let reset_indices = (reset_vertices / VERTICES_PER_QUAD * INDICES_PER_QUAD) as u32;
    let total_indices = (total_vertices / VERTICES_PER_QUAD * INDICES_PER_QUAD) as u32;
    (0..reset_indices, reset_indices..total_indices)
}

/// Scratch capacity, in bytes, below which the vertex scratch is never shrunk.
const SCRATCH_RELEASE_FLOOR_BYTES: usize = 1024 * 1024;

/// Shrink `scratch` after a frame that used `used_vertices` of it.
///
/// A capacity over four times the frame's use and over 1 MiB is cut to twice that use,
/// so one large frame does not pin its peak for the window's life. A frame that used no
/// vertices therefore releases a scratch over 1 MiB entirely; a smaller one is kept.
/// Contents are kept.
pub(crate) fn release_scratch_excess(scratch: &mut Vec<Vertex>, used_vertices: usize) {
    release_excess(scratch, used_vertices);
}

/// Shrink any reused `scratch` after a pass that used `used` of its elements, by the vertex
/// scratch's rule: a capacity over four times the use and over 1 MiB is cut to twice the use.
/// Contents are kept.
pub(crate) fn release_excess<T>(scratch: &mut Vec<T>, used: usize) {
    let capacity_bytes = scratch.capacity().saturating_mul(std::mem::size_of::<T>());
    if scratch.capacity() > used.saturating_mul(4) && capacity_bytes > SCRATCH_RELEASE_FLOOR_BYTES
    {
        // The scratch is both oversized for this pass and large in absolute terms.
        scratch.shrink_to(used.saturating_mul(2));
    }
}

/// One clipped image draw with its original packed-atlas sampling boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageInstance {
    /// Visible `[x, y, width, height]` in physical surface pixels.
    pub rect_px: [f32; 4],
    /// Visible normalized UV endpoints, interpolated across `rect_px`.
    pub uv: [f32; 4],
    /// Original packed tile UV endpoints, independent of destination clipping.
    pub sample_uv: [f32; 4],
}

/// Single final presentation pipeline for all atlas glyphs and colored
/// geometry. Replaces the separate SonicTerm text/quad render pipelines at
/// the final draw boundary.
pub struct WeztermPipeline {
    pipeline: wgpu::RenderPipeline,
    reset_pipeline: wgpu::RenderPipeline,
    dual_source_pipeline: Option<wgpu::RenderPipeline>,
    image_bind_group_layout: wgpu::BindGroupLayout,
    glyph_bind_group_layout: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    /// Vertices the vertex buffer holds.
    vertex_capacity: u64,
    /// Indices the index buffer holds; its quad capacity is this over `INDICES_PER_QUAD`.
    index_capacity: u64,
    /// Quads whose index pattern the current index buffer holds: 0 after creation and growth.
    index_pattern_quads: u64,
    /// Per-frame vertex assembly storage, cleared and reused every frame.
    vertex_scratch: Vec<Vertex>,
}

impl WeztermPipeline {
    /// Build the pipeline against the swapchain format.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, initial_quads: u64) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sonic-wezterm-present-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source(false).into()),
        });
        let dual_source_shader =
            device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING).then(|| {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("sonic-wezterm-dual-source-shader"),
                    source: wgpu::ShaderSource::Wgsl(shader_source(true).into()),
                })
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

        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2,
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
            },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let image_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sonic-wezterm-image-bgl"),
                entries: &[texture_entry(0), sampler_entry(1)],
            });
        let glyph_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sonic-wezterm-glyph-bgl"),
                entries: &[texture_entry(0), sampler_entry(1), texture_entry(2), sampler_entry(3)],
            });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sonic-wezterm-present-layout"),
            bind_group_layouts: &[
                Some(&uniform_bind_group_layout),
                Some(&image_bind_group_layout),
                Some(&glyph_bind_group_layout),
            ],
            immediate_size: 0,
        });

        let create_pipeline = |label, shader: &wgpu::ShaderModule, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(Vertex::desc())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
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
            })
        };
        let pipeline =
            create_pipeline("sonic-wezterm-present-pipeline", &shader, premultiplied_alpha_blend());
        let reset_pipeline =
            create_pipeline("sonic-retained-reset-pipeline", &shader, wgpu::BlendState::REPLACE);
        let dual_source_pipeline = dual_source_shader.as_ref().map(|shader| {
            create_pipeline("sonic-wezterm-dual-source-pipeline", shader, dual_source_alpha_blend())
        });

        let initial_quads = initial_quads.max(1);
        let uniform_buf = create_uniform_buffer(
            device,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
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
        let index_buf = create_index_buffer(device, index_capacity, INDEX_BUFFER_USAGE);

        Self {
            pipeline,
            reset_pipeline,
            dual_source_pipeline,
            image_bind_group_layout,
            glyph_bind_group_layout,
            uniform_buf,
            uniform_bind_group,
            vertex_buf,
            index_buf,
            vertex_capacity,
            index_capacity,
            index_pattern_quads: 0,
            vertex_scratch: Vec::new(),
        }
    }

    /// CPU storage held by the reusable vertex scratch: its capacity in bytes, one item.
    pub(crate) fn vertex_scratch_retained(&self) -> ResourceAmount {
        let capacity = self.vertex_scratch.capacity();
        ResourceAmount {
            bytes: capacity.saturating_mul(std::mem::size_of::<Vertex>()),
            items: usize::from(capacity > 0),
        }
    }

    /// Image-atlas layout: one sRGB view and one linear sampler.
    pub(crate) fn image_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.image_bind_group_layout
    }

    /// Glyph-atlas layout: unorm coverage and sRGB color views with nearest samplers.
    pub(crate) fn glyph_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.glyph_bind_group_layout
    }

    /// Upload one batch, replace optional damage pixels, then blend layers in final painter order.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_frame<'p>(
        &'p mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'p>,
        image_atlas_bind_group: &'p wgpu::BindGroup,
        glyph_atlas_bind_group: &'p wgpu::BindGroup,
        surface_w: f32,
        surface_h: f32,
        subpixel_aa: SubpixelAaMode,
        reset: Option<QuadInstance>,
        quads: &[QuadInstance],
        images: &[ImageInstance],
        glyphs: &[GlyphInstance],
        overlay_quads: &[QuadInstance],
        overlay_glyphs: &[GlyphInstance],
    ) {
        let mut vertices = std::mem::take(&mut self.vertex_scratch);
        vertices.clear();
        let layers = PipelineLayers { quads, images, glyphs, overlay_quads, overlay_glyphs };
        let reset_vertices = assemble_vertices(
            &mut vertices,
            reset.as_ref(),
            &layers,
            surface_w,
            surface_h,
            subpixel_aa,
        );
        let (reset_range, main_range) = draw_index_ranges(reset_vertices, vertices.len());
        // The release policy runs once per frame, before either exit, so a frame that emits no
        // vertices (empty or all degenerate) still releases a large scratch. Shrinking keeps
        // the assembled vertices, since the target capacity is at least their count.
        let used_vertices = vertices.len();
        release_scratch_excess(&mut vertices, used_vertices);
        self.vertex_scratch = vertices;
        if main_range.end == 0 {
            // When: `main_range.end` is zero, the frame emitted no vertices and nothing is drawn.
            return;
        }
        self.ensure_capacity(device, used_vertices as u64, u64::from(main_range.end));

        let index_bytes = self.upload_index_pattern(queue);
        let vertex_bytes: &[u8] = bytemuck::cast_slice(&self.vertex_scratch);
        crate::frame_stats::note_buffer_writes(vertex_bytes.len(), index_bytes);
        queue.write_buffer(&self.vertex_buf, 0, vertex_bytes);

        let uniform = ShaderUniform {
            foreground_text_hsb: [1.0, 1.0, 1.0],
            milliseconds: 0,
            projection: orthographic_projection(surface_w, surface_h),
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&[uniform]));

        let pipeline = match subpixel_aa {
            SubpixelAaMode::Off => &self.pipeline,
            SubpixelAaMode::Rgb | SubpixelAaMode::Bgr => self
                .dual_source_pipeline
                .as_ref()
                .expect("effective LCD mode requires a dual-source pipeline"),
        };
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_bind_group(1, image_atlas_bind_group, &[]);
        pass.set_bind_group(2, glyph_atlas_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        pass.set_index_buffer(self.index_buf.slice(..), wgpu::IndexFormat::Uint32);
        if !reset_range.is_empty() {
            // Retained ink must be erased independently of content alpha and LCD mode.
            pass.set_pipeline(&self.reset_pipeline);
            pass.draw_indexed(reset_range, 0, 0..1);
        }
        pass.set_pipeline(pipeline);
        pass.draw_indexed(main_range, 0, 0..1);
    }

    /// Write the index pattern for the buffer's whole quad capacity when the buffer does
    /// not hold it yet (the first frame and the first after growth); return the bytes written.
    ///
    /// Runs inside the renderer's device-error boundary like every other frame write.
    fn upload_index_pattern(&mut self, queue: &wgpu::Queue) -> usize {
        let quad_capacity = self.index_capacity / INDICES_PER_QUAD as u64;
        if self.index_pattern_quads >= quad_capacity {
            // When: `index_pattern_quads` already covers `quad_capacity`; every draw range is a prefix.
            return 0;
        }
        let pattern = build_indices(quad_capacity as usize);
        let pattern_bytes: &[u8] = bytemuck::cast_slice(&pattern);
        queue.write_buffer(&self.index_buf, 0, pattern_bytes);
        self.index_pattern_quads = quad_capacity;
        pattern_bytes.len()
    }

    /// Grow the vertex buffer to hold `vertices` vertices and the index buffer to hold
    /// `indices` indices, doubling each; a grown index buffer holds no pattern yet.
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
            self.index_buf = create_index_buffer(device, cap, INDEX_BUFFER_USAGE);
            self.index_capacity = cap;
            self.index_pattern_quads = 0;
        }
    }
}

fn push_image_instances(out: &mut Vec<Vertex>, images: &[ImageInstance], sw: f32, sh: f32) {
    for image in images {
        let [x, y, w, h] = image.rect_px;
        if w <= 0.0 || h <= 0.0 {
            // When: `w` or `h` is empty, no image fragments can be produced.
            continue;
        }
        let [u0, v0, u1, v1] = image.uv;
        let tex = [[u0, v0], [u1, v0], [u0, v1], [u1, v1]];
        push_rect_vertices(
            out,
            x,
            y,
            w,
            h,
            sw,
            sh,
            [1.0; 4],
            IS_IMAGE,
            tex,
            [image.sample_uv; VERTICES_PER_QUAD],
        );
    }
}

fn push_glyph_instances(
    out: &mut Vec<Vertex>,
    glyphs: &[GlyphInstance],
    sw: f32,
    sh: f32,
    subpixel_aa: SubpixelAaMode,
) {
    for g in glyphs {
        let Some((x, y, w, h)) = ndc_rect_to_pixels(g.rect, sw, sh) else {
            // When: ndc_rect_to_pixels returns None the surface has zero extent, so this
            // glyph is dropped instead of scaled against a zero-sized frame.
            continue;
        };
        if w <= 0.0 || h <= 0.0 {
            // When: w or h is not positive the quad is degenerate or inverted, adding no
            // fragments while still consuming vertices and indices in the batch.
            continue;
        }
        let color = g.color;
        let has_color = if g.flags[0] >= 0.5 {
            IS_COLOR_EMOJI
        } else if g.flags[1] >= 0.5 {
            // When: `g.flags[1]` marks a subpixel mask, `subpixel_aa` selects its presentation classification.
            match subpixel_aa {
                SubpixelAaMode::Off => IS_GLYPH,
                SubpixelAaMode::Rgb => IS_SUBPIXEL_RGB,
                SubpixelAaMode::Bgr => IS_SUBPIXEL_BGR,
            }
        } else {
            // When: no flags bit is set, so the glyph is monochrome: atlas alpha is coverage
            // and the shader scales fg_color by it, then applies foreground_text_hsb.
            IS_GLYPH
        };
        let [u0, v0, u1, v1] = g.uv;
        let tex = [[u0, v0], [u1, v0], [u0, v1], [u1, v1]];
        push_rect_vertices(
            out,
            x,
            y,
            w,
            h,
            sw,
            sh,
            color,
            has_color,
            tex,
            [[0.0; 4]; VERTICES_PER_QUAD],
        );
    }
}

fn push_quad_instances(out: &mut Vec<Vertex>, quads: &[QuadInstance], sw: f32, sh: f32) {
    for q in quads {
        let Some((x, y, w, h)) = ndc_rect_to_pixels(q.rect, sw, sh) else {
            // When: ndc_rect_to_pixels returns None the surface has zero extent, so this
            // quad is dropped instead of scaled against a zero-sized frame.
            continue;
        };
        if w <= 0.0 || h <= 0.0 {
            // When: w or h is not positive the quad is degenerate or inverted, adding no
            // fragments while still consuming vertices and indices in the batch.
            continue;
        }
        let kind = if q.line_thickness_px > 0.0 {
            IS_LINE
        } else if q.radius_px > 0.0 {
            // When: radius_px is positive the shader evaluates a rounded-rect distance
            // field, so the corners stay antialiased instead of squared off.
            IS_ROUNDED_RECT
        } else {
            // When: neither line_thickness_px nor radius_px is set the quad is a plain
            // fill, so the shader emits fg_color with no distance-field pass.
            IS_SOLID_COLOR
        };
        let size = if q.size_px[0] > 0.0 && q.size_px[1] > 0.0 {
            q.size_px
        } else {
            // When: size_px is unset the pixel extent w and h stand in, keeping the
            // distance field in the same units the vertices were built from.
            [w, h]
        };
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
                v.alt_color = [q.line_a[0], q.line_a[1], q.line_b[0], q.line_b[1]];
                v.mix_value = q.line_thickness_px;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
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

pub(crate) fn ndc_rect_to_pixels(rect: [f32; 4], sw: f32, sh: f32) -> Option<(f32, f32, f32, f32)> {
    if sw <= 0.0 || sh <= 0.0 {
        // When: sw or sh is not positive the surface has no pixel extent and no NDC
        // mapping exists, so every caller skips the primitive instead of scaling by zero.
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

fn dual_source_alpha_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc1,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc1Alpha,
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
    @location(4) @interpolate(flat) has_color: f32,
    @location(5) mix_value: f32,
};

const IS_GLYPH: f32 = 0.0;
const IS_COLOR_EMOJI: f32 = 1.0;
const IS_IMAGE: f32 = 2.0;
const IS_SOLID_COLOR: f32 = 3.0;
const IS_ROUNDED_RECT: f32 = 5.0;
const IS_LINE: f32 = 6.0;
const IS_SUBPIXEL_RGB: f32 = 7.0;
const IS_SUBPIXEL_BGR: f32 = 8.0;

struct ShaderUniform {
    foreground_text_hsb: vec3<f32>,
    milliseconds: u32,
    projection: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> uniforms: ShaderUniform;

@group(1) @binding(0) var atlas_linear_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_linear_sampler: sampler;

@group(2) @binding(0) var glyph_coverage_tex: texture_2d<f32>;
@group(2) @binding(1) var glyph_coverage_sampler: sampler;
@group(2) @binding(2) var glyph_color_tex: texture_2d<f32>;
@group(2) @binding(3) var glyph_color_sampler: sampler;

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

struct ShadeResult {
    color: vec4<f32>,
    factor: vec4<f32>,
};

fn shade(in: VertexOutput) -> ShadeResult {
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
        let a = in.alt_color.xy;
        let b = in.alt_color.zw;
        let thickness = in.mix_value;
        let d = sd_segment(in.tex, a, b) - thickness * 0.5;
        let w = max(fwidth(d), 1.0e-4);
        let aa = 1.0 - smoothstep(-w, w, d);
        color = in.fg_color * aa;
    } else if (in.has_color == IS_IMAGE) {
        let atlas_size = vec2<f32>(textureDimensions(atlas_linear_tex));
        let half_texel = 0.5 / atlas_size;
        let tile_center = (in.alt_color.xy + in.alt_color.zw) * 0.5;
        let tile_half_extent = max(
            (in.alt_color.zw - in.alt_color.xy) * 0.5 - half_texel,
            vec2<f32>(0.0),
        );
        let sample_uv = clamp(
            in.tex,
            tile_center - tile_half_extent,
            tile_center + tile_half_extent,
        );
        color = textureSample(atlas_linear_tex, atlas_linear_sampler, sample_uv);
    } else if (in.has_color == IS_COLOR_EMOJI) {
        color = textureSample(glyph_color_tex, glyph_color_sampler, in.tex);
    } else if (in.has_color == IS_SUBPIXEL_RGB || in.has_color == IS_SUBPIXEL_BGR) {
        let sample = textureSample(glyph_coverage_tex, glyph_coverage_sampler, in.tex);
        let coverage = select(sample.rgb, sample.bgr, in.has_color == IS_SUBPIXEL_BGR);
        let transformed_foreground = apply_hsv(in.fg_color, uniforms.foreground_text_hsb);
        let weights = coverage * transformed_foreground.a;
        let source_alpha = max(weights.r, max(weights.g, weights.b));
        color = vec4<f32>(transformed_foreground.rgb * coverage, source_alpha);
        return ShadeResult(color, vec4<f32>(weights, source_alpha));
    } else if (in.has_color == IS_GLYPH) {
        let sample = textureSample(glyph_coverage_tex, glyph_coverage_sampler, in.tex);
        let cov = sample.a;
        color = vec4<f32>(in.fg_color.rgb * cov, in.fg_color.a * cov);
        hsv *= uniforms.foreground_text_hsb;
    }

    color = apply_hsv(color, hsv);
    return ShadeResult(color, vec4<f32>(color.a));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return shade(in).color;
}
"#;

fn shader_source(dual_source: bool) -> String {
    if !dual_source {
        // When: `dual_source` is false, use the standard fragment wrapper without the optional WGSL extension.
        return SHADER.to_string();
    }
    let body = SHADER
        .strip_suffix(
            "\n@fragment\nfn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {\n    return shade(in).color;\n}\n",
        )
        .expect("standard shader wrapper remains canonical");
    format!(
        "enable dual_source_blending;\n{body}\n\nstruct FragmentOutput {{\n    @location(0) @blend_src(0) color: vec4<f32>,\n    @location(0) @blend_src(1) factor: vec4<f32>,\n}};\n\n@fragment\nfn fs_main(in: VertexOutput) -> FragmentOutput {{\n    let shaded = shade(in);\n    return FragmentOutput(shaded.color, shaded.factor);\n}}\n"
    )
}

#[cfg(test)]
#[path = "wezterm_pipeline_tests.rs"]
mod wezterm_pipeline_tests;
