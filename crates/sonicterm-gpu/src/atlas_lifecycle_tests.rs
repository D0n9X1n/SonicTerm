use super::*;

#[test]
fn extracted_image_policy_preserves_promotion_reset_and_idle_threshold() {
    // Image residency retains its placeholder and sustained-absence policies after lifecycle extraction.
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
    let promoted = GlyphAtlas::new(8, 8);
    assert!(!image_atlas_reset_warranted(&placeholder));
    assert!(image_atlas_reset_warranted(&promoted));
    assert!(image_atlas_promotion_required(&placeholder, true));
    assert!(!image_atlas_promotion_required(&placeholder, false));
    assert!(!image_atlas_promotion_required(&promoted, true));
    assert!(!image_atlas_demotion_ready(&promoted, false, IMAGE_ATLAS_IDLE_FRAMES - 1));
    assert!(image_atlas_demotion_ready(&promoted, false, IMAGE_ATLAS_IDLE_FRAMES));
    assert!(!image_atlas_demotion_ready(&promoted, true, u32::MAX));
    assert!(!image_atlas_demotion_ready(&placeholder, false, u32::MAX));
    assert_eq!(IMAGE_ATLAS_IDLE_FRAMES, 240);
}

#[test]
fn extracted_upload_policy_preserves_dimensions_and_cpu_payload() {
    // Software presentation keeps full CPU contents while GPU mirrors use placeholders.
    let atlas = GlyphAtlas::new(16, 8);
    assert_eq!(desired_gpu_atlas_dimensions(false, &atlas), (16, 8));
    assert_eq!(desired_gpu_atlas_dimensions(true, &atlas), (1, 1));
    assert!(!atlas_texture_rebuild_required((16, 8), (16, 8)));
    assert!(atlas_texture_rebuild_required((1, 1), (16, 8)));
    assert_eq!(atlas_payload_bytes(16, 8), 512);
}

#[test]
fn extracted_retry_and_success_keep_distinct_settlement_boundaries() {
    // Rejection invalidates UVs before requesting redraw; only acknowledged presentation settles retry.
    let lifecycle = include_str!("atlas_lifecycle.rs");
    let retry = lifecycle.split_once("fn reset_glyph_atlas_after_invalidation(").unwrap().1;
    let retry = retry.split_once("fn glyph_atlas_stamp(").unwrap().0;
    let mut previous = 0;
    for call in [
        "self.reset_glyph_atlas_in_place(reason)",
        "self.glyph_atlas.set_eviction_enabled(false)",
        "self.row_glyph_cache.invalidate_all()",
        "self.glyph_atlas_retry_without_eviction = true",
        "self.last_frame_key = None",
        "self.window.request_redraw()",
    ] {
        let position = retry.find(call).unwrap_or_else(|| panic!("missing {call}"));
        assert!(position > previous, "misordered {call}");
        previous = position;
    }
    assert!(!retry.contains("acknowledge_presented_plan"));
    assert!(retry.contains("let current_epoch = self.glyph_atlas.evictions();"));
    assert!(retry.contains("current_epoch > frame_epoch"));
    assert!(retry.contains("\"eviction_compaction\""));
    assert!(retry.contains("\"content_reset_or_replacement\""));
    assert!(retry.contains("?before,") && retry.contains("?after,"));
    let core = include_str!("core.rs");
    assert_eq!(core.matches("self.finish_glyph_atlas_retry();").count(), 1);
    let success = core.split_once("fn finish_successful_frame(").unwrap().1;
    assert!(
        success.find("acknowledge_presented_plan").unwrap()
            < success.find("self.finish_glyph_atlas_retry();").unwrap()
    );
}

#[test]
fn extracted_upload_rebuilds_still_admit_device_work_before_allocation() {
    // Moving atlas methods cannot move GPU allocation ahead of the device gate.
    let source = include_str!("atlas_lifecycle.rs");
    for (start, end, gate) in [
        (
            "fn rebuild_glyph_upload_if_needed(",
            "fn rebuild_image_upload_if_needed(",
            "glyph_upload.rebuild",
        ),
        (
            "fn rebuild_image_upload_if_needed(",
            "fn log_atlas_upload_stats(",
            "image_upload.rebuild",
        ),
    ] {
        let body = source.split_once(start).unwrap().1.split_once(end).unwrap().0;
        assert_eq!(body.matches("AtlasUpload::new_sized(").count(), 1);
        assert!(body.find(gate).unwrap() < body.find("AtlasUpload::new_sized(").unwrap());
    }
}
