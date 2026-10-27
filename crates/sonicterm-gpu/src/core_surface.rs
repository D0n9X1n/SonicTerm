//! Surface responsibilities of the renderer, preserving its single owner.

use super::*;

pub(super) fn full_surface_rect(width: u32, height: u32) -> PixelRect {
    PixelRect { x: 0, y: 0, w: width.max(1), h: height.max(1) }
}

pub(crate) const MAX_SURFACE_DIMENSION: u32 = 16_384;

pub(super) const MAX_SURFACE_BYTES: u64 = 160 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedSurfaceSize {
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

/// What a requested surface resize did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeOutcome {
    /// The surface took a new size.
    Changed,
    /// The validated size equals the configured one; nothing was reconfigured.
    Unchanged,
    /// The size is unrepresentable; the previous surface stays configured.
    Rejected,
}

/// Classify a resize from the configured `(width, height)` to a validated candidate.
#[must_use]
pub(crate) fn classify_resize(
    configured: (u32, u32),
    candidate: Option<&ValidatedSurfaceSize>,
) -> ResizeOutcome {
    match candidate {
        None => ResizeOutcome::Rejected,
        Some(size) if (size.width, size.height) == configured => ResizeOutcome::Unchanged,
        Some(_) => ResizeOutcome::Changed,
    }
}

