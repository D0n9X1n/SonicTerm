//! Renderer inline images contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn inline_image_atlas_starts_placeholder_and_promotes_once() {
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
    assert!(!image_atlas_promotion_required(&placeholder, false));
    assert!(image_atlas_promotion_required(&placeholder, true));

    let promoted = GlyphAtlas::default_size();
    assert!(!image_atlas_promotion_required(&promoted, true));
}

#[test]
fn windows_software_presenter_uses_placeholder_gpu_atlases() {
    let atlas = GlyphAtlas::default_size();
    assert_eq!(
        desired_gpu_atlas_dimensions(true, &atlas),
        (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
    );
    assert_eq!(
        desired_gpu_atlas_dimensions(false, &atlas),
        (sonicterm_text::glyph_atlas::ATLAS_DIM, sonicterm_text::glyph_atlas::ATLAS_DIM)
    );
}

#[test]
fn inline_image_atlas_skips_older_images_without_eviction() {
    let older = sonicterm_render_model::InlineImage {
        id: 1,
        row: 0,
        col: 0,
        width: 1,
        height: 1,
        bgra: std::sync::Arc::from(vec![0, 0, 255, 255]),
    };
    let newer = sonicterm_render_model::InlineImage {
        id: 2,
        row: 0,
        col: 0,
        width: 1,
        height: 1,
        bgra: std::sync::Arc::from(vec![0, 255, 0, 255]),
    };
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut instances = Vec::new();
    let placements = [
        InlineImagePlacement {
            image: &older,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: PaneRect::new(0.0, 0.0, 10.0, 10.0),
            painter_order: 0,
        },
        InlineImagePlacement {
            image: &newer,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: PaneRect::new(0.0, 0.0, 10.0, 10.0),
            painter_order: 1,
        },
    ];

    let skipped =
        emit_inline_image_instances(&mut atlas, &mut instances, &placements, 1.0, 1.0, 10.0, 10.0);

    let newer_key = sonicterm_types::GlyphKey {
        ch: '\u{fffc}',
        font_slot: 0xFE,
        weight_bold: false,
        italic: false,
        glyph_id: fold_u64_to_u32(2),
        raster_variant: GlyphRasterVariant::Normal,
    };
    let older_key = sonicterm_types::GlyphKey { glyph_id: fold_u64_to_u32(1), ..newer_key };
    assert_eq!(skipped, 1);
    assert_eq!(atlas.evictions(), 0, "image pressure must never recycle atlas rectangles");
    assert!(atlas.get(newer_key).is_some(), "newest image should win bounded capacity");
    assert!(atlas.get(older_key).is_none(), "older image should be skipped once full");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].sample_uv, instances[0].uv, "uncut images retain their complete tile");
}

/// Image pixels beyond a pane's right edge must not overwrite its peer on either presenter.
#[test]
fn inline_image_clips_512_pixels_to_400_pixel_pane() {
    let image = striped_inline_image(512, 2);
    let mut atlas = GlyphAtlas::new(512, 2);
    let mut instances = Vec::new();
    let placement = InlineImagePlacement {
        image: &image,
        origin_x: 0.0,
        origin_y: 0.0,
        content_clip: PaneRect::new(0.0, 0.0, 400.0, 2.0),
        painter_order: 0,
    };
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &[placement], 1.0, 1.0, 520.0, 2.0),
        0
    );
    assert_eq!(instances.len(), 1);
    let sentinel = [0.0, 1.0, 0.0, 1.0];
    let gpu = crate::atlas_upload::render_image_instances_readback(
        &mut atlas, &instances, 520, 2, sentinel,
    );
    #[cfg(target_os = "windows")]
    let cpu = {
        let mut frame = crate::software_frame::SoftwareFrame::new(520, 2, sentinel).unwrap();
        frame.draw_layers(&atlas, &atlas, &[], &instances, &[], &[], &[]);
        (0..2)
            .flat_map(|y| (0..520).map(move |x| (x, y)))
            .flat_map(|(x, y)| frame.pixel_bgra_at(x, y).unwrap())
            .collect::<Vec<u8>>()
    };
    for x in 0..400 {
        let offset = x * 4;
        assert_eq!(&gpu[offset..offset + 4], &image.bgra[offset..offset + 4]);
    }
    for x in 400..520 {
        let offset = x * 4;
        assert_eq!(&gpu[offset..offset + 4], &[0, 255, 0, 255], "GPU peer pixel {x}");
        #[cfg(target_os = "windows")]
        assert_eq!(&cpu[offset..offset + 4], &[0, 255, 0, 255], "CPU peer pixel {x}");
    }
}

