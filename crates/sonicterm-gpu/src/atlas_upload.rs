//! GPU-side wrapper around [`sonicterm_text::glyph_atlas::GlyphAtlas`].
//!
//! Owns a wgpu texture/view/sampler plus the bind group that
//! [`crate::text_pipeline::TextPipeline`] samples from. This used to live
//! inside `glyph_atlas.rs` itself, but in PR 4 the atlas was moved into
//! the headless `sonicterm-text` crate, which does not depend on wgpu. The
//! GPU wrapper stays here in `sonicterm-shared` (later: `sonicterm-gpu`).

use sonicterm_text::glyph_atlas::{DirtyRect, GlyphAtlas, BYTES_PER_PIXEL};

/// Work completed by one atlas synchronization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtlasUploadStats {
    /// Number of dirty rectangles emitted by the CPU atlas.
    pub dirty_rects: usize,
    /// Number of texture writes after coalescing.
    pub upload_calls: usize,
    /// Total tightly packed bytes submitted to the queue.
    pub uploaded_bytes: usize,
}

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
    dirty_rects: Vec<DirtyRect>,
    coalesced_rects: Vec<DirtyRect>,
    scratch: Vec<u8>,
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
    /// Allocate a GPU texture sized to match `atlas`. Tiles are initialized
    /// lazily by [`Self::sync`] when the CPU atlas marks them dirty; avoiding
    /// a full-atlas seed upload keeps startup/rebuild staging memory bounded.
    /// `bgl` must match `crate::text_pipeline::TextPipeline::bind_group_layout`.
    pub fn new(device: &wgpu::Device, atlas: &GlyphAtlas, bgl: &wgpu::BindGroupLayout) -> Self {
        Self::new_sized(device, atlas.width(), atlas.height(), bgl)
    }

    /// Allocate a GPU texture with explicit dimensions.
    pub fn new_sized(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        bgl: &wgpu::BindGroupLayout,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sonic-glyph-atlas"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
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
        Self {
            texture,
            view,
            sampler,
            bind_group,
            width,
            height,
            dirty_rects: Vec::new(),
            coalesced_rects: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Push every dirty rect since the last sync to the GPU. Drains
    /// the atlas's dirty list.
    pub fn sync(&mut self, queue: &wgpu::Queue, atlas: &mut GlyphAtlas) -> AtlasUploadStats {
        atlas.drain_dirty_rects_into(&mut self.dirty_rects);
        let dirty_rects = self.dirty_rects.len();
        if dirty_rects == 0 {
            return AtlasUploadStats::default();
        }
        coalesce_dirty_rects(&self.dirty_rects, &mut self.coalesced_rects);
        let atlas_w = atlas.width();
        let pixels = atlas.pixels();
        let mut uploaded_bytes = 0usize;
        for &rect in &self.coalesced_rects {
            copy_rect_into_scratch(pixels, atlas_w, rect, &mut self.scratch);
            uploaded_bytes = uploaded_bytes.saturating_add(self.scratch.len());
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: rect.x, y: rect.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &self.scratch,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(rect.w * BYTES_PER_PIXEL),
                    rows_per_image: Some(rect.h),
                },
                wgpu::Extent3d { width: rect.w, height: rect.h, depth_or_array_layers: 1 },
            );
        }
        AtlasUploadStats { dirty_rects, upload_calls: self.coalesced_rects.len(), uploaded_bytes }
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

fn coalesce_dirty_rects(input: &[DirtyRect], out: &mut Vec<DirtyRect>) {
    out.clear();
    out.extend(input.iter().copied().filter(|rect| rect.w > 0 && rect.h > 0));
    out.sort_unstable_by_key(|rect| (rect.y, rect.h, rect.x, rect.w));
    merge_sorted_rects(
        out,
        |left, right| {
            left.y == right.y && left.h == right.h && right.x <= left.x.saturating_add(left.w)
        },
        |left, right| {
            left.w = left.x.saturating_add(left.w).max(right.x.saturating_add(right.w)) - left.x;
        },
    );
    out.sort_unstable_by_key(|rect| (rect.x, rect.w, rect.y, rect.h));
    merge_sorted_rects(
        out,
        |top, bottom| {
            top.x == bottom.x && top.w == bottom.w && bottom.y <= top.y.saturating_add(top.h)
        },
        |top, bottom| {
            top.h = top.y.saturating_add(top.h).max(bottom.y.saturating_add(bottom.h)) - top.y;
        },
    );
}

fn merge_sorted_rects(
    rects: &mut Vec<DirtyRect>,
    compatible: impl Fn(DirtyRect, DirtyRect) -> bool,
    merge: impl Fn(&mut DirtyRect, DirtyRect),
) {
    let mut write = 0usize;
    for read in 0..rects.len() {
        let next = rects[read];
        if write > 0 && compatible(rects[write - 1], next) {
            merge(&mut rects[write - 1], next);
        } else {
            rects[write] = next;
            write += 1;
        }
    }
    rects.truncate(write);
}

fn copy_rect_into_scratch(pixels: &[u8], atlas_width: u32, rect: DirtyRect, scratch: &mut Vec<u8>) {
    let bpp = BYTES_PER_PIXEL as usize;
    let row_bytes = rect.w as usize * bpp;
    scratch.clear();
    scratch.reserve(rect.h as usize * row_bytes);
    for row in 0..rect.h {
        let offset = ((rect.y + row) * atlas_width + rect.x) as usize * bpp;
        scratch.extend_from_slice(&pixels[offset..offset + row_bytes]);
    }
}

#[cfg(test)]
#[path = "atlas_upload_tests.rs"]
mod atlas_upload_tests;
