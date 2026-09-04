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
    dirty_rects: Vec<DirtyRect>,
    coalesced_rects: Vec<DirtyRect>,
    scratch: Vec<u8>,
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
            dirty_rects: Vec::new(),
            coalesced_rects: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Push every kind-bearing dirty rect since the last sync to the GPU.
    pub(crate) fn sync(&mut self, queue: &wgpu::Queue, atlas: &mut GlyphAtlas) -> AtlasUploadStats {
        atlas.drain_dirty_rects_into(&mut self.dirty_rects);
        let dirty_rects = self.dirty_rects.len();
        if dirty_rects == 0 {
            // When: `dirty_rects` is zero, skip queue writes and report an idle synchronization.
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

fn coalesce_dirty_rects(input: &[DirtyRect], out: &mut Vec<DirtyRect>) {
    out.clear();
    out.extend(input.iter().copied().filter(|rect| rect.w > 0 && rect.h > 0));
    out.sort_unstable_by_key(|rect| (rect.kind, rect.y, rect.h, rect.x, rect.w));
    merge_sorted_rects(
        out,
        |left, right| {
            left.kind == right.kind
                && left.y == right.y
                && left.h == right.h
                && right.x <= left.x.saturating_add(left.w)
        },
        |left, right| {
            left.w = left.x.saturating_add(left.w).max(right.x.saturating_add(right.w)) - left.x;
        },
    );
    out.sort_unstable_by_key(|rect| (rect.kind, rect.x, rect.w, rect.y, rect.h));
    merge_sorted_rects(
        out,
        |top, bottom| {
            top.kind == bottom.kind
                && top.x == bottom.x
                && top.w == bottom.w
                && bottom.y <= top.y.saturating_add(top.h)
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
            // When: `next` is incompatible with the previous output rectangle, retain it as a new merge run.
            rects[write] = next;
            write += 1;
        }
    }
    rects.truncate(write);
}

fn copy_rect_into_scratch(pixels: &[u8], atlas_width: u32, rect: DirtyRect, scratch: &mut Vec<u8>) {
    let bpp = BYTES_PER_PIXEL as usize;
    let row_bytes = rect.w as usize * bpp;
    let required = rect.h as usize * row_bytes;
    scratch.clear();
    // Exact growth prevents a larger dirty rect from retaining Vec's geometric
    // spare capacity beyond one full-atlas staging payload.
    if required > scratch.capacity() {
        scratch.reserve_exact(required);
    }
    for row in 0..rect.h {
        let offset = ((rect.y + row) * atlas_width + rect.x) as usize * bpp;
        let source = &pixels[offset..offset + row_bytes];
        match rect.kind {
            AtlasPixelKind::Coverage => scratch.extend_from_slice(source),
            AtlasPixelKind::Color => {
                // When: Color is selected, decoded linear RGB must be premultiplied before sRGB texture storage.
                for pixel in source.as_chunks::<4>().0 {
                    let alpha = pixel[3];
                    if alpha == 0 {
                        // When: alpha is zero, canonicalize arbitrary encoded RGB to the unique transparent premultiplied value.
                        scratch.extend_from_slice(&[0, 0, 0, 0]);
                        continue;
                    }
                    let alpha_linear = alpha as f64 / 255.0;
                    for &encoded_premul in &pixel[..3] {
                        let straight_srgb = (encoded_premul as f64 / alpha as f64).clamp(0.0, 1.0);
                        let premul_linear =
                            crate::color::srgb_channel_to_linear(straight_srgb) * alpha_linear;
                        scratch.push(crate::color::linear_channel_to_srgb_u8(premul_linear as f32));
                    }
                    scratch.push(alpha);
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
pub(crate) use atlas_upload_tests::{render_glyph_readback_on, render_image_readback_on};
#[cfg(test)]
#[path = "atlas_upload_tests.rs"]
mod atlas_upload_tests;
