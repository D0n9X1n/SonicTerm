//! The presentation seam of [`GpuRenderer`].
//!
//! `render_frame` in `core` assembles one frame's layers and hands them to
//! exactly one presenter: GDI when software-render degradation is enabled on
//! Windows, otherwise the wgpu swapchain presenter. Every exit of a frame reports
//! a [`PresentOutcome`]; only [`PresentOutcome::Presented`] lets the caller
//! acknowledge the frame's plan, so every other outcome keeps its dirty rows.
//!
//! This module is a child of `core` so the presenters keep direct access to the
//! renderer's private fields without widening their visibility.

use super::*;

use crate::device_errors::{
    decide_frame_outcome, record_invalid_frame_command, DeviceGate, GpuWorkScope,
};

/// Why a frame had nothing new to present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// Every pane's parser lock was dropped by the caller, so no grid was supplied.
    NoPanes,
    /// The frame plan matches the retained frame key, so assembly was skipped.
    Unchanged,
    /// Software rendering found no pixel that needs new assembly. The new frame
    /// key is recorded, but the plan's grid revisions stay unacknowledged.
    Noop,
}

/// Why a surface handed back no texture for this frame.
///
/// These are surface states, not device loss: a surface retry is reported only
/// while the device still accepts work. A stopped device is always reported as
/// [`PresentOutcome::RenderingUnavailable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceRetryReason {
    /// Texture acquisition timed out.
    Timeout,
    /// The window is occluded, for example minimized or covered.
    Occluded,
    /// The swapchain no longer matched the window, so it was reconfigured.
    Outdated,
    /// The swapchain still worked but no longer matched the surface. Its
    /// texture was dropped unpresented and the surface was reconfigured.
    Suboptimal,
    /// The surface itself was lost and was recreated on the same device.
    SurfaceLost,
}

impl SurfaceRetryReason {
    /// How the wgpu presenter restores the surface before the next frame.
    const fn recovery(self) -> SurfaceRecovery {
        match self {
            Self::Timeout | Self::Occluded => SurfaceRecovery::Keep,
            Self::Outdated | Self::Suboptimal => SurfaceRecovery::Reconfigure,
            Self::SurfaceLost => SurfaceRecovery::Recreate,
        }
    }
}

/// The stopped device behind [`PresentOutcome::RenderingUnavailable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspendedContext {
    /// The generation of the device that stopped accepting work.
    pub generation: u64,
    /// The device state and destroy request read for this frame.
    pub gate: DeviceGate,
    /// Whether this frame carries the renderer's one-time stop report. The
    /// compatibility `render` returns an error only for that frame.
    pub reports_stop: bool,
}

impl SuspendedContext {
    /// The error the compatibility `render` returns for the reporting frame.
    pub fn stop_error(&self) -> anyhow::Error {
        anyhow::anyhow!(
            "GPU device {} stopped accepting work ({:?}, destroy requested: {})",
            self.generation,
            self.gate.state,
            self.gate.destroy_requested
        )
    }
}

/// What one [`render_with_outcome`](crate::core::GpuRenderer::render_with_outcome) call did
/// with its frame.
///
/// Only [`PresentOutcome::Presented`] means the frame passed the present boundary
/// and its plan was acknowledged. A cached reblit reaches the native presenter
/// without acknowledging a new plan; other outcomes never advance the successful
/// frame count. wgpu cannot report a later scanout failure.
#[derive(Debug)]
#[must_use = "inspect whether the frame presented, retried, or remained unacknowledged"]
pub enum PresentOutcome {
    /// Nothing new was presented; the reason says why.
    Skipped(SkipReason),
    /// The frame was unchanged, and the Windows GDI presenter blitted its
    /// retained CPU frame again. No plan was acknowledged.
    CachedReblit,
    /// The glyph atlas recycled a tile during assembly. The atlas was rebuilt
    /// with eviction disabled and another frame was requested.
    AtlasRetry,
    /// The surface handed back no texture. It was recovered as the reason
    /// describes, and another frame was requested.
    SurfaceRetry(SurfaceRetryReason),
    /// The device stopped accepting work before this frame was acknowledged.
    /// A frame whose device stopped during presentation may still show its
    /// pixels, but it is never acknowledged.
    RenderingUnavailable(SuspendedContext),
    /// The frame passed the presentation boundary and its plan was acknowledged.
    Presented,
    /// The frame failed with an error; its plan stays unacknowledged.
    Failed(anyhow::Error),
}

