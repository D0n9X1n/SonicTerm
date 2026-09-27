//! Rebuilding a renderer's device-bound objects on a recovered GPU context.
//!
//! [`crate::core::GpuRenderer::prepare_rebind`] checks the candidate surface, then
//! builds the objects on the candidate context under that context's gate and changes
//! nothing on the renderer. [`crate::core::GpuRenderer::commit_rebind`] installs them:
//! it drops the old surface before it configures the new one, and resets every cache
//! tied to the old device's objects. The caller prepares and commits every live and
//! warm renderer in one event-loop callback and destroys the candidate if any step
//! fails; nothing here schedules, redraws, or destroys a context.
//!
//! This module is a child of `core` so it can replace the renderer's private fields
//! without widening their visibility.

use super::*;

use crate::device_errors::DeviceGate;

/// A renderer's device-bound objects rebuilt on a candidate context, not yet installed.
///
/// Preparing changes nothing on the renderer, and dropping this value releases only
/// the candidate objects. It keeps the candidate surface and its window until
/// [`crate::core::GpuRenderer::commit_rebind`] installs them.
pub struct PreparedRebind {
    context: GpuSharedContext,
    surface: CandidateSurface,
    software_rendering: bool,
    software_render_degrade: bool,
    hardware_present_mode: PresentMode,
    width: u32,
    height: u32,
    present_pipeline: WeztermPipeline,
    frame_texture: Texture,
    frame_view: TextureView,
    frame_blitter: wgpu::util::TextureBlitter,
    glyph_upload: AtlasUpload,
    image_upload: AtlasUpload,
}

/// What a candidate surface must support to keep this window's format and native
/// alpha under the current software-render degrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SurfaceNeeds {
    format: TextureFormat,
    alpha_mode: CompositeAlphaMode,
    degrade: bool,
}

/// The hardware present mode for a candidate surface offering `formats`,
/// `present_modes`, `alpha_modes`, and `usages`, or an error when it cannot keep the
/// window's format, effective alpha, or render-attachment use. Mailbox is preferred
/// and Fifo is the fallback; the mode in effect must be offered. Nothing is
/// substituted: a candidate that would change the window's colors or backdrop is refused.
fn supported_present_mode(
    needs: SurfaceNeeds,
    formats: &[TextureFormat],
    present_modes: &[PresentMode],
    alpha_modes: &[CompositeAlphaMode],
    usages: TextureUsages,
) -> Result<PresentMode> {
    if !formats.contains(&needs.format) {
        // When: `formats` lacks the window's format, its colors cannot be kept.
        return Err(anyhow!("recovery candidate surface lacks the window's format"));
    }
    let alpha = if needs.degrade { CompositeAlphaMode::Opaque } else { needs.alpha_mode };
    if !alpha_modes.contains(&alpha) {
        // When: `alpha_modes` lacks the effective `alpha`, the window's backdrop would change.
        return Err(anyhow!("recovery candidate surface lacks the window's alpha mode"));
    }
    if !usages.contains(TextureUsages::RENDER_ATTACHMENT) {
        // When: `usages` lacks `RENDER_ATTACHMENT`, the renderer cannot draw into the surface.
        return Err(anyhow!("recovery candidate surface cannot be a render attachment"));
    }
    let hardware = if present_modes.contains(&PresentMode::Mailbox) {
        PresentMode::Mailbox
    } else {
        PresentMode::Fifo
    };
    let effective = if needs.degrade { PresentMode::Fifo } else { hardware };
    if !present_modes.contains(&effective) {
        // When: `present_modes` lacks the `effective` mode, the surface cannot present.
        return Err(anyhow!("recovery candidate surface lacks a supported present mode"));
    }
    Ok(hardware)
}

/// Refuse a candidate surface made on another instance or for another window, before
/// anything asks the surface for its capabilities.
fn candidate_identity<I: PartialEq, W: PartialEq>(
    instance: &I,
    expected_instance: &I,
    window: W,
    expected_window: W,
) -> Result<()> {
    if instance != expected_instance {
        // When: `instance` differs from `expected_instance`, the surface is on another instance.
        return Err(anyhow!("candidate surface was made on another instance"));
    }
    if window != expected_window {
        // When: `window` differs from `expected_window`, the surface would draw elsewhere.
        return Err(anyhow!("candidate surface belongs to another window"));
    }
    Ok(())
}

