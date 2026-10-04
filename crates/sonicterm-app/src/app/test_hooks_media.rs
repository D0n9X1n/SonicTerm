//! Doc-hidden test hooks for a window's image atlas, its idle release, and the aggregate memory reading.

use super::*;

impl App {
    /// Test seam: a window renderer's image atlas sizes, as `(cpu, gpu_mirror)`.
    ///
    /// `None` when the window is unknown or has no renderer yet.
    #[doc(hidden)]
    pub fn __test_window_image_atlas_dimensions(
        &self,
        id: WindowId,
    ) -> Option<((u32, u32), (u32, u32))> {
        Some(self.windows.get(&id)?.renderer.as_ref()?.__test_image_atlas_dimensions())
    }

    /// Test seam: the bytes the window renderer's own reading reports for its image atlas.
    #[doc(hidden)]
    pub fn __test_window_image_atlas_bytes(&self, id: WindowId) -> Option<usize> {
        Some(self.windows.get(&id)?.renderer.as_ref()?.retained_amounts().image_atlas.bytes)
    }

    /// Test seam: every part the window renderer's own reading reports, so a test can account for
    /// each part a release frees rather than the image atlas alone.
    #[doc(hidden)]
    pub fn __test_window_retained_amounts(
        &self,
        id: WindowId,
    ) -> Option<sonicterm_gpu::core::RendererRetention> {
        Some(self.windows.get(&id)?.renderer.as_ref()?.retained_amounts())
    }

    /// Test seam: when the window's promoted image atlas may be released, through the app's adapter.
    #[doc(hidden)]
    pub fn __test_window_image_atlas_release_deadline(&self, id: WindowId) -> Option<Instant> {
        self.windows.get(&id)?.image_atlas_release_deadline()
    }

    /// Test seam: the frame-family state that decides whether the release is collected, for failure messages.
    #[doc(hidden)]
    pub fn __test_window_release_blockers(&self, id: WindowId) -> String {
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` tracks no entry for `id`, so there is no state to describe.
            return String::from("unknown window");
        };
        format!(
            "collectable={} allowed={} deferred={} in_flight={} pending={} deadline={:?}",
            window.image_atlas_release_collectable(),
            window.frame_deadlines_allowed(),
            window.redraw.deferred,
            window.redraw.request_in_flight,
            window.redraw.has_pending(),
            window.image_atlas_release_deadline(),
        )
    }

    /// Test seam: one wake at `now` through production's order: collect the due work, then service it.
    ///
    /// Returns how many image atlas releases were collected. Any other due work is serviced as well.
    #[doc(hidden)]
    pub fn __test_collect_and_service_redraw_due(&mut self, now: Instant) -> usize {
        self.redraw_due = self.frame_due_work_at(now);
        let collected = self
            .redraw_due
            .iter()
            .filter(|work| work.cause == redraw::DueCause::ImageAtlasRelease)
            .count();
        self.service_redraw_due(now);
        collected
    }

    /// Test seam: replace one pane's decoded inline images, as a finished decode would.
    ///
    /// `false` when the window or pane is unknown.
    #[doc(hidden)]
    pub fn __test_set_pane_inline_images(
        &mut self,
        window: WindowId,
        pane_id: u64,
        images: Vec<sonicterm_render_model::InlineImage>,
    ) -> bool {
        let Some(pane) = self.windows.get(&window).and_then(|state| state.panes.get(&pane_id))
        else {
            // When: neither `window` nor its `panes` resolve `pane_id`, so no images can be stored.
            return false;
        };
        *pane.inline_images.lock() = images;
        true
    }

    /// Test seam: run the production retention pass at `now`, keeping its interval gate.
    ///
    /// Unlike [`Self::__test_sample_pane_retention_now`], the rate limiter is not cleared, so a call
    /// inside the interval does nothing. Returns whether the pass was due and ran.
    #[doc(hidden)]
    pub fn __test_sample_pane_retention_at(&mut self, now: Instant) -> bool {
        let before = self.last_retention_sample;
        self.sample_pane_retention(now);
        self.last_retention_sample != before
    }

    /// Test seam: `renderer_total_bytes` from the last emitted aggregate memory snapshot.
    ///
    /// `None` until a sample has emitted one, which needs the `memory` target enabled at INFO.
    #[doc(hidden)]
    pub fn __test_last_sampled_renderer_bytes(&self) -> Option<usize> {
        self.last_memory_totals.map(|totals| totals.renderer_bytes)
    }
}
