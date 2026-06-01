//! GPU-side wrapper around [`sonicterm_text::glyph_atlas::GlyphAtlas`].
//!
//! Owns a wgpu texture/view/sampler plus the bind group that
//! [`crate::text_pipeline::TextPipeline`] samples from. This used to live
//! inside `glyph_atlas.rs` itself, but in PR 4 the atlas was moved into
//! the headless `sonicterm-text` crate, which does not depend on wgpu. The
//! GPU wrapper stays here in `sonicterm-shared` (later: `sonicterm-gpu`).

use sonicterm_text::glyph_atlas::{GlyphAtlas, BYTES_PER_PIXEL};

/// GPU-side wrapper around [`GlyphAtlas`]. Owns a wgpu texture/view/
/// sampler plus the bind group that [`crate::text_pipeline::TextPipeline`]
/// samples from.
///
/// Per frame the renderer:
///   1. Calls `atlas.get_or_insert(...)` for each visible cell (this
///      mutates the CPU buffer + records dirty rects).
///   2. Calls `upload.sync(&queue, &mut atlas)` to push any new tiles
///      to the GPU. Subregion writes are cheap; the typical
///      steady-state frame uploads 0 bytes (atlas is warm).
///   3. Hands `upload.bind_group()` to the text pipeline draw call.
pub struct AtlasUpload {
    texture: wgpu::Texture,
    #[allow(dead_code)]
    view: wgpu::TextureView,
    #[allow(dead_code)]
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

#[doc(hidden)]
pub fn atlas_sampler_descriptor() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("sonic-glyph-atlas-sampler"),
        // Nearest is the right call for a monospace grid: pixels
        // line up to cell boundaries and linear filtering would
        // just blur tile edges. Rasterization already produced
        // anti-aliased coverage.
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    }
}

impl AtlasUpload {
    /// Allocate a GPU texture sized to match `atlas` and seed it with
    /// the atlas's current (probably empty) pixels. `bgl` must match
    /// `crate::text_pipeline::TextPipeline::bind_group_layout`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &GlyphAtlas,
        bgl: &wgpu::BindGroupLayout,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sonic-glyph-atlas"),
            size: wgpu::Extent3d {
                width: atlas.width(),
                height: atlas.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Seed-write the whole texture once. Cheap (16 MiB) and means
        // the first frame can render against a black atlas if nothing
        // has been requested yet, rather than tripping a "texture is
        // in undefined state" validation error on some backends.
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            atlas.pixels(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width() * BYTES_PER_PIXEL),
                rows_per_image: Some(atlas.height()),
            },
            wgpu::Extent3d {
                width: atlas.width(),
                height: atlas.height(),
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&atlas_sampler_descriptor());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sonic-glyph-atlas-bg"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self { texture, view, sampler, bind_group, width: atlas.width(), height: atlas.height() }
    }

    /// Push every dirty rect since the last sync to the GPU. Drains
    /// the atlas's dirty list.
    pub fn sync(&self, queue: &wgpu::Queue, atlas: &mut GlyphAtlas) {
        let rects = atlas.take_dirty_rects();
        if rects.is_empty() {
            return;
        }
        let atlas_w = atlas.width();
        let pixels = atlas.pixels();
        let bpp = BYTES_PER_PIXEL as usize;
        for r in rects {
            // Copy out the subrect into a tightly-packed buffer so
            // bytes_per_row == r.w * bpp. write_texture requires that.
            let mut sub = Vec::with_capacity((r.w * r.h) as usize * bpp);
            for row in 0..r.h {
                let off = ((r.y + row) * atlas_w + r.x) as usize * bpp;
                sub.extend_from_slice(&pixels[off..off + r.w as usize * bpp]);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: r.x, y: r.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &sub,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(r.w * BYTES_PER_PIXEL),
                    rows_per_image: Some(r.h),
                },
                wgpu::Extent3d { width: r.w, height: r.h, depth_or_array_layers: 1 },
            );
        }
    }

    /// Bind group exposing the atlas texture + sampler to the text pipeline.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Atlas texture width in pixels — matches the CPU `GlyphAtlas`.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Atlas texture height in pixels — matches the CPU `GlyphAtlas`.
    pub fn height(&self) -> u32 {
        self.height
    }
}
