//! Device negotiation shared by startup and GPU recovery, and the owned request a
//! recovery worker runs.
//!
//! Startup and recovery both call `negotiate_device`, so a rebuilt device gets the
//! same adapter preference, optional features, memory policy, and logs as the first
//! one, and its error handlers are installed before any work reaches it. A
//! [`crate::core::ContextRequest`] is built on the event-loop thread with one
//! unconfigured surface for the requesting window on a new instance;
//! [`crate::core::ContextRequest::run`] makes the blocking adapter and device
//! requests on the caller's worker thread, so the event loop never waits on them.
//! Nothing here schedules recovery or spawns a thread.
//!
//! This module is a child of `core` so it can build [`crate::core::GpuSharedContext`]
//! and read the renderer's surface without widening their visibility.

use super::*;

use crate::device_errors::DeviceGate;
use crate::recovery::RetiredGeneration;

/// The adapter, device, and queue one negotiation produced, with the device's
/// containment state.
pub(super) struct NegotiatedDevice {
    pub(super) adapter: wgpu::Adapter,
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    pub(super) device_errors: Arc<DeviceErrorState>,
    pub(super) software_rendering: bool,
}

/// Request an adapter compatible with `surface` and open a device on it under
/// startup's policy, installing the error handlers before any work reaches it.
///
/// This is the only adapter and device request in production code. It blocks on the
/// driver, so recovery reaches it only through [`crate::core::ContextRequest::run`]
/// on a worker thread.
pub(super) async fn negotiate_device(
    instance: &Instance,
    surface: &wgpu::Surface<'_>,
) -> Result<NegotiatedDevice> {
    let adapter = instance
        .request_adapter(&RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })
        .await
        .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;
    let info = adapter.get_info();
    let software_rendering = detect_software_rendering(&info);
    let device_memory_policy = device_memory_policy_from(software_rendering);
    tracing::info!(
        backend = ?info.backend,
        name = %info.name,
        driver = %info.driver,
        device_type = ?info.device_type,
        software_rendering,
        device_memory_policy = ?device_memory_policy,
        "wgpu adapter selected"
    );
    if software_rendering {
        tracing::warn!(
            adapter = %info.name,
            "No hardware GPU — wgpu fell back to a software rasterizer (CPU). \
             Rendering will be degraded to stay responsive (lower frame cap, \
             no fade animation). Common cause: RDP / VM without GPU passthrough. \
             See [appearance].software_render_mode."
        );
    }
    if matches!(info.backend, wgpu::Backend::Gl) {
        tracing::warn!(
            adapter = %info.name,
            "GPU backend is GLES — rendering may differ from native D3D12/Metal. \
             Glyph sharpness, Powerline anchoring, and HiDPI snap may behave \
             unexpectedly. Common cause: running over RDP without GPU passthrough."
        );
    }
    let optional_features = selected_optional_device_features(adapter.features(), cfg!(windows));
    let (device, queue) = adapter
        .request_device(&device_descriptor_for(software_rendering, optional_features))
        .await
        .context("request device")?;
    // Replaces wgpu's default handler, which panics, before any work runs on the device.
    let device_errors = install_device_error_handlers(&device);
    Ok(NegotiatedDevice { adapter, device, queue, device_errors, software_rendering })
}

/// A request for a new GPU context, built on the event-loop thread and run once on a
/// worker thread.
///
/// It owns one unconfigured surface for the requesting window, the new instance that
/// made it, and the window, so the surface stays valid even if the window closes
/// while the request runs. It captures no window size or setting: the renderer reads
/// them again in [`crate::core::GpuRenderer::prepare_rebind`] after the result returns.
#[derive(Debug)]
pub struct ContextRequest {
    surface: CandidateSurface,
}

// The recovery worker receives the request and sends its result back by value, so the
// request, the context, and the failure must all stay `Send`.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<ContextRequest>();
    assert_send::<RecoveredContext>();
    assert_send::<RequestFailure>();
};