impl GpuRenderer {
    /// Build this renderer's device-bound objects on `context` without changing the
    /// renderer: the present pipeline, the retained frame texture, view, and blitter,
    /// and both atlas uploads.
    ///
    /// The candidate surface must come from `context`'s own instance and this
    /// renderer's window; only then do the adapter's support and the surface's fresh
    /// capabilities decide whether it keeps the renderer's format and native alpha.
    /// The native alpha is kept rather than re-read from configuration, because a
    /// backdrop setting applies only to new windows. `software_render_mode` is the
    /// current setting, and the window's current size is read here.
    ///
    /// `surface` is the requesting window's surface from
    /// [`crate::core::RecoveredContext::into_parts`]; with `None`, a surface is created
    /// for this renderer's window on the context's instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the candidate stopped accepting work; when the surface
    /// belongs to another instance or window, is unsupported, or cannot keep the
    /// format, effective alpha, or a present mode; when the window does not fit the
    /// candidate's limits; or when a build raised a contained error. The renderer is
    /// unchanged.
    pub fn prepare_rebind(
        &self,
        context: &GpuSharedContext,
        surface: Option<CandidateSurface>,
        software_render_mode: SoftwareRenderMode,
    ) -> Result<PreparedRebind> {
        let errors = &context.device_errors;
        let prepare_scope = errors
            .enter_gpu_work("recovery.prepare")
            .ok_or_else(|| anyhow!("recovery candidate stopped accepting work"))?;
        let surface = match surface {
            Some(surface) => surface,
            None => self.candidate_surface(&context.instance)?,
        };
        candidate_identity(
            &surface.instance,
            &context.instance,
            surface.window.id(),
            self.window.id(),
        )?;
        if !context.adapter.is_surface_supported(&surface.surface) {
            // When: `is_surface_supported` is false, the candidate adapter cannot present here.
            return Err(anyhow!("recovery candidate adapter cannot present to this window"));
        }
        let capabilities = surface.surface.get_capabilities(&context.adapter);
        let software_rendering = detect_software_rendering(&context.adapter.get_info());
        let software_render_degrade =
            software_render_degrade_from(software_render_mode, software_rendering);
        let needs = SurfaceNeeds {
            format: self.config.format,
            alpha_mode: self.hardware_alpha_mode,
            degrade: software_render_degrade,
        };
        let hardware_present_mode = supported_present_mode(
            needs,
            &capabilities.formats,
            &capabilities.present_modes,
            &capabilities.alpha_modes,
            capabilities.usages,
        )?;
        let size = self.window.inner_size();
        let max_dimension =
            context.device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let validated =
            validated_surface_size(size.width, size.height, max_dimension).ok_or_else(|| {
                anyhow!("window surface does not fit the recovery candidate's limits")
            })?;
        prepare_scope.set_operation("recovery.prepare.resources");
        let format = self.config.format;
        // Size-comparing rebuild helpers would skip an equal-size upload, so every
        // object is built here on the candidate device.
        let present_pipeline = WeztermPipeline::new(&context.device, format, 4096);
        let (frame_texture, frame_view) =
            create_frame_texture(&context.device, validated.width, validated.height, format);
        let frame_blitter = wgpu::util::TextureBlitter::new(&context.device, format);
        let software_presenter = cfg!(target_os = "windows") && software_render_degrade;
        let glyph_dimensions = desired_gpu_atlas_dimensions(software_presenter, &self.glyph_atlas);
        let image_dimensions = desired_gpu_atlas_dimensions(software_presenter, &self.image_atlas);
        let glyph_upload = AtlasUpload::new_sized(
            &context.device,
            glyph_dimensions.0,
            glyph_dimensions.1,
            present_pipeline.glyph_bind_group_layout(),
            AtlasBindingKind::Glyph,
        );
        let image_upload = AtlasUpload::new_sized(
            &context.device,
            image_dimensions.0,
            image_dimensions.1,
            present_pipeline.image_bind_group_layout(),
            AtlasBindingKind::Image,
        );
        if !errors.accepts_gpu_work() {
            // When: `accepts_gpu_work` fails after the builds, a candidate object is invalid.
            return Err(anyhow!("recovery candidate raised a contained GPU error while preparing"));
        }
        Ok(PreparedRebind {
            context: context.clone(),
            surface,
            software_rendering,
            software_render_degrade,
            hardware_present_mode,
            width: validated.width,
            height: validated.height,
            present_pipeline,
            frame_texture,
            frame_view,
            frame_blitter,
            glyph_upload,
            image_upload,
        })
    }

