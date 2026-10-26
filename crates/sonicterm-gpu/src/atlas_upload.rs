//! GPU-side wrapper around [`sonicterm_text::glyph_atlas::GlyphAtlas`].
//!
//! Owns one wgpu texture, unorm and sRGB views, nearest and linear samplers,
//! and the bind groups that [`crate::wezterm_pipeline::WeztermPipeline`] samples.
//!
//! The atlas itself lives in the headless `sonicterm-text` crate, which does
//! not depend on wgpu; only this GPU-side wrapper does. That split is what
//! lets the atlas be exercised without a device.

use sonicterm_text::glyph_atlas::{AtlasPixelKind, DirtyRect, GlyphAtlas, BYTES_PER_PIXEL};

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

/// Rect-list capacity above which a synchronized list is shrunk, so the resident-wide list a
/// growth re-upload builds is held for one sync rather than for the renderer's life.
pub(crate) const UPLOAD_LIST_SHRINK_ABOVE: usize = 1024;
/// Capacity a released rect list keeps; ordinary frames dirty fewer rects than this.
pub(crate) const UPLOAD_LIST_RETAINED: usize = 64;

/// The upload's reusable CPU storage: the drained dirty rects, their same-kind coalescing, and
/// the staging buffer each texture write is copied through. Device-free, so its release policy is
/// testable without a queue.
#[derive(Debug, Default)]
pub(crate) struct UploadLists {
    dirty_rects: Vec<DirtyRect>,
    coalesced_rects: Vec<DirtyRect>,
    scratch: Vec<u8>,
}

/// Texture binding assembled for this atlas mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtlasBindingKind {
    /// One sRGB view with linear filtering for packed inline images.
    Image,
    /// Unorm coverage and sRGB color views, both with nearest filtering.
    Glyph,
}

/// GPU-side wrapper around [`GlyphAtlas`]. Owns one texture with unorm and sRGB
/// views plus independent nearest and linear sampler bindings.
///
/// Per frame the renderer:
///   1. Calls `atlas.get_or_insert(...)` for each visible cell (this
///      mutates the CPU buffer + records kind-bearing dirty rects).
///   2. Calls `upload.sync(&queue, &mut atlas)` to push any new tiles
///      to the GPU. Subregion writes are cheap; the typical
///      steady-state frame uploads 0 bytes (atlas is warm).
///   3. Hands the role-specific bind group to the unified draw call.
pub struct AtlasUpload {
    texture: wgpu::Texture,
    #[allow(dead_code)]
    // The bind group retains this view; sibling tests also verify shared texture identity.
    coverage_view: wgpu::TextureView,
    #[allow(dead_code)]
    // The bind group retains this view; sibling tests also verify shared texture identity.
    color_view: wgpu::TextureView,
    #[allow(dead_code)] // The glyph bind group owns nearest-filtering policy for both views.
    nearest_sampler: wgpu::Sampler,
    #[allow(dead_code)]
    // The image bind group owns linear-filtering policy independently of glyphs.
    linear_sampler: wgpu::Sampler,
    binding_kind: AtlasBindingKind,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    lists: UploadLists,
}

/// Build the nearest-filtered sampler descriptor used for glyph-atlas pixels.
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

fn image_atlas_sampler_descriptor() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("sonic-image-atlas-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    }
}

impl AtlasUpload {
    /// Allocate a GPU texture sized to match `atlas` and expose the requested binding role.
    #[cfg(test)]
    pub(crate) fn new(
        device: &wgpu::Device,
        atlas: &GlyphAtlas,
        bgl: &wgpu::BindGroupLayout,
        binding_kind: AtlasBindingKind,
    ) -> Self {
        Self::new_sized(device, atlas.width(), atlas.height(), bgl, binding_kind)
    }