#[must_use]
pub(crate) fn validated_surface_size(
    width: u32,
    height: u32,
    device_max_dimension: u32,
) -> Option<ValidatedSurfaceSize> {
    let width = width.max(1);
    let height = height.max(1);
    let max_dimension = device_max_dimension.clamp(1, MAX_SURFACE_DIMENSION);
    if width > max_dimension || height > max_dimension {
        // When: an axis exceeds `max_dimension` — configuring the surface
        // anyway is a driver-level failure, so the caller rejects the size.
        return None;
    }
    let bytes = u64::from(width).checked_mul(u64::from(height))?.checked_mul(4)?;
    if bytes > MAX_SURFACE_BYTES {
        // When: `bytes > MAX_SURFACE_BYTES` — both axes legal, their product
        // not. The ceiling bounds one surface against the process budget.
        return None;
    }
    Some(ValidatedSurfaceSize { width, height, bytes: usize::try_from(bytes).ok()? })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_retained_frame(
    pipeline: &mut WeztermPipeline,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    view: &TextureView,
    image_atlas: &wgpu::BindGroup,
    glyph_atlas: &wgpu::BindGroup,
    sw: f32,
    sh: f32,
    first: bool,
    damage: PixelRect,
    background: wgpu::Color,
    subpixel_aa: SubpixelAaMode,
    quads: &[QuadInstance],
    images: &[ImageInstance],
    glyphs: &[GlyphInstance],
    overlay_quads: &[QuadInstance],
    overlay_glyphs: &[GlyphInstance],
) {
    let full = first || damage == full_surface_rect(sw as u32, sh as u32);
    let reset = (!full).then(|| {
        QuadInstance::sharp(
            px_to_ndc(damage.x as f32, damage.y as f32, damage.w as f32, damage.h as f32, sw, sh),
            [background.r as f32, background.g as f32, background.b as f32, background.a as f32],
        )
    });
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("sonic-retained-pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: Operations {
                load: if full {
                    LoadOp::Clear(background)
                } else {
                    // When: `full` is false, replace only the damaged pixels and retain the rest of the attachment.
                    LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_scissor_rect(
        damage.x.max(0) as u32,
        damage.y.max(0) as u32,
        damage.w.max(1),
        damage.h.max(1),
    );
    pipeline.draw_frame(
        device,
        queue,
        &mut pass,
        image_atlas,
        glyph_atlas,
        sw,
        sh,
        subpixel_aa,
        reset,
        quads,
        images,
        glyphs,
        overlay_quads,
        overlay_glyphs,
    );
}

pub(super) fn create_frame_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: TextureFormat,
    copy_source: bool,
) -> (Texture, TextureView) {
    let mut usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    if copy_source {
        // Only a test that enabled retained-frame readback copies the frame out.
        usage |= TextureUsages::COPY_SRC;
    }
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("sonic-retained-frame"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    (texture, view)
}

/// The retained frame texture's extent: 1x1 under the Windows software presenter, which presents the
/// CPU frame and never samples it, else the surface size, never zero.
#[must_use]
pub(super) fn frame_texture_extent(
    software_presenter: bool,
    width: u32,
    height: u32,
) -> (u32, u32) {
    if software_presenter {
        (1, 1)
    } else {
        // When: !software_presenter, the GPU presenter draws into the texture at width x height.
        (width.max(1), height.max(1))
    }
}

/// GPU bytes a frame texture of `extent` holds, at 4 bytes per texel.
#[must_use]
pub(super) fn frame_texture_payload_bytes(extent: (u32, u32)) -> u64 {
    atlas_payload_bytes(extent.0, extent.1)
}

/// Build the retained frame texture sized for the presenter; the only caller of `create_frame_texture`.
/// `copy_source` is the test-only readback flag; production passes false.
pub(super) fn build_frame_texture(
    device: &wgpu::Device,
    software_presenter: bool,
    width: u32,
    height: u32,
    format: TextureFormat,
    copy_source: bool,
) -> (Texture, TextureView) {
    let (texture_width, texture_height) = frame_texture_extent(software_presenter, width, height);
    create_frame_texture(device, texture_width, texture_height, format, copy_source)
}

impl GpuRenderer {
    /// Checked resize used by window-event paths that must react to rejection.
    ///
    /// While the device is stopped, a valid size is recorded and `true` is
    /// returned without reconfiguring the surface.
    #[must_use]
    pub fn try_resize(&mut self, width: u32, height: u32) -> bool {
        self.try_resize_outcome(width, height) != ResizeOutcome::Rejected
    }
}

impl GpuRenderer {
    /// Checked resize that reports whether the surface actually changed size.
    ///
    /// `Unchanged` reconfigures nothing and keeps the retained frame; only `Changed` resizes.
    #[must_use]
    pub fn try_resize_outcome(&mut self, width: u32, height: u32) -> ResizeOutcome {
        let max_dimension =
            self.device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let Some(size) = validated_surface_size(width, height, max_dimension) else {
            // When: `validated_surface_size` returns None. The old surface
            // stays configured — refusing is recoverable, resizing is not.
            tracing::error!(
                target: "memory",
                requested_width = width,
                requested_height = height,
                current_width = self.config.width,
                current_height = self.config.height,
                max_dimension,
                max_bgra_bytes = MAX_SURFACE_BYTES,
                "renderer rejected unsafe surface resize"
            );
            return ResizeOutcome::Rejected;
        };
        if classify_resize((self.config.width, self.config.height), Some(&size))
            == ResizeOutcome::Unchanged
        {
            // When: the validated size equals the configured one — common on
            // scale events. Reconfiguring would drop both caches for nothing.
            return ResizeOutcome::Unchanged;
        }
        let planned_frame_texture =
            frame_texture_extent(self.uses_windows_software_presenter(), size.width, size.height);
        tracing::debug!(
            target: "memory",
            window = self.render_timing_label,
            old_width = self.config.width,
            old_height = self.config.height,
            requested_width = width,
            requested_height = height,
            width = size.width,
            height = size.height,
            bgra_bytes = size.bytes,
            software_rendering = self.software_rendering,
            frame_texture_width = planned_frame_texture.0,
            frame_texture_height = planned_frame_texture.1,
            frame_texture_payload_bytes = frame_texture_payload_bytes(planned_frame_texture),
            payload_estimate = true,
            "renderer surface resize accepted"
        );
        self.config.width = size.width;
        self.config.height = size.height;
        if let Some(_scope) = self.device_errors.enter_gpu_work("try_resize") {
            // A stopped device records the size only; the surface is not reconfigured.
            self.surface.configure(&self.device, &self.config);
            self.record_surface_request();
            self.rebuild_frame_texture();
        }
        // Geometry change → force the next frame to actually render.
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        // Field clips and caret rects are in the old surface's pixels.
        self.presented_fields.clear();
        // Cell layout and absolute-row positioning both change with
        // the surface size; cached glyph instances would land at the
        // wrong NDC coordinates.
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // Post-glyphon there is no persistent text buffer to
        // resize — chrome strings are re-shaped through
        // `chrome_text::layout` on every frame, picking up the new
        // surface dims via the per-call `(sw, sh)` parameter. The
        // legacy `*_buffer.set_size(...)` block that lived here is
        // gone with the glyphon plumbing.
        ResizeOutcome::Changed
    }
}

impl GpuRenderer {
    /// Update the resolved no-GPU degrade state after a config reload.
    /// A transition invalidates the retained frame and reconfigures the
    /// surface with the software-render present tweaks. While the device is
    /// stopped, only the flag and present settings are recorded; the surface
    /// and GPU atlas uploads are left alone.
    pub fn set_software_render_degrade(&mut self, degrade: bool) {
        if self.software_render_degrade == degrade {
            // When: `software_render_degrade == degrade`. The body below
            // reconfigures the surface and may drop both atlases.
            return;
        }
        let used_software_presenter = self.uses_windows_software_presenter();
        self.software_render_degrade = degrade;
        if degrade {
            // Fifo, opaque compositing, and one frame of latency trade
            // smoothness for the lowest CPU cost per presented frame.
            self.config.present_mode = PresentMode::Fifo;
            self.config.alpha_mode = CompositeAlphaMode::Opaque;
            self.config.desired_maximum_frame_latency = 1;
        } else {
            // When: leaving degrade. The modes captured at construction are
            // restored, so a backend without Mailbox does not acquire it here.
            self.config.present_mode = self.hardware_present_mode;
            self.config.alpha_mode = self.hardware_alpha_mode;
            self.config.desired_maximum_frame_latency = 2;
            #[cfg(target_os = "windows")]
            {
                self.software_frame = None;
            }
        }
        if let Some(_scope) = self.device_errors.enter_gpu_work("set_software_render_degrade") {
            // A stopped device records the flag only; the surface is not reconfigured.
            self.surface.configure(&self.device, &self.config);
            self.record_surface_request();
        }
        let uses_software_presenter = self.uses_windows_software_presenter();
        if used_software_presenter != uses_software_presenter {
            // The software and GPU presenters size their atlas textures
            // differently, so cached UVs do not survive the transition.
            self.row_glyph_cache.invalidate_all();
            self.line_quad_cache.invalidate_all();
            if !uses_software_presenter {
                // GPU textures must grow from the placeholder dimensions the
                // software path left behind.
                self.reset_glyph_atlas_in_place("software_to_gpu");
                self.reset_image_atlas();
                self.glyph_atlas_retry_without_eviction = false;
            }
            self.rebuild_glyph_upload_if_needed();
            self.rebuild_image_upload_if_needed();
            // The GDI presenter never samples the frame texture, so it shrinks to 1x1; leaving GDI
            // grows it once. A stopped device keeps it, and recovery builds it from this flag.
            self.rebuild_frame_texture();
            let frame_texture = self.frame_texture_extent();
            tracing::debug!(
                target: "memory",
                renderer_role = self.render_timing_label,
                window_id = ?self.window.id(),
                software_presenter = uses_software_presenter,
                frame_texture_width = frame_texture.0,
                frame_texture_height = frame_texture.1,
                frame_texture_payload_bytes = frame_texture_payload_bytes(frame_texture),
                glyph_gpu_width = self.glyph_upload.width(),
                glyph_gpu_height = self.glyph_upload.height(),
                glyph_gpu_payload_bytes = self.glyph_upload.payload_bytes(),
                image_gpu_width = self.image_upload.width(),
                image_gpu_height = self.image_upload.height(),
                image_gpu_payload_bytes = self.image_upload.payload_bytes(),
                glyph_resident = self.glyph_atlas.len(),
                image_resident = self.image_atlas.len(),
                payload_estimate = true,
                "renderer software/GPU atlas transition"
            );
        }
        self.last_frame_key = None;
        self.log_subpixel_aa_policy();
        self.request_window_redraw();
    }
}

impl GpuRenderer {
    pub(super) fn uses_windows_software_presenter(&self) -> bool {
        cfg!(target_os = "windows") && self.software_render_degrade
    }
}

impl GpuRenderer {
    /// Size the frame texture for the current presenter and surface, inside the device gate.
    ///
    /// A stopped device refuses; `commit_rebind` builds the texture later from the current flag.
    pub(super) fn rebuild_frame_texture(&mut self) {
        let Some(_scope) = self.device_errors.enter_gpu_work("frame_texture.rebuild") else {
            // When: enter_gpu_work refuses, the stopped device keeps its texture until recovery.
            return;
        };
        let (frame_texture, frame_view) = build_frame_texture(
            &self.device,
            self.uses_windows_software_presenter(),
            self.config.width,
            self.config.height,
            self.config.format,
            self.retained_frame_readback,
        );
        self.frame_texture = frame_texture;
        self.frame_view = frame_view;
        self.frame_texture_trimmed = false;
        self.frame_texture_installs += 1;
        self.record_frame_texture_request();
    }
}

impl GpuRenderer {
    /// Record the configured surface in the graphics ledger: one drawable's requested bytes times
    /// the drawables the backend keeps for the configured latency, marked as an estimate where the
    /// backend does not fix that count (Vulkan, GL). Follows every `configure`.
    pub(crate) fn record_surface_request(&self) {
        let drawables = crate::graphics_ledger::surface_drawables(
            self.adapter.get_info().backend,
            self.config.desired_maximum_frame_latency,
            None,
        );
        let drawable_bytes = crate::graphics_ledger::surface_drawable_bytes(
            self.config.width,
            self.config.height,
            self.config.format,
        );
        self.graphics_ledger.replace_request(
            crate::graphics_ledger::GraphicsSlot::Surface,
            drawable_bytes.saturating_mul(drawables.count),
            drawables.count,
            drawables.estimated,
        );
    }
}

impl GpuRenderer {
    /// Record the installed frame texture's requested bytes in the graphics ledger.
    pub(crate) fn record_frame_texture_request(&self) {
        self.graphics_ledger.replace(
            crate::graphics_ledger::GraphicsSlot::FrameTexture,
            frame_texture_payload_bytes(self.frame_texture_extent()),
            1,
        );
    }
}

impl GpuRenderer {
    /// Record the installed glyph atlas texture's requested bytes in the graphics ledger.
    pub(crate) fn record_glyph_atlas_request(&self) {
        self.graphics_ledger.replace(
            crate::graphics_ledger::GraphicsSlot::GlyphAtlas,
            self.glyph_upload.payload_bytes(),
            1,
        );
    }
}

impl GpuRenderer {
    /// Record the installed image atlas texture's requested bytes in the graphics ledger.
    pub(crate) fn record_image_atlas_request(&self) {
        self.graphics_ledger.replace(
            crate::graphics_ledger::GraphicsSlot::ImageAtlas,
            self.image_upload.payload_bytes(),
            1,
        );
    }
}

impl GpuRenderer {
    /// Restore a trimmed frame texture to the surface size before a GPU present; returns false
    /// when the device refused the rebuild, so the caller presents nothing and retries later.
    pub(crate) fn ensure_frame_texture(&mut self) -> bool {
        if !self.frame_texture_trimmed || self.uses_windows_software_presenter() {
            // When: frame_texture_trimmed is clear or uses_windows_software_presenter, the texture already fits.
            return true;
        }
        self.rebuild_frame_texture();
        !self.frame_texture_trimmed
    }
}

#[cfg(test)]
#[path = "core_surface_tests.rs"]
mod core_surface_tests;