impl ContextRequest {
    /// Request the adapter and device for this request's surface, install the waker
    /// `make_waker` builds for the new device's generation, and return the new
    /// context with that surface, still unconfigured.
    ///
    /// Call it on a worker thread: it blocks on the driver. The waker is installed
    /// before any renderer uses the context, so it is the device's first waker and a
    /// later renderer's waker cannot replace it.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::core::RequestFailure`] with startup's adapter or device
    /// error. It keeps the request's surface, instance, and window, so they are
    /// released where the caller drops it rather than on the worker.
    pub fn run(
        self,
        make_waker: impl FnOnce(u64) -> DeviceStateWaker,
    ) -> Result<RecoveredContext, RequestFailure> {
        let (negotiated, surface) = keep_on_failure(self.surface, |surface| {
            pollster::block_on(negotiate_device(&surface.instance, &surface.surface))
        })
        .map_err(|(error, surface)| RequestFailure { error, surface: Box::new(surface) })?;
        let generation = negotiated.device_errors.generation();
        // A new state has no waker yet, so this install always takes effect.
        let _ = negotiated.device_errors.set_waker(make_waker(generation));
        Ok(RecoveredContext {
            context: GpuSharedContext {
                instance: surface.instance.clone(),
                adapter: negotiated.adapter,
                device: negotiated.device,
                queue: negotiated.queue,
                device_errors: negotiated.device_errors,
            },
            surface,
        })
    }

    /// The window the request's surface draws to.
    #[must_use]
    pub fn window(&self) -> &Arc<Window> {
        &self.surface.window
    }
}

/// A new GPU context from [`crate::core::ContextRequest::run`], with the requesting
/// window's surface on it, still unconfigured.
pub struct RecoveredContext {
    context: GpuSharedContext,
    surface: CandidateSurface,
}

impl RecoveredContext {
    /// The new device's generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.context.device_errors.generation()
    }

    /// The new context, which every renderer is rebound to.
    #[must_use]
    pub fn context(&self) -> &GpuSharedContext {
        &self.context
    }

    /// Split into the context and the requesting window's surface, which that
    /// window's [`crate::core::GpuRenderer::prepare_rebind`] takes.
    #[must_use]
    pub fn into_parts(self) -> (GpuSharedContext, CandidateSurface) {
        (self.context, self.surface)
    }
}

/// A request whose adapter or device could not be opened, holding the request's
/// surface, instance, and window so the caller decides where they are released.
///
/// Send it back to the event loop and drop it there. Dropping it anywhere runs the
/// native release of those objects on that thread, and that release cannot be
/// cancelled, so a caller whose event loop has already exited needs its own
/// fallback. It does not implement [`std::error::Error`], so `?` cannot convert it
/// into an [`anyhow::Error`] and drop the surface on the way.
#[must_use = "a failed request still owns the window's surface; release it on the event loop"]
pub struct RequestFailure {
    error: anyhow::Error,
    // Boxed so a failed `run` returns a small error value.
    surface: Box<CandidateSurface>,
}

impl RequestFailure {
    /// The adapter or device request error.
    #[must_use]
    pub fn error(&self) -> &anyhow::Error {
        &self.error
    }

    /// Split into the error and the surface, instance, and window to release.
    #[must_use]
    pub fn into_parts(self) -> (anyhow::Error, CandidateSurface) {
        (self.error, *self.surface)
    }
}

impl std::fmt::Debug for RequestFailure {
    /// Write only the error; formatting reaches no native object.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestFailure").field("error", &self.error).finish_non_exhaustive()
    }
}

/// Lend `owned` to `negotiate` and return it with the outcome, so a failed
/// negotiation hands `owned` back instead of dropping it here.
fn keep_on_failure<T, O>(
    owned: O,
    negotiate: impl FnOnce(&O) -> Result<T>,
) -> Result<(T, O), (anyhow::Error, O)> {
    match negotiate(&owned) {
        Ok(value) => Ok((value, owned)),
        Err(error) => Err((error, owned)),
    }
}