    /// Install `prepared`: replace the device handles, the device-bound objects, and
    /// the surface, reset every cache tied to the old device, then configure the new
    /// surface under the candidate's gate and return the gate read after it.
    ///
    /// Dropping the old surface unconfigures it before the new one is configured.
    /// Neither step is atomic, and the driver may block in either. Commit every live
    /// and warm renderer in one callback; if any commit fails, or a returned gate
    /// refuses work, destroy the candidate so no renderer presents on it. The caller
    /// issues the one redraw afterwards; the committed context is never destroyed here.
    ///
    /// # Errors
    ///
    /// Returns an error, changing nothing, when the prepared surface belongs to another
    /// instance or window, or its candidate stopped accepting work before the commit.
    pub fn commit_rebind(&mut self, prepared: PreparedRebind) -> Result<DeviceGate> {
        let PreparedRebind {
            context,
            surface,
            software_rendering,
            software_render_degrade,
            hardware_present_mode,
            width,
            height,
            present_pipeline,
            frame_texture,
            frame_view,
            frame_blitter,
            glyph_upload,
            image_upload,
        } = prepared;
        let errors = Arc::clone(&context.device_errors);
        let commit_scope = errors
            .enter_gpu_work("recovery.commit")
            .ok_or_else(|| anyhow!("recovery candidate stopped accepting work before commit"))?;
        candidate_identity(
            &surface.instance,
            &context.instance,
            surface.window.id(),
            self.window.id(),
        )?;
        // Dropping the old surface unconfigures it on its own device first, so the
        // window never holds two configured swapchains.
        self.surface = surface.surface;
        self.instance = context.instance;
        self.adapter = context.adapter;
        self.device = context.device;
        self.queue = context.queue;
        self.device_errors = context.device_errors;
        self.software_rendering = software_rendering;
        self.software_render_degrade = software_render_degrade;
        self.hardware_present_mode = hardware_present_mode;
        self.present_pipeline = present_pipeline;
        self.frame_texture = frame_texture;
        self.frame_view = frame_view;
        self.frame_blitter = frame_blitter;
        self.glyph_upload = glyph_upload;
        self.image_upload = image_upload;
        self.config.width = width;
        self.config.height = height;
        self.config.present_mode =
            if software_render_degrade { PresentMode::Fifo } else { hardware_present_mode };
        self.config.alpha_mode = if software_render_degrade {
            CompositeAlphaMode::Opaque
        } else {
            self.hardware_alpha_mode
        };
        self.config.desired_maximum_frame_latency = if software_render_degrade { 1 } else { 2 };
        self.reset_after_rebind();
        commit_scope.set_operation("recovery.configure");
        self.surface.configure(&self.device, &self.config);
        Ok(self.device_errors.gate())
    }

    /// Reset the CPU state tied to the old device's objects: both atlases, the
    /// UV-bearing row and line caches, retry and fault flags, the retained frame key,
    /// and the pane layout, so the next frame is a full first frame. Counters, fonts,
    /// metrics, and settings are kept. Inline-image instances are rebuilt every frame,
    /// so no durable image-UV cache exists to reset.
    fn reset_after_rebind(&mut self) {
        self.reset_glyph_atlas_in_place("device_recovery");
        self.reset_image_atlas();
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.glyph_atlas_retry_without_eviction = false;
        self.fault_invalid_glyph_upload = false;
        self.fault_frame_probe = None;
        #[cfg(target_os = "windows")]
        {
            self.fault_stop_before_cached_present = false;
            self.software_frame = None;
        }
        self.device_stop_reported = false;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
    }
}

#[cfg(test)]
#[path = "rebind_tests.rs"]
mod rebind_tests;
