//! Atlas allocation, content invalidation, upload mirrors and bounded frame retries.

use super::*;

impl GpuRenderer {
    /// Discard stale UV state and schedule one eviction-disabled retry.
    pub(super) fn reset_glyph_atlas_after_invalidation(
        &mut self,
        before: GlyphContentStamp,
        frame_epoch: u64,
    ) {
        let after = self.glyph_atlas_stamp();
        let current_epoch = self.glyph_atlas.evictions();
        let reason = if before.device_generation == after.device_generation
            && before.allocation_generation == after.allocation_generation
            && current_epoch > frame_epoch
        {
            "eviction_compaction"
        } else {
            "content_reset_or_replacement"
        };
        let resident = self.glyph_atlas.len();
        let hits = self.glyph_atlas.hits();
        let misses = self.glyph_atlas.misses();
        let width = self.glyph_atlas.width();
        let height = self.glyph_atlas.height();
        tracing::warn!(
            target: "sonic::glyph_atlas",
            frame_epoch,
            current_epoch,
            ?before,
            ?after,
            reason,
            resident,
            hits,
            misses,
            width,
            height,
            "glyph atlas contents changed during frame assembly; rebuilding before presentation"
        );

        self.reset_glyph_atlas_in_place(reason);
        self.glyph_atlas.set_eviction_enabled(false);
        self.row_glyph_cache.invalidate_all();
        self.glyph_atlas_retry_without_eviction = true;
        self.last_frame_key = None;
        self.request_window_redraw();
    }

    /// Capture exact device, allocation and content identity for UV validity.
    pub(super) fn glyph_atlas_stamp(&self) -> GlyphContentStamp {
        GlyphContentStamp::capture(
            self.device_errors.generation(),
            self.glyph_atlas_generation,
            &self.glyph_atlas,
        )
    }

    /// Retry a frame whose assembly only grew the glyph atlas.
    ///
    /// Growth moved no tile, so the atlas is kept as it is: no reset, eviction stays enabled. The
    /// UV caches are dropped because their UVs were normalized to the old size, the GPU texture
    /// is recreated at the new size (the grown atlas already queued every resident tile for one
    /// re-upload), and one redraw presents the frame again.
    pub(super) fn retry_after_glyph_atlas_growth(&mut self) {
        self.row_glyph_cache.invalidate_all();
        self.preedit_glyph_cache = None;
        self.rebuild_glyph_upload_if_needed();
        self.last_frame_key = None;
        tracing::debug!(
            target: "sonic::glyph_atlas",
            width = self.glyph_atlas.width(),
            growths = self.glyph_atlas.growths(),
            "glyph atlas grew during frame assembly; retrying without a reset"
        );
        self.request_window_redraw();
    }

    /// Add growths since the last check to the frame counters and start their timing at
    /// `frame_start`, unless an earlier growth's timing is still pending.
    pub(super) fn count_glyph_atlas_growths(&mut self, frame_start: std::time::Instant) {
        self.growth_episodes.count(self.glyph_atlas.growths(), frame_start);
    }

    /// Finalize the growth episodes once the device stops: no frame on this device will present
    /// a pending growth, so it is counted abandoned. A reset in place keeps the device, so it
    /// abandons nothing.
    pub(super) fn finalize_growth_episodes_if_device_stopped(&mut self) {
        if self.device_errors.accepts_gpu_work() {
            // When: accepts_gpu_work is true a later frame can still present the grown atlas.
            return;
        }
        self.finalize_growth_episodes();
    }

    /// Count uncounted growths and abandon a pending growth episode straight into this
    /// renderer's sink; idempotent, and safe outside any collection scope.
    pub(super) fn finalize_growth_episodes(&mut self) {
        self.growth_episodes.finalize(self.glyph_atlas.growths(), self.frame_sink.as_ref());
    }

    fn mark_glyph_atlas_replaced(&mut self) {
        self.glyph_atlas_generation = self.glyph_atlas_generation.wrapping_add(1);
        self.preedit_glyph_cache = None;
    }

