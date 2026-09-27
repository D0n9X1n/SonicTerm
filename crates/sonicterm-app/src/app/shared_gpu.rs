//! Choose the live GPU context that a New Window renderer shares.

use super::App;
use sonicterm_gpu::core::{GpuRenderer, GpuSharedContext};

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

#[cfg(test)]
#[path = "shared_gpu_tests.rs"]
mod shared_gpu_tests;