/// An unconfigured surface, the instance that created it, and the window it draws to.
///
/// [`crate::core::GpuRenderer::prepare_rebind`] accepts it only for its own window on
/// the context's own instance. Fields drop in order, surface first: a surface made
/// from the window's Metal layer holds no handle that keeps the window alive.
#[derive(Debug)]
pub struct CandidateSurface {
    pub(super) surface: wgpu::Surface<'static>,
    pub(super) instance: Instance,
    pub(super) window: Arc<Window>,
}

impl GpuSharedContext {
    /// This context's device generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.device_errors.generation()
    }

    /// A reading of this context's device gate.
    #[must_use]
    pub fn gate(&self) -> DeviceGate {
        self.device_errors.gate()
    }

    /// Install the waker for this context's device. The first waker on a device wins,
    /// so the generation-tagged waker goes in before any renderer installs its own.
    /// Returns whether this call installed it.
    pub fn set_state_waker(&self, waker: DeviceStateWaker) -> bool {
        self.device_errors.set_waker(waker)
    }

    /// Close the gate and destroy the device of a context whose generation was
    /// retired, such as a discarded recovery candidate. Nothing is polled.
    ///
    /// # Errors
    ///
    /// Returns an error, destroying nothing, when `retired` names another generation.
    pub fn destroy_retired(&self, retired: RetiredGeneration) -> Result<()> {
        if retired.generation() != self.device_errors.generation() {
            // When: `retired` names another generation than `device_errors`, it is not this device.
            return Err(anyhow!(
                "a retired token for another generation cannot destroy this device"
            ));
        }
        self.device_errors.request_destroy();
        self.device.destroy();
        Ok(())
    }
}

impl GpuRenderer {
    /// Build the owned request for a new GPU context on the event-loop thread: an
    /// unconfigured surface for this renderer's window on a new instance configured
    /// like startup's. It issues no device work.
    ///
    /// # Errors
    ///
    /// Returns an error when the new instance cannot create a surface for the window.
    pub fn recovery_request(&self, event_loop: &ActiveEventLoop) -> Result<ContextRequest> {
        let surface = self.candidate_surface(&new_instance(event_loop))?;
        Ok(ContextRequest { surface })
    }

    /// Create an unconfigured surface for this renderer's window on `instance`.
    ///
    /// On macOS it wraps the window's existing `CAMetalLayer`, so the view gains no
    /// sublayer and no layer property changes, and it refuses a surface that is not
    /// Metal-backed instead of wrapping the view again. It never reuses the
    /// configured surface, whose swapchain belongs to the old device.
    pub(super) fn candidate_surface(&self, instance: &Instance) -> Result<CandidateSurface> {
        let window = Arc::clone(&self.window);
        #[cfg(target_os = "macos")]
        let surface = {
            let metal_surface = {
                // SAFETY: `as_hal` only reads the Metal surface behind `self.surface`; its guard
                // is used only to clone the layer below.
                unsafe { self.surface.as_hal::<wgpu::hal::api::Metal>() }
            };
            let layer = metal_surface.map(|hal| {
                let guard = hal.render_layer().lock();
                let retained = guard.clone();
                // Release the layer mutex before the HAL guard drops; only the clone escapes.
                drop(guard);
                retained
            });
            let Some(layer) = layer else {
                // When: `layer` is None, no Metal layer exists; refuse rather than add a sublayer.
                return Err(anyhow!("recovery requires the window's Metal layer"));
            };
            let raw = std::ptr::from_ref(&*layer).cast_mut().cast::<std::ffi::c_void>();
            let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(raw);
            let surface = {
                // SAFETY: `layer` stays retained through `create_surface_unsafe` and the surface
                // retains it after; `window`, kept beside the surface, owns the hosting view.
                unsafe { instance.create_surface_unsafe(target) }
            }
            .context("create recovery surface from the window's Metal layer")?;
            drop(layer);
            surface
        };
        #[cfg(not(target_os = "macos"))]
        let surface =
            instance.create_surface(Arc::clone(&window)).context("create recovery surface")?;
        Ok(CandidateSurface { surface, instance: instance.clone(), window })
    }
}

#[cfg(test)]
#[path = "recovery_context_tests.rs"]
mod recovery_context_tests;