    /// Allocate a GPU texture with explicit dimensions and one role-specific bind group.
    pub(crate) fn new_sized(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        bgl: &wgpu::BindGroupLayout,
        binding_kind: AtlasBindingKind,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sonic-atlas"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Bgra8UnormSrgb],
        });
        let coverage_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("sonic-atlas-coverage-view"),
            format: Some(wgpu::TextureFormat::Bgra8Unorm),
            ..Default::default()
        });
        let color_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("sonic-atlas-color-view"),
            format: Some(wgpu::TextureFormat::Bgra8UnormSrgb),
            ..Default::default()
        });
        let nearest_sampler = device.create_sampler(&atlas_sampler_descriptor());
        let linear_sampler = device.create_sampler(&image_atlas_sampler_descriptor());
        let (label, entries) = match binding_kind {
            AtlasBindingKind::Image => (
                "sonic-image-atlas-bg",
                vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&linear_sampler),
                    },
                ],
            ),
            AtlasBindingKind::Glyph => (
                "sonic-glyph-atlas-bg",
                vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&coverage_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&nearest_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&nearest_sampler),
                    },
                ],
            ),
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: bgl,
            entries: &entries,
        });
        Self {
            texture,
            coverage_view,
            color_view,
            nearest_sampler,
            linear_sampler,
            binding_kind,
            bind_group,
            width,
            height,
            lists: UploadLists::default(),
        }
    }

    /// Push every kind-bearing dirty rect since the last sync to the GPU.
    pub(crate) fn sync(&mut self, queue: &wgpu::Queue, atlas: &mut GlyphAtlas) -> AtlasUploadStats {
        let texture = &self.texture;
        self.lists.sync_with(atlas, |rect, staged| {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: rect.x, y: rect.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                staged,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(rect.w * BYTES_PER_PIXEL),
                    rows_per_image: Some(rect.h),
                },
                wgpu::Extent3d { width: rect.w, height: rect.h, depth_or_array_layers: 1 },
            );
        })
    }

    /// Bytes this upload's CPU storage keeps: its rect lists and its staging buffer.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.lists.retained_bytes()
    }

    /// Release this upload's staging buffer and rect lists, for a covered window's trim.
    pub(crate) fn release_scratch(&mut self) {
        self.lists.release_scratch();
    }

    /// Unorm view used by mask and subpixel coverage consumers.
    #[cfg(test)]
    pub(crate) fn coverage_view(&self) -> &wgpu::TextureView {
        &self.coverage_view
    }

    /// sRGB view used by self-colored atlas consumers.
    #[cfg(test)]
    pub(crate) fn color_view(&self) -> &wgpu::TextureView {
        &self.color_view
    }

    /// Nearest sampler used by both glyph views.
    #[cfg(test)]
    pub(crate) fn nearest_sampler(&self) -> &wgpu::Sampler {
        &self.nearest_sampler
    }

    /// Bind group exposing an sRGB image view with linear filtering.
    pub(crate) fn image_bind_group(&self) -> &wgpu::BindGroup {
        debug_assert_eq!(self.binding_kind, AtlasBindingKind::Image);
        &self.bind_group
    }

    /// Bind group exposing unorm coverage and sRGB color glyph views with nearest filtering.
    pub(crate) fn glyph_bind_group(&self) -> &wgpu::BindGroup {
        debug_assert_eq!(self.binding_kind, AtlasBindingKind::Glyph);
        &self.bind_group
    }

    /// Retained GPU pixel payload for the single texture allocation.
    pub(crate) fn payload_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * u64::from(BYTES_PER_PIXEL)
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

#[cfg(test)]
pub(crate) use atlas_upload_tests::render_image_instances_readback;
#[cfg(test)]
pub(crate) use atlas_upload_tests::{render_glyph_readback_on, render_image_readback_on};
#[cfg(test)]
#[path = "atlas_upload_tests.rs"]
mod atlas_upload_tests;

#[path = "atlas_upload_staging.rs"]
mod staging;
