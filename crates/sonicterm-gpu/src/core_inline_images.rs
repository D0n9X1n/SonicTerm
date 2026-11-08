//! Inline images responsibilities of the renderer, preserving its single owner.

use super::*;

#[allow(clippy::too_many_arguments)]
#[derive(Clone, Copy)]
pub(super) struct InlineImagePlacement<'a> {
    pub(super) image: &'a sonicterm_render_model::InlineImage,
    pub(super) origin_x: f32,
    pub(super) origin_y: f32,
    pub(super) content_clip: PaneRect,
    pub(super) painter_order: usize,
    /// The drawn rect and image part: a placeholder run's slice, or an anchored image's box moved
    /// with the scrolled text; `None` draws the whole image at its anchor and pixel size.
    pub(super) slice: Option<super::kitty_placeholders::PlaceholderSlice>,
}

pub(super) fn report_inline_image_pressure(changed: bool, skipped: usize, atlas: &GlyphAtlas) {
    if changed && skipped > 0 {
        // Explain omitted images only after a media change, without repeating an idle warning.
        tracing::warn!(
            target: "sonic::glyph_atlas",
            skipped,
            resident = atlas.len(),
            width = atlas.width(),
            height = atlas.height(),
            "inline image atlas full; skipped older images without evicting text glyphs"
        );
    }
}

impl InlineImagePlacement<'_> {
    pub(super) fn visible_rect(
        &self,
        cell_w: f32,
        cell_h: f32,
        sw: f32,
        sh: f32,
    ) -> Option<PaneRect> {
        let image = self.image;
        if image.width == 0 || image.height == 0 || image.bgra.is_empty() {
            // When: `image` has no decoded pixels, neither residency nor emission can use its rectangle.
            return None;
        }
        let drawn = self.drawn_rect(cell_w, cell_h);
        let left = drawn.x.max(self.content_clip.x).max(0.0);
        let top = drawn.y.max(self.content_clip.y).max(0.0);
        let right = (drawn.x + drawn.w).min(self.content_clip.x + self.content_clip.w).min(sw);
        let bottom = (drawn.y + drawn.h).min(self.content_clip.y + self.content_clip.h).min(sh);
        if right <= left || bottom <= top {
            // When: `right <= left` or `bottom <= top`, the clipped image is empty and needs no atlas allocation.
            return None;
        }
        Some(PaneRect::new(left, top, right - left, bottom - top))
    }

    /// The unclipped rect this placement draws: the slice's part, or the whole image at its anchor.
    fn drawn_rect(&self, cell_w: f32, cell_h: f32) -> PaneRect {
        match self.slice {
            Some(slice) => slice.dest,
            None => PaneRect::new(
                self.origin_x + self.image.col as f32 * cell_w,
                self.origin_y + self.image.row as f32 * cell_h,
                self.image.width as f32,
                self.image.height as f32,
            ),
        }
    }

    /// The normalised image rect `drawn_rect` shows.
    fn drawn_uv(&self) -> [f32; 4] {
        self.slice.map_or([0.0, 0.0, 1.0, 1.0], |slice| slice.source_uv)
    }
}

/// Emit `placements` into `out` with fresh sort buffers; the renderer passes its frame scratch's
/// buffers to [`emit_inline_image_instances_with`] instead.
#[cfg(test)]
pub(super) fn emit_inline_image_instances(
    image_atlas: &mut GlyphAtlas,
    out: &mut Vec<ImageInstance>,
    placements: &[InlineImagePlacement<'_>],
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
) -> usize {
    let mut sort = frame_scratch::ImageSortScratch::default();
    let surface = (sw, sh);
    emit_inline_image_instances_with(
        image_atlas,
        out,
        &mut sort,
        placements,
        (cell_w, cell_h),
        surface,
    )
}

/// Make each visible placement's image resident, newest first, and append its draw to `out` in
/// painter order. Returns how many placements the bounded atlas could not hold. `sort`'s buffers
/// are cleared and refilled, so a steady frame reuses their capacity.
pub(super) fn emit_inline_image_instances_with(
    image_atlas: &mut GlyphAtlas,
    out: &mut Vec<ImageInstance>,
    sort: &mut frame_scratch::ImageSortScratch,
    placements: &[InlineImagePlacement<'_>],
    cell_size: (f32, f32),
    surface: (f32, f32),
) -> usize {
    let (cell_w, cell_h) = cell_size;
    let (sw, sh) = surface;
    let mut skipped = 0usize;
    let frame_scratch::ImageSortScratch { allocation_order, emitted } = sort;
    allocation_order.clear();
    allocation_order.extend(0..placements.len());
    // Equal ids share one atlas tile, so the index tiebreak only makes the order deterministic.
    allocation_order
        .sort_unstable_by_key(|index| (std::cmp::Reverse(placements[*index].image.id), *index));
    emitted.clear();
    // Prefer the globally newest retained images when the bounded atlas
    // cannot hold the entire history, regardless of which pane owns them.
    for placement in allocation_order.iter().map(|index| &placements[*index]) {
        let image = placement.image;
        let Some(visible) = placement.visible_rect(cell_w, cell_h, sw, sh) else {
            // When: `visible_rect` is absent, this image cannot contribute pixels or need an atlas tile.
            continue;
        };
        let drawn = placement.drawn_rect(cell_w, cell_h);
        let [image_u0, image_v0, image_u1, image_v1] = placement.drawn_uv();
        let key = sonicterm_types::glyph_key::GlyphKey {
            ch: '\u{fffc}',
            font_slot: 0xFE,
            weight_bold: false,
            italic: false,
            glyph_id: fold_u64_to_u32(image.id),
            raster_variant: GlyphRasterVariant::Normal,
        };
        // The image's shared pixels are copied once, straight into the atlas.
        let Some(info) = image_atlas.get_or_insert_color_pixels_without_eviction(
            key,
            image.width,
            image.height,
            &image.bgra,
        ) else {
            // When: `get_or_insert_color_pixels_without_eviction` is None — the image
            // does not fit the bounded atlas; older ones are dropped first.
            skipped += 1;
            continue;
        };
        let [u0, v0, u1, v1] = info.uv;
        // Atlas uv per drawn pixel: the drawn rect spans image_u0..image_u1 of the image's tile.
        let du = (u1 - u0) * (image_u1 - image_u0) / drawn.w;
        let dv = (v1 - v0) * (image_v1 - image_v0) / drawn.h;
        let tile_u = u0 + (u1 - u0) * image_u0;
        let tile_v = v0 + (v1 - v0) * image_v0;
        emitted.push((
            placement.painter_order,
            ImageInstance {
                rect_px: [visible.x, visible.y, visible.w, visible.h],
                uv: [
                    tile_u + (visible.x - drawn.x) * du,
                    tile_v + (visible.y - drawn.y) * dv,
                    tile_u + (visible.x + visible.w - drawn.x) * du,
                    tile_v + (visible.y + visible.h - drawn.y) * dv,
                ],
                sample_uv: info.uv,
            },
        ));
    }
    emitted.sort_unstable_by_key(|(painter_order, _)| *painter_order);
    // Copied, not drained: the scratch's finish reads each buffer's length as the frame's use.
    out.extend(emitted.iter().map(|(_, instance)| *instance));
    skipped
}

#[cfg(test)]
#[path = "core_inline_images_tests.rs"]
mod core_inline_images_tests;

#[cfg(test)]
#[path = "core_inline_images_sort_tests.rs"]
mod core_inline_images_sort_tests;