impl PresentOutcome {
    /// Map this outcome to the result [`render`](crate::core::GpuRenderer::render) returns.
    ///
    /// Every success that presents nothing stays `Ok(())`, a failure keeps its
    /// original error, and a stopped device returns its error only on the frame
    /// that carries the renderer's one-time stop report.
    pub fn into_render_result(self) -> anyhow::Result<()> {
        match self {
            Self::Failed(error) => Err(error),
            Self::RenderingUnavailable(context) if context.reports_stop => {
                Err(context.stop_error())
            }
            // Later stopped frames stay silent, so each renderer reports its stopped device once.
            Self::RenderingUnavailable(_) => Ok(()),
            Self::Skipped(_)
            | Self::CachedReblit
            | Self::AtlasRetry
            | Self::SurfaceRetry(_)
            | Self::Presented => Ok(()),
        }
    }
}

/// How the wgpu presenter restores a surface that handed back no texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRecovery {
    /// Keep the surface as it is configured.
    Keep,
    /// Reconfigure the existing surface.
    Reconfigure,
    /// Recreate the surface on the same device, then configure it.
    Recreate,
}

/// What a frame reports after its surface was recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRetryDisposition {
    /// The device still accepts work: request a redraw and report the surface retry.
    Retry,
    /// The device stopped and the surface was only kept: request a redraw and
    /// leave the one-time stop report to the next frame's device check.
    DeferStop,
    /// The device stopped while the surface was reconfigured or recreated:
    /// report the stop now and request no redraw.
    Stop,
}

/// Decide what a frame reports after its surface was recovered.
///
/// Pure, so the whole table is testable without a GPU. A stopped device is
/// never an ordinary surface retry, whatever the surface reported.
const fn surface_retry_disposition(
    reason: SurfaceRetryReason,
    after_recovery: DeviceGate,
) -> SurfaceRetryDisposition {
    if after_recovery.accepts_gpu_work() {
        // When: `after_recovery` accepts GPU work, only the surface failed, so the frame retries.
        return SurfaceRetryDisposition::Retry;
    }
    match reason.recovery() {
        SurfaceRecovery::Keep => SurfaceRetryDisposition::DeferStop,
        SurfaceRecovery::Reconfigure | SurfaceRecovery::Recreate => SurfaceRetryDisposition::Stop,
    }
}