/// Fully pane-clipped or undecoded images must not promote residency, pack tiles, or emit geometry.
#[test]
fn inline_image_visibility_matches_empty_emission_and_residency() {
    let image = striped_inline_image(8, 2);
    let mut atlas = GlyphAtlas::new(16, 4);
    let before = atlas.retained_amount();
    let placement = InlineImagePlacement {
        image: &image,
        origin_x: 20.0,
        origin_y: 0.0,
        content_clip: PaneRect::new(0.0, 0.0, 10.0, 4.0),
        painter_order: 0,
    };
    let mut instances = Vec::new();
    let visible = placement.visible_rect(1.0, 1.0, 40.0, 4.0).is_some();
    assert!(!visible, "on-surface pixels outside their pane are not renderable media");
    assert!(!image_atlas_promotion_required(&GlyphAtlas::new(1, 1), visible));
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &[placement], 1.0, 1.0, 40.0, 4.0),
        0
    );
    assert!(instances.is_empty());
    assert!(atlas.is_empty());
    assert_eq!(atlas.retained_amount(), before);
    let empty =
        sonicterm_render_model::InlineImage { bgra: std::sync::Arc::from([]), ..image.clone() };
    let undecoded = InlineImagePlacement { image: &empty, origin_x: 0.0, ..placement };
    assert!(undecoded.visible_rect(1.0, 1.0, 40.0, 4.0).is_none());
}

/// Each pane and surface edge clips destination geometry without changing the source-image scale.
#[test]
fn inline_image_visible_rect_intersects_all_edges() {
    let image = striped_inline_image(10, 8);
    let cases = [
        (PaneRect::new(3.25, 2.0, 8.0, 8.0), PaneRect::new(3.25, 2.0, 6.75, 6.0)),
        (PaneRect::new(0.0, 0.0, 7.5, 6.25), PaneRect::new(0.0, 0.0, 7.5, 6.25)),
        (PaneRect::new(-3.0, -2.0, 20.0, 20.0), PaneRect::new(0.0, 0.0, 10.0, 8.0)),
        (PaneRect::new(1.25, 0.75, 6.5, 4.5), PaneRect::new(1.25, 0.75, 6.5, 4.5)),
    ];
    for (content_clip, expected) in cases {
        let placement = InlineImagePlacement {
            image: &image,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip,
            painter_order: 0,
        };
        assert_eq!(placement.visible_rect(1.0, 1.0, 20.0, 20.0), Some(expected));
    }
    let surface = InlineImagePlacement {
        image: &image,
        origin_x: -2.25,
        origin_y: -1.5,
        content_clip: PaneRect::new(-4.0, -3.0, 30.0, 30.0),
        painter_order: 0,
    };
    assert_eq!(surface.visible_rect(1.0, 1.0, 6.0, 5.0), Some(PaneRect::new(0.0, 0.0, 6.0, 5.0)));
}