    /// Reset CPU glyph contents in place and invalidate preedit identity.
    pub(super) fn reset_glyph_atlas_in_place(&mut self, reason: &'static str) {
        let width = self.glyph_atlas.width();
        let height = self.glyph_atlas.height();
        self.glyph_atlas.reset_in_place();
        self.mark_glyph_atlas_replaced();
        self.glyph_atlas_resets = self.glyph_atlas_resets.saturating_add(1);
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "glyph",
            reason,
            width,
            height,
            cpu_payload_bytes = atlas_payload_bytes(width, height),
            gpu_width = self.glyph_upload.width(),
            gpu_height = self.glyph_upload.height(),
            gpu_payload_bytes = self.glyph_upload.payload_bytes(),
            resident = self.glyph_atlas.len(),
            retained_inline_media_bytes = self.retained_inline_media_bytes,
            retained_pixel_allocation = true,
            payload_estimate = true,
            "renderer atlas reset in place"
        );
    }

    /// Reset image packing without replacing retained pixel allocation.
    pub(super) fn reset_image_atlas(&mut self) {
        let width = self.image_atlas.width();
        let height = self.image_atlas.height();
        self.image_atlas.reset_in_place();
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "image",
            width,
            height,
            cpu_payload_bytes = atlas_payload_bytes(width, height),
            gpu_width = self.image_upload.width(),
            gpu_height = self.image_upload.height(),
            gpu_payload_bytes = self.image_upload.payload_bytes(),
            resident = self.image_atlas.len(),
            retained_inline_media_bytes = self.retained_inline_media_bytes,
            retained_pixel_allocation = true,
            payload_estimate = true,
            "renderer atlas reset in place"
        );
    }

    /// Release promoted image storage after 240 assemblies without visible media (the frame trigger).
    pub(super) fn demote_image_atlas_if_idle(&mut self, has_inline_media: bool) {
        if has_inline_media {
            // When: has_inline_media resets the idle run; demotion requires sustained absence.
            self.frames_without_inline_media = 0;
            return;
        }
        self.frames_without_inline_media = self.frames_without_inline_media.saturating_add(1);
        if !image_atlas_demotion_ready(
            &self.image_atlas,
            has_inline_media,
            self.frames_without_inline_media,
        ) {
            // When: image_atlas_demotion_ready is false, retain the placeholder or still-recent full atlas.
            return;
        }

        self.release_image_atlas("idle_frames");
    }

    /// When the interval trigger releases this window's promoted image atlas: 30 s after renderable
    /// media was last visible. `None` for a placeholder atlas or while media is visible.
    #[must_use]
    pub fn image_atlas_release_deadline(&self) -> Option<Instant> {
        image_atlas_release_deadline_for(
            image_atlas_promoted(&self.image_atlas),
            self.inline_media_absent_since,
        )
    }

    /// Release the promoted image atlas without assembling a frame once no renderable media has been
    /// visible for 30 s; returns whether it released. The rule is checked again here, so a firing after
    /// media returned, or after the atlas was already released, changes nothing.
    ///
    /// Safe without a frame: the atlas is sampled only during assembly, image instances are rebuilt
    /// every frame, and the next frame with media promotes it again before emitting any image.
    pub fn release_idle_image_atlas(&mut self, now: Instant) -> bool {
        let promoted = image_atlas_promoted(&self.image_atlas);
        if !image_atlas_release_due(promoted, self.inline_media_absent_since, now) {
            // When: image_atlas_release_due is false at service time (media returned or already released), the firing is stale.
            return false;
        }
        self.release_image_atlas("idle_interval");
        self.flush_image_upload_rebuild();
        true
    }

    /// Release the promoted image atlas for a covered window's trim when no renderable media is
    /// visible; returns whether it released. Visible media keeps its atlas.
    pub(super) fn release_image_atlas_for_trim(&mut self) -> bool {
        if !image_atlas_promoted(&self.image_atlas) || self.inline_media_absent_since.is_none() {
            // When: image_atlas_promoted is false or inline_media_absent_since is None, nothing idle is held.
            return false;
        }
        self.release_image_atlas("occlusion_trim");
        self.flush_image_upload_rebuild();
        true
    }

    /// The image atlas's CPU size and its GPU mirror's size, so a native test can check that a
    /// release shrinks both (or, on a stopped device, only the CPU atlas).
    #[doc(hidden)]
    #[must_use]
    pub fn __test_image_atlas_dimensions(&self) -> ((u32, u32), (u32, u32)) {
        (
            (self.image_atlas.width(), self.image_atlas.height()),
            (self.image_upload.width(), self.image_upload.height()),
        )
    }

    /// The one release body both triggers share: drop the CPU atlas to the placeholder and shrink the
    /// GPU mirror inside the device gate (a stopped device keeps the old mirror until recovery).
    fn release_image_atlas(&mut self, reason: &'static str) {
        let released_width = self.image_atlas.width();
        let released_height = self.image_atlas.height();
        self.image_atlas = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
        // The GPU mirror shrinks with the CPU atlas once no parser guard is held: after `lend`
        // returns for a frame, or at once for the interval trigger, which runs outside any frame.
        self.image_upload_rebuild_pending = true;
        self.frames_without_inline_media = 0;
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "image",
            released_width,
            released_height,
            released_cpu_bytes = atlas_payload_bytes(released_width, released_height),
            gpu_width = self.image_upload.width(),
            gpu_height = self.image_upload.height(),
            idle_frames = IMAGE_ATLAS_IDLE_FRAMES,
            idle_interval_s = IMAGE_ATLAS_IDLE_INTERVAL.as_secs(),
            reason,
            "image atlas released after sustained absence of inline media"
        );
    }

    /// Allocate a full image atlas only when visible media needs it.
    pub(super) fn promote_image_atlas_if_needed(
        &mut self,
        has_inline_media: bool,
        retained_inline_media_bytes: usize,
    ) -> bool {
        if !image_atlas_promotion_required(&self.image_atlas, has_inline_media) {
            // When: image_atlas_promotion_required is false, existing cached UVs remain valid.
            return false;
        }
        self.image_atlas = GlyphAtlas::default_size();
        // The GPU mirror grows after `lend` returns, before any presenter samples the media.
        self.image_upload_rebuild_pending = true;
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "image",
            width = self.image_atlas.width(),
            height = self.image_atlas.height(),
            cpu_payload_bytes = atlas_payload_bytes(self.image_atlas.width(), self.image_atlas.height()),
            gpu_width = self.image_upload.width(),
            gpu_height = self.image_upload.height(),
            gpu_payload_bytes = self.image_upload.payload_bytes(),
            resident = self.image_atlas.len(),
            retained_inline_media_bytes,
            payload_estimate = true,
            "inline image atlas promoted"
        );
        true
    }

    /// Rebuild the image mirror an assembly or a release asked for, once no parser guard is held.
    /// The software presenter samples the CPU atlas directly and needs no mirror.
    pub(super) fn flush_image_upload_rebuild(&mut self) {
        if std::mem::take(&mut self.image_upload_rebuild_pending)
            && !self.uses_windows_software_presenter()
        {
            self.rebuild_image_upload_if_needed();
        }
    }

    /// Rebuild a mismatched glyph mirror only inside the live device gate.
    pub(super) fn rebuild_glyph_upload_if_needed(&mut self) {
        let current = (self.glyph_upload.width(), self.glyph_upload.height());
        let next =
            desired_gpu_atlas_dimensions(self.uses_windows_software_presenter(), &self.glyph_atlas);
        let fault = std::mem::take(&mut self.fault_invalid_glyph_upload);
        if !fault && !atlas_texture_rebuild_required(current, next) {
            // When: neither fault nor atlas_texture_rebuild_required asks for a new upload.
            return;
        }
        let Some(_scope) = self.device_errors.enter_gpu_work("glyph_upload.rebuild") else {
            // When: enter_gpu_work refuses, the stopped device never samples the old upload.
            return;
        };
        let mut dimensions = next;
        if fault {
            // A zero-sized retained texture forces wgpu to reject the requested fault allocation.
            dimensions = (0, 0);
        }
        self.glyph_upload = AtlasUpload::new_sized(
            &self.device,
            dimensions.0,
            dimensions.1,
            self.present_pipeline.glyph_bind_group_layout(),
            AtlasBindingKind::Glyph,
        );
    }

    /// Rebuild a mismatched image mirror only inside the live device gate.
    pub(super) fn rebuild_image_upload_if_needed(&mut self) {
        let current = (self.image_upload.width(), self.image_upload.height());
        let next =
            desired_gpu_atlas_dimensions(self.uses_windows_software_presenter(), &self.image_atlas);
        if !atlas_texture_rebuild_required(current, next) {
            // When: atlas_texture_rebuild_required is false, the upload already mirrors the atlas.
            return;
        }
        let Some(_scope) = self.device_errors.enter_gpu_work("image_upload.rebuild") else {
            // When: enter_gpu_work refuses, the stopped device never samples the old upload.
            return;
        };
        self.image_upload = AtlasUpload::new_sized(
            &self.device,
            next.0,
            next.1,
            self.present_pipeline.image_bind_group_layout(),
            AtlasBindingKind::Image,
        );
    }

    /// Report actual dirty-region uploads without logging unchanged frames.
    pub(super) fn log_atlas_upload_stats(
        &self,
        atlas: &'static str,
        stats: AtlasUploadStats,
        retained_inline_media_bytes: usize,
    ) {
        if stats.dirty_rects == 0 {
            // When: stats.dirty_rects is zero, no upload occurred and a log would obscure changed frames.
            return;
        }
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas,
            dirty_rects = stats.dirty_rects,
            upload_calls = stats.upload_calls,
            uploaded_bytes = stats.uploaded_bytes,
            retained_inline_media_bytes,
            glyph_resident = self.glyph_atlas.len(),
            image_resident = self.image_atlas.len(),
            "renderer atlas upload synchronized"
        );
    }

    /// Re-enable eviction only after the compaction retry presents successfully.
    pub(super) fn finish_glyph_atlas_retry(&mut self) {
        if std::mem::take(&mut self.glyph_atlas_retry_without_eviction) {
            // Settling re-enables eviction. Cached rows stay valid: only complete rows were admitted,
            // and a later eviction changes the identity they are checked against (the frame-wide
            // stamp check covers a replay earlier in the same assembly). The preedit cache is dropped
            // because chrome layout keeps a run whose glyph the retry refused.
            self.glyph_atlas.set_eviction_enabled(true);
            self.preedit_glyph_cache = None;
            tracing::warn!(
                target: "sonic::glyph_atlas",
                resident = self.glyph_atlas.len(),
                misses = self.glyph_atlas.misses(),
                "glyph atlas compaction retry presented with eviction disabled"
            );
        }
    }
}

#[cfg(test)]
#[path = "atlas_lifecycle_tests.rs"]
mod atlas_lifecycle_tests;