/// One frame's render-timing laps: frame start, previous lap, and named laps in
/// milliseconds. Present only while `render_timing` debug logs are enabled.
pub(super) type FrameTiming = Option<(Instant, Instant, Vec<(&'static str, f32)>)>;

/// Record the time since the previous lap under `name`.
pub(super) fn lap(timing: &mut FrameTiming, name: &'static str) {
    if let Some((_, last, parts)) = timing.as_mut() {
        let now = Instant::now();
        parts.push((name, now.saturating_duration_since(*last).as_secs_f32() * 1000.0));
        *last = now;
    }
}

/// The drawable layers and geometry of one assembled frame.
///
/// It borrows no plan, grid, or parser guard: those stay with the caller, which
/// acknowledges the plan only after a presenter reports `Presented`.
pub(super) struct FrameLayers<'a> {
    /// Surface width in physical pixels.
    pub(super) surface_width: f32,
    /// Surface height in physical pixels.
    pub(super) surface_height: f32,
    /// Whether this is the retained frame's first draw.
    pub(super) first_frame: bool,
    /// The damage rectangle redrawn inside the retained frame.
    pub(super) damage: PixelRect,
    /// The subpixel antialiasing mode resolved for this frame.
    pub(super) subpixel_aa: SubpixelAaMode,
    /// Base quads.
    pub(super) quads: &'a [QuadInstance],
    /// Inline-image instances.
    pub(super) images: &'a [ImageInstance],
    /// Base glyphs.
    pub(super) glyphs: &'a [GlyphInstance],
    /// Overlay quads.
    pub(super) overlay_quads: &'a [QuadInstance],
    /// Overlay glyphs.
    pub(super) overlay_glyphs: &'a [GlyphInstance],
}

impl GpuRenderer {
    /// Hand an assembled frame to the presenter that owns this window's output:
    /// GDI when software-render degradation is enabled on Windows, otherwise the wgpu
    /// swapchain. The caller acknowledges the plan only for `Presented`.
    pub(super) fn present_frame(
        &mut self,
        layers: &FrameLayers<'_>,
        timing: &mut FrameTiming,
    ) -> anyhow::Result<PresentOutcome> {
        #[cfg(target_os = "windows")]
        if self.software_render_degrade {
            // When: `software_render_degrade` on Windows — frames reach the
            // window through the CPU blitter, not the swapchain.
            return self.present_software_frame(layers, timing);
        }
        self.present_wgpu_frame(layers, timing)
    }

    /// Prepare a retained CPU frame's checkpoint without admitting work yet.
    /// The caller keeps the existing reblit scope and stopped early exit in its render branch.
    pub(super) fn prepare_cached_present(&mut self) -> Option<DeviceGate> {
        #[cfg(target_os = "windows")]
        if self.software_render_degrade && self.software_frame.is_some() {
            // When: `software_render_degrade` retains a frame, consume the fault at its pre-reblit checkpoint.
            if std::mem::take(&mut self.fault_stop_before_cached_present) {
                self.device_errors
                    .record_observed_validation("injected stop before a cached present");
            }
            return Some(self.device_errors.gate());
        }
        None
    }

    /// Present the unchanged frame using the scope already admitted by the caller.
    pub(super) fn present_unchanged_frame(
        &mut self,
        before: DeviceGate,
        reblit_scope: GpuWorkScope,
    ) -> anyhow::Result<PresentOutcome> {
        self.reblit_software_frame(before, reblit_scope)
    }

    /// Reblit under the caller's existing scope; non-Windows callers never prepare a cached frame.
    fn reblit_software_frame(
        &mut self,
        before: DeviceGate,
        reblit_scope: GpuWorkScope,
    ) -> anyhow::Result<PresentOutcome> {
        #[cfg(target_os = "windows")]
        if let Some(frame) = self.software_frame.as_ref() {
            self.present_calls = self.present_calls.saturating_add(1);
            crate::software_windows::present_frame(frame, &self.window)?;
        }
        // A reblit submits nothing, so its entry reading is also its submission reading.
        let outcome = decide_frame_outcome(before, before, self.device_errors.gate());
        drop(reblit_scope);
        if !outcome.acknowledges() {
            // When: `acknowledges` is false, retain dirt and clear the key without requesting another redraw.
            return Ok(self.rendering_unavailable());
        }
        Ok(PresentOutcome::CachedReblit)
    }

    /// Compose the frame on the CPU and present it through GDI.
    #[cfg(target_os = "windows")]
    fn present_software_frame(
        &mut self,
        layers: &FrameLayers<'_>,
        timing: &mut FrameTiming,
    ) -> anyhow::Result<PresentOutcome> {
        let before = self.device_errors.gate();
        let Some(frame_scope) = self.device_errors.enter_gpu_work("render.software") else {
            // When: `enter_gpu_work` refuses, assembly stopped the device; nothing is composed.
            return Ok(self.rendering_unavailable());
        };
        if let Some(probe) = self.fault_frame_probe.as_ref() {
            // Software presentation issues no GPU work, so the armed fault submits its own.
            crate::device_errors::submit_invalid_frame_command(&self.device, &self.queue, probe);
        }
        let bg_clear = [self.bg.r as f32, self.bg.g as f32, self.bg.b as f32, self.bg.a as f32];
        if self.software_frame.is_none() {
            // First degraded frame, or the buffer was released when the
            // path last turned off.
            self.software_frame = Some(crate::software_frame::SoftwareFrame::new(
                self.config.width,
                self.config.height,
                bg_clear,
            )?);
        }
        let frame = self.software_frame.as_mut().expect("software frame initialized");
        frame.prepare(self.config.width, self.config.height, bg_clear)?;
        frame.draw_layers_with_subpixel_aa(
            &self.glyph_atlas,
            &self.image_atlas,
            layers.subpixel_aa,
            layers.quads,
            layers.images,
            layers.glyphs,
            layers.overlay_quads,
            layers.overlay_glyphs,
        );
        self.glyph_atlas.clear_dirty_rects();
        self.image_atlas.clear_dirty_rects();
        let after_submit = self.device_errors.gate();
        if !decide_frame_outcome(before, after_submit, after_submit).presents() {
            // When: `presents` is false, the fault stopped the device; the frame stays hidden.
            return Ok(self.rendering_unavailable());
        }
        frame_scope.set_operation("render.present");
        self.present_calls = self.present_calls.saturating_add(1);
        crate::software_windows::present_frame(frame, &self.window)?;
        lap(timing, "software_present");
        let outcome = decide_frame_outcome(before, after_submit, self.device_errors.gate());
        drop(frame_scope);
        if !outcome.acknowledges() {
            // When: `acknowledges` is false, the plan stays dirty for a later frame.
            return Ok(self.rendering_unavailable());
        }
        Ok(PresentOutcome::Presented)
    }

    /// Upload dirty atlas tiles, acquire a swapchain texture, draw the retained
    /// frame into it, then submit and present.
    fn present_wgpu_frame(
        &mut self,
        layers: &FrameLayers<'_>,
        timing: &mut FrameTiming,
    ) -> anyhow::Result<PresentOutcome> {
        let before = self.device_errors.gate();
        let Some(frame_scope) = self.device_errors.enter_gpu_work("render.upload") else {
            // When: `enter_gpu_work` refuses, assembly stopped the device; nothing is submitted.
            return Ok(self.rendering_unavailable());
        };
        // Push new glyph tiles to the GPU texture before any draw call samples
        // it: after frame assembly populated the dirty rects, and before the
        // WezTerm presentation draw call in the render pass below.
        let image_upload_stats = self.image_upload.sync(&self.queue, &mut self.image_atlas);
        let glyph_upload_stats = self.glyph_upload.sync(&self.queue, &mut self.glyph_atlas);
        let retained_inline_media_bytes = self.retained_inline_media_bytes;
        self.log_atlas_upload_stats("image", image_upload_stats, retained_inline_media_bytes);
        self.log_atlas_upload_stats("glyph", glyph_upload_stats, retained_inline_media_bytes);
        lap(timing, "glyph_upload");

        frame_scope.set_operation("render.acquire");
        let acquired = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Ok(frame),
            // Acquisition timed out without a texture to draw into.
            wgpu::CurrentSurfaceTexture::Timeout => Err(SurfaceRetryReason::Timeout),
            // The window is minimized or covered, so no texture was handed back.
            wgpu::CurrentSurfaceTexture::Occluded => Err(SurfaceRetryReason::Occluded),
            // The swapchain no longer matches the window (a resize landed
            // between configure and acquire).
            wgpu::CurrentSurfaceTexture::Outdated => Err(SurfaceRetryReason::Outdated),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // The swapchain still works but no longer matches the surface.
                // wgpu 29: Surface::configure panics if a SurfaceTexture is
                // still alive. Drop the frame BEFORE reconfiguring.
                drop(frame);
                Err(SurfaceRetryReason::Suboptimal)
            }
            // The surface itself is gone (display change, driver reset).
            wgpu::CurrentSurfaceTexture::Lost => Err(SurfaceRetryReason::SurfaceLost),
            wgpu::CurrentSurfaceTexture::Validation => {
                // When: `Validation` — wgpu routed the acquisition error to this
                // device's handler, so the device stops instead of retrying.
                self.device_errors.record_observed_validation("surface acquisition validation");
                return Ok(self.rendering_unavailable());
            }
        };
        let frame = match acquired {
            Ok(frame) => frame,
            Err(reason) => {
                // When: `reason` names a surface that handed back no texture; it is
                // recovered inside this gated scope, then the frame retries or stops.

                // Invariant: any render() that returns without a successful present
                // must force the next render() onto the full-redraw path. Otherwise an
                // unchanged FrameKey hits the fast path at the top of render() and
                // skips the present again, leaving a freshly (re)configured swapchain
                // texture blank until the next output changes the key.
                self.last_frame_key = None;
                match reason.recovery() {
                    SurfaceRecovery::Keep => {
                        // When: `Keep` — a timed-out or occluded surface stays configured as it is.
                    }
                    SurfaceRecovery::Reconfigure => {
                        self.surface.configure(&self.device, &self.config);
                    }
                    SurfaceRecovery::Recreate => {
                        self.surface = self.instance.create_surface(self.window.clone())?;
                        self.surface.configure(&self.device, &self.config);
                    }
                }
                return Ok(self.finish_surface_retry(reason));
            }
        };
        lap(timing, "surface_acquire");
        frame_scope.set_operation("render.encode");
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sonic") });
        draw_retained_frame(
            &mut self.present_pipeline,
            &self.device,
            &self.queue,
            &mut encoder,
            &self.frame_view,
            self.image_upload.image_bind_group(),
            self.glyph_upload.glyph_bind_group(),
            layers.surface_width,
            layers.surface_height,
            layers.first_frame,
            layers.damage,
            self.bg,
            layers.subpixel_aa,
            layers.quads,
            layers.images,
            layers.glyphs,
            layers.overlay_quads,
            layers.overlay_glyphs,
        );
        self.frame_blitter.copy(&self.device, &mut encoder, &self.frame_view, &view);
        if let Some(probe) = self.fault_frame_probe.as_ref() {
            // The armed frame fault adds an invalid command, so this submission fails validation.
            record_invalid_frame_command(&mut encoder, probe);
        }
        lap(timing, "render_pass");
        frame_scope.set_operation("render.submit");
        self.queue.submit(Some(encoder.finish()));
        lap(timing, "queue_submit");
        let after_submit = self.device_errors.gate();
        if !decide_frame_outcome(before, after_submit, after_submit).presents() {
            // When: `presents` is false, submission stopped the device; the texture is dropped.
            drop(frame);
            return Ok(self.rendering_unavailable());
        }
        frame_scope.set_operation("render.present");
        self.present_calls = self.present_calls.saturating_add(1);
        self.queue.present(frame);
        lap(timing, "present");
        let outcome = decide_frame_outcome(before, after_submit, self.device_errors.gate());
        drop(frame_scope);
        if !outcome.acknowledges() {
            // When: `acknowledges` is false, the plan stays dirty for a later frame.
            return Ok(self.rendering_unavailable());
        }
        Ok(PresentOutcome::Presented)
    }

    /// Classify a frame whose surface handed back no texture and was recovered.
    ///
    /// Issues no GPU work: it reads the device gate and requests the next redraw.
    fn finish_surface_retry(&mut self, reason: SurfaceRetryReason) -> PresentOutcome {
        match surface_retry_disposition(reason, self.device_errors.gate()) {
            SurfaceRetryDisposition::Retry => {
                self.window.request_redraw();
                PresentOutcome::SurfaceRetry(reason)
            }
            SurfaceRetryDisposition::DeferStop => {
                self.window.request_redraw();
                PresentOutcome::RenderingUnavailable(self.suspended_context(false))
            }
            SurfaceRetryDisposition::Stop => self.rendering_unavailable(),
        }
    }

    /// Stop a frame on a device that no longer accepts work.
    ///
    /// Clears the retained frame key, so no later frame takes the unchanged
    /// path, and marks only the renderer's first stopped frame as the one that
    /// reports the stop.
    pub(super) fn rendering_unavailable(&mut self) -> PresentOutcome {
        self.last_frame_key = None;
        let reports_stop = !std::mem::replace(&mut self.device_stop_reported, true);
        PresentOutcome::RenderingUnavailable(self.suspended_context(reports_stop))
    }

    /// Read the stopped device's generation and gate for a frame outcome.
    fn suspended_context(&self, reports_stop: bool) -> SuspendedContext {
        SuspendedContext {
            generation: self.device_errors.generation(),
            gate: self.device_errors.gate(),
            reports_stop,
        }
    }
}

#[cfg(test)]
#[path = "present_tests.rs"]
mod present_tests;
