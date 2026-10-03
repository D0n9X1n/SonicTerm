//! Choose the live GPU context that a New Window renderer shares.

use super::{App, UserEvent};
use sonicterm_gpu::core::{GpuRenderer, GpuSharedContext};
use winit::event_loop::EventLoopProxy;

impl App {
    /// Return the committed recovery context, or select a live renderer before it is registered.
    pub(super) fn shared_gpu_context(&self) -> Option<GpuSharedContext> {
        if let Some(recovery) = &self.gpu_recovery {
            // When: `gpu_recovery` owns the context, a discarded partial rebind must never supply a new window.
            return Some(recovery.context());
        }
        let windows = self
            .windows
            .iter()
            .filter_map(|(id, state)| state.renderer.as_ref().map(|renderer| (*id, renderer)));
        let warm = self.warm_window_pool.iter().map(|pooled| &pooled.renderer);
        select_shared_renderer(self.main_renderer(), windows, warm).map(GpuRenderer::shared_context)
    }
}

/// Pick the renderer whose device a new window shares: main, then the lowest window key, then
/// the first warm entry.
///
/// Generic so every branch is testable without a GPU. Ordering windows by key keeps `HashMap`
/// iteration order out of the choice.
fn select_shared_renderer<'a, K: Ord + Copy, R: ?Sized>(
    main: Option<&'a R>,
    windows: impl IntoIterator<Item = (K, &'a R)>,
    warm: impl IntoIterator<Item = &'a R>,
) -> Option<&'a R> {
    main.or_else(|| windows.into_iter().min_by_key(|(key, _)| *key).map(|(_, renderer)| renderer))
        .or_else(|| warm.into_iter().next())
}

/// Build the waker a GPU device calls after it stops accepting work.
///
/// It posts [`UserEvent::GpuDeviceStateChanged`]. The device calls it inline on
/// the thread that raised the error, at most once per transition. The callback
/// never blocks and takes no app, window, or renderer lock: it only tries the
/// proxy's private mutex. Only a call of this waker holds that mutex, so every
/// device stop posts at least one wake; a call skips its wake only while
/// another call is posting one, after the device has already stopped.
pub(crate) fn gpu_device_state_waker(
    proxy: EventLoopProxy<UserEvent>,
) -> sonicterm_gpu::device_errors::DeviceStateWaker {
    // Windows' proxy is `Send` but not `Sync`, and the waker must be both.
    let proxy = std::sync::Mutex::new(proxy);
    std::sync::Arc::new(move || {
        let Ok(guard) = proxy.try_lock() else {
            // When: `try_lock` fails, another call is posting a wake for this stopped device.
            return;
        };
        // `EventLoopClosed` means the app is shutting down and needs no wake.
        let _ = guard.send_event(UserEvent::GpuDeviceStateChanged);
    })
}

/// Build the wake a renderer attaches to its fallback notices: it posts `FontFallbackReady` for
/// `window_id` from the fallback worker thread, and never touches renderer state.
pub(crate) fn font_fallback_waker(
    proxy: EventLoopProxy<UserEvent>,
    window_id: winit::window::WindowId,
) -> sonicterm_gpu::core::FontFallbackWaker {
    // Windows' proxy is `Send` but not `Sync`, and the waker must be both.
    let proxy = std::sync::Mutex::new(proxy);
    std::sync::Arc::new(move |notice_id| {
        // A blocking lock, not `try_lock`: a dropped post would leave the notice's claim posted
        // with no event, so no later completion could wake this window again.
        let guard = proxy.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // `EventLoopClosed` means the app is shutting down and needs no wake.
        let _ = guard.send_event(UserEvent::FontFallbackReady { window_id, notice_id });
    })
}

#[cfg(test)]
#[path = "shared_gpu_tests.rs"]
mod shared_gpu_tests;
