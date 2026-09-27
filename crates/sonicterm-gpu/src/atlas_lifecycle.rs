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
        self.window.request_redraw();
    }

    /// Capture exact device, allocation and content identity for UV validity.
    pub(super) fn glyph_atlas_stamp(&self) -> GlyphContentStamp {
        GlyphContentStamp::capture(
            self.device_errors.generation(),
            self.glyph_atlas_generation,
            &self.glyph_atlas,
        )
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

    /// Release promoted image storage after 240 assemblies without visible media.
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

        let released_width = self.image_atlas.width();
        let released_height = self.image_atlas.height();
        self.image_atlas = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
        if !self.uses_windows_software_presenter() {
            // The GPU mirror must shrink with the CPU atlas when software presentation is inactive.
            self.rebuild_image_upload_if_needed();
        }
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
        if !self.uses_windows_software_presenter() {
            // The GPU mirror must grow before sampling media when software presentation is inactive.
            self.rebuild_image_upload_if_needed();
        }
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
            // Settling the eviction-disabled retry clears UV caches before later recycling resumes.
            self.glyph_atlas.set_eviction_enabled(true);
            self.row_glyph_cache.invalidate_all();
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
