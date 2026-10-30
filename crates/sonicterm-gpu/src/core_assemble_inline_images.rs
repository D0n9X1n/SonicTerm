//! Assemble inline images in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_inline_images(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        retained_inline_media_bytes: usize,
        inline_media_changed: bool,
    ) {
        let AssemblyGeometry {
            pane_views,
            cell_width_px: cell_w,
            cell_height_px: cell_h,
            surface_width_px: sw,
            surface_height_px: sh,
            ..
        } = geometry;
        let frame_scratch::FrameScratch { images: image_glyph_instances, .. } = scratch;
        // Anchored images first, then each pane's placeholder runs, in pane order. Placeholder runs
        // feed the same atlas promotion, newest-first residency and painter order as anchored images.
        let mut inline_image_placements: Vec<InlineImagePlacement<'_>> = Vec::new();
        for pv in pane_views {
            for image in pv.inline_images.iter().filter(|image| image.draws_at_anchor()) {
                inline_image_placements.push(InlineImagePlacement {
                    image,
                    origin_x: pv.origin_x,
                    origin_y: pv.origin_y,
                    content_clip: pv.planned.content_clip,
                    painter_order: inline_image_placements.len(),
                    slice: None,
                });
            }
            if !kitty_placeholders::has_virtual_placement(pv.inline_images) {
                // When: has_virtual_placement is false, no row can hold a drawable placeholder, so none is scanned.
                continue;
            }
            for (image, slice) in kitty_placeholders::placeholder_slices(
                pv.grid,
                pv.planned,
                pv.inline_images,
                (pv.origin_x, pv.origin_y),
                (cell_w, cell_h),
            ) {
                inline_image_placements.push(InlineImagePlacement {
                    image,
                    origin_x: pv.origin_x,
                    origin_y: pv.origin_y,
                    content_clip: pv.planned.content_clip,
                    painter_order: inline_image_placements.len(),
                    slice: Some(slice),
                });
            }
        }
        let has_renderable_inline_media = inline_image_placements
            .iter()
            .any(|placement| placement.visible_rect(cell_w, cell_h, sw, sh).is_some());
        self.inline_media_absent_since = next_inline_media_absent_since(
            self.inline_media_absent_since,
            has_renderable_inline_media,
            Instant::now(),
        );
        self.demote_image_atlas_if_idle(has_renderable_inline_media);
        let image_atlas_promoted = self.promote_image_atlas_if_needed(
            has_renderable_inline_media,
            retained_inline_media_bytes,
        );
        if inline_media_changed
            && !image_atlas_promoted
            && image_atlas_reset_warranted(&self.image_atlas)
        {
            self.reset_image_atlas();
        }
        let skipped_inline_images = emit_inline_image_instances(
            &mut self.image_atlas,
            &mut *image_glyph_instances,
            &inline_image_placements,
            cell_w,
            cell_h,
            sw,
            sh,
        );
        report_inline_image_pressure(
            inline_media_changed,
            skipped_inline_images,
            &self.image_atlas,
        );
    }
}