/// Fractional pane cuts keep the unclipped image's interpolation and cannot redefine packed-tile edges.
#[test]
fn inline_image_fractional_clips_preserve_original_tile_sampling() {
    let image = striped_inline_image(8, 6);
    let surface = PaneRect::new(0.0, 0.0, 16.0, 12.0);
    let cases = [
        ([2.25, 1.75], PaneRect::new(4.375, 0.0, 11.625, 12.0)),
        ([2.25, 1.75], PaneRect::new(0.0, 0.0, 7.625, 12.0)),
        ([2.25, 1.75], PaneRect::new(0.0, 3.375, 16.0, 8.625)),
        ([2.25, 1.75], PaneRect::new(0.0, 0.0, 16.0, 5.625)),
        ([2.25, 1.75], PaneRect::new(4.375, 3.375, 3.25, 2.25)),
        ([-2.25, -1.75], PaneRect::new(0.375, 0.375, 4.25, 3.25)),
    ];
    let sentinel = [0.0, 1.0, 0.0, 1.0];
    for (origin, clip) in cases {
        let (mut full_atlas, full_instances) = packed_image_fixture(&image, origin, surface);
        let baseline = crate::atlas_upload::render_image_instances_readback(
            &mut full_atlas,
            &full_instances,
            16,
            12,
            sentinel,
        );
        let (mut atlas, instances) = packed_image_fixture(&image, origin, clip);
        assert_eq!(instances.len(), 1);
        let instance = instances[0];
        assert_ne!(instance.uv, instance.sample_uv);
        assert!(instance.sample_uv[0] > 0.0 && instance.sample_uv[1] > 0.0);
        assert!(instance.sample_uv[2] < 1.0 && instance.sample_uv[3] < 1.0);
        assert_eq!(instance.sample_uv, full_instances[0].sample_uv);
        let gpu = crate::atlas_upload::render_image_instances_readback(
            &mut atlas, &instances, 16, 12, sentinel,
        );
        #[cfg(target_os = "windows")]
        let cpu = {
            let mut frame = crate::software_frame::SoftwareFrame::new(16, 12, sentinel).unwrap();
            frame.draw_layers(&atlas, &atlas, &[], &instances, &[], &[], &[]);
            (0..12)
                .flat_map(|y| (0..16).map(move |x| (x, y)))
                .flat_map(|(x, y)| frame.pixel_bgra_at(x, y).unwrap())
                .collect::<Vec<u8>>()
        };
        let [left, top, width, height] = instance.rect_px;
        let mut visible_samples = 0;
        for y in 0..12 {
            for x in 0..16 {
                let offset = (y * 16 + x) * 4;
                let point = [x as f32 + 0.5, y as f32 + 0.5];
                let visible = point[0] >= left
                    && point[0] < left + width
                    && point[1] >= top
                    && point[1] < top + height;
                let expected: [u8; 4] = if visible {
                    visible_samples += 1;
                    baseline[offset..offset + 4].try_into().unwrap()
                } else {
                    [0, 255, 0, 255]
                };
                let actual = gpu[offset..offset + 4].try_into().unwrap();
                assert!(
                    pixels_close(actual, expected),
                    "clip={clip:?} pixel=({x},{y}) GPU={actual:?} expected={expected:?}"
                );
                #[cfg(target_os = "windows")]
                {
                    let actual = cpu[offset..offset + 4].try_into().unwrap();
                    assert!(
                        pixels_close(actual, expected),
                        "clip={clip:?} pixel=({x},{y}) CPU={actual:?} expected={expected:?}"
                    );
                }
            }
        }
        assert!(visible_samples > 0);
    }
}

/// Newest-first atlas packing must not change overlapping images' original painter order.
#[test]
fn inline_image_clipping_preserves_painter_order() {
    let older = striped_inline_image(4, 2);
    let newer = sonicterm_render_model::InlineImage { id: older.id + 1, ..older.clone() };
    let clip = PaneRect::new(1.25, 0.0, 2.5, 2.0);
    let placements = [
        InlineImagePlacement {
            image: &older,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: clip,
            painter_order: 0,
        },
        InlineImagePlacement {
            image: &newer,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: clip,
            painter_order: 1,
        },
    ];
    let mut atlas = GlyphAtlas::new(8, 2);
    let mut instances = Vec::new();
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &placements, 1.0, 1.0, 8.0, 2.0),
        0
    );
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].rect_px, instances[1].rect_px);
    assert!(
        instances[0].sample_uv[0] > instances[1].sample_uv[0],
        "newer tile packs first but paints last"
    );
}
