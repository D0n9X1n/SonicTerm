//! Inline-image emission reuses the frame scratch's sort buffers and draws what fresh buffers draw.

use super::*;

/// `count` 2x2 images with ids `0..count`, every third one repeating its predecessor's id so
/// equal ids share a tile, as a kitty placement shares its upload.
fn images(count: u64) -> Vec<sonicterm_render_model::InlineImage> {
    (0..count)
        .map(|index| sonicterm_render_model::InlineImage {
            scroll_mark: 0,
            id: if index % 3 == 2 { index - 1 } else { index },
            row: (index % 4) as u16,
            col: (index % 7) as u16,
            width: 2,
            height: 2,
            bgra: std::sync::Arc::from(vec![(index % 251) as u8; 16]),
            kitty: None,
        })
        .collect()
}

/// One placement per image, with painter orders reversed against store order so both sorts
/// reorder.
fn placements(images: &[sonicterm_render_model::InlineImage]) -> Vec<InlineImagePlacement<'_>> {
    images
        .iter()
        .enumerate()
        .map(|(index, image)| InlineImagePlacement {
            image,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: PaneRect::new(0.0, 0.0, 16.0, 8.0),
            painter_order: images.len() - 1 - index,
            slice: None,
        })
        .collect()
}

/// The emission with fresh buffers and an allocation order sorted by id alone, the oracle the
/// scratch-reusing emission must match draw for draw.
fn reference_emit(
    image_atlas: &mut GlyphAtlas,
    out: &mut Vec<ImageInstance>,
    placements: &[InlineImagePlacement<'_>],
) -> usize {
    let (cell_w, cell_h, sw, sh) = (1.0, 1.0, 16.0, 8.0);
    let mut skipped = 0usize;
    let mut allocation_order: Vec<&InlineImagePlacement<'_>> = placements.iter().collect();
    allocation_order.sort_unstable_by_key(|placement| std::cmp::Reverse(placement.image.id));
    let mut emitted = Vec::with_capacity(placements.len());
    for placement in allocation_order {
        let image = placement.image;
        let Some(visible) = placement.visible_rect(cell_w, cell_h, sw, sh) else {
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
        let Some(info) =
            image_atlas.get_or_insert_lazy_without_eviction(key, image.width, image.height, || {
                sonicterm_text::glyph_atlas::RasterTile {
                    width: image.width,
                    height: image.height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: image.width as f32,
                    coverage: image.bgra.as_ref().to_vec(),
                    is_color: true,
                    is_subpixel: false,
                }
            })
        else {
            skipped += 1;
            continue;
        };
        let [u0, v0, u1, v1] = info.uv;
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
    out.extend(emitted.into_iter().map(|(_, instance)| instance));
    skipped
}

/// One frame through the scratch: clear the draw list, emit into it and the sort buffers.
fn scratch_frame(
    atlas: &mut GlyphAtlas,
    scratch: &mut frame_scratch::FrameScratch,
    placements: &[InlineImagePlacement<'_>],
) -> usize {
    let frame_scratch::FrameScratch { images: out, image_sort, .. } = scratch;
    emit_inline_image_instances_with(atlas, out, image_sort, placements, (1.0, 1.0), (16.0, 8.0))
}

#[test]
fn steady_frames_reuse_the_sort_buffers_and_draw_as_before() {
    // Contract: after a completed or an incomplete pass, the next frame sorts in the same two
    // allocations, each buffer keeps the frame's use as its length (the scratch's release rule
    // reads it, and a drained buffer is a freed one whose address the allocator may hand back),
    // and every frame's draws and skip count equal the oracle's. The 8x8 atlas
    // holds 16 tiles of 2x2, fewer than the 40 distinct ids, so newest-first residency and the
    // skip count are exercised too.
    let images = images(60);
    let placements = placements(&images);
    let mut reference_atlas = GlyphAtlas::new(8, 8);
    let mut expected = Vec::new();
    let expected_skipped = reference_emit(&mut reference_atlas, &mut expected, &placements);
    assert!(expected_skipped > 0, "the atlas must refuse some images");

    let mut atlas = GlyphAtlas::new(8, 8);
    let mut scratch = frame_scratch::FrameScratch::default();
    // A capacity no fresh buffer for 60 placements would get (a filled range allocates exactly
    // 60, pushes grow by doubling), and small enough that no release rule touches it, so an
    // unchanged capacity shows the buffer was reused even where the allocator reuses addresses.
    const WARM: usize = 1_000;
    scratch.image_sort.allocation_order.reserve_exact(WARM);
    scratch.image_sort.emitted.reserve_exact(WARM);
    let warm =
        (scratch.image_sort.allocation_order.capacity(), scratch.image_sort.emitted.capacity());
    assert_eq!(scratch_frame(&mut atlas, &mut scratch, &placements), expected_skipped);
    assert_eq!(scratch.images, expected, "first frame draws as before");
    let order_buffer = (scratch.image_sort.allocation_order.as_ptr(), warm.0);
    let emitted_buffer = (scratch.image_sort.emitted.as_ptr(), warm.1);

    for pass in [
        frame_scratch::PassEnd::Completed,
        frame_scratch::PassEnd::Incomplete,
        frame_scratch::PassEnd::Completed,
    ] {
        scratch.finish(pass);
        assert_eq!(scratch_frame(&mut atlas, &mut scratch, &placements), expected_skipped);
        assert_eq!(scratch.images, expected, "a steady frame after {pass:?} draws as before");
        let sort = &scratch.image_sort;
        assert_eq!(sort.allocation_order.len(), placements.len(), "the order holds its use");
        assert_eq!(sort.emitted.len(), expected.len(), "the emitted draws hold their use");
        assert_eq!(
            (sort.allocation_order.as_ptr(), sort.allocation_order.capacity()),
            order_buffer,
            "the allocation order is sorted in the same buffer after {pass:?}"
        );
        assert_eq!(
            (sort.emitted.as_ptr(), sort.emitted.capacity()),
            emitted_buffer,
            "the emitted draws are sorted in the same buffer after {pass:?}"
        );
    }
}
