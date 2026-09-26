//! Choose the live GPU context that a New Window renderer shares.

use super::App;
use sonicterm_gpu::core::{GpuRenderer, GpuSharedContext};

impl App {
    /// The GPU context a New Window renderer shares, or `None` when no renderer exists yet.
    ///
    /// The main renderer wins, then the lowest-id window that has a renderer, then the first
    /// warm-pool renderer: the tier order the memory report uses to choose its allocator source.
    /// Headless entries without a renderer are skipped.
    pub(super) fn shared_gpu_context(&self) -> Option<GpuSharedContext> {
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
