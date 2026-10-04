use super::*;
use std::time::{Duration, Instant};

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

/// The settlement scan over one checkout of `atlas_lifecycle.rs` and `core.rs`, read as LF.
fn check_settlement_boundaries(lifecycle: &str, core: &str) {
    let (lifecycle, core) = (lifecycle.replace("\r\n", "\n"), core.replace("\r\n", "\n"));
    let retry = lifecycle.split_once("fn reset_glyph_atlas_after_invalidation(").unwrap().1;
    let retry = retry.split_once("fn glyph_atlas_stamp(").unwrap().0;
    let mut previous = 0;
    for call in [
        "self.reset_glyph_atlas_in_place(reason)",
        "self.glyph_atlas.set_eviction_enabled(false)",
        "self.row_glyph_cache.invalidate_all()",
        "self.glyph_atlas_retry_without_eviction = true",
        "self.last_frame_key = None",
        "self.request_window_redraw()",
    ] {
        let position = retry.find(call).unwrap_or_else(|| panic!("missing {call}"));
        assert!(position > previous, "misordered {call}");
        previous = position;
    }
    assert!(!retry.contains("clear_dirty") && !retry.contains("acknowledge_receipts"));
    assert!(retry.contains("let current_epoch = self.glyph_atlas.evictions();"));
    assert!(retry.contains("current_epoch > frame_epoch"));
    assert!(retry.contains("\"eviction_compaction\""));
    assert!(retry.contains("\"content_reset_or_replacement\""));
    assert!(retry.contains("?before,") && retry.contains("?after,"));
    assert_eq!(core.matches("self.finish_glyph_atlas_retry();").count(), 1);
    // A presented frame settles the retry; it clears no grid dirt, which its receipts carry out.
    let success = core.split_once("fn finish_successful_frame(").unwrap().1;
    let success = success.split_once("\n    }\n").unwrap().0;
    assert!(success.contains("self.finish_glyph_atlas_retry();"));
    assert!(!success.contains("clear_dirty") && !success.contains("panes"));
}

#[test]
fn extracted_retry_and_success_keep_distinct_settlement_boundaries() {
    // Rejection invalidates UVs before requesting redraw; only acknowledged presentation settles retry.
    // Windows CI checks sources out with CRLF line ends, so the scan runs on a CRLF copy too.
    let (lifecycle, core) = (include_str!("atlas_lifecycle.rs"), include_str!("core.rs"));
    check_settlement_boundaries(lifecycle, core);
    check_settlement_boundaries(&lifecycle.replace('\n', "\r\n"), &core.replace('\n', "\r\n"));
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

#[test]
fn interval_release_is_due_at_thirty_seconds_of_absence_only_for_a_promoted_atlas() {
    // The interval trigger fires at exactly 30 s without renderable media, never for a placeholder or
    // while media is visible (no absence instant).
    let absent_since = Instant::now();
    let just_before = absent_since + IMAGE_ATLAS_IDLE_INTERVAL - Duration::from_millis(1);
    let at_deadline = absent_since + IMAGE_ATLAS_IDLE_INTERVAL;
    assert_eq!(IMAGE_ATLAS_IDLE_INTERVAL, Duration::from_secs(30));
    assert!(!image_atlas_release_due(true, Some(absent_since), just_before));
    assert!(image_atlas_release_due(true, Some(absent_since), at_deadline));
    assert!(image_atlas_release_due(
        true,
        Some(absent_since),
        at_deadline + Duration::from_secs(5)
    ));
    assert!(!image_atlas_release_due(false, Some(absent_since), at_deadline));
    assert!(!image_atlas_release_due(true, None, at_deadline));
}

#[test]
fn absence_starts_once_and_clears_when_media_returns() {
    // The absence instant is set by the first media-free assembly and kept by later ones, so repeated
    // idle frames never push the deadline back; a frame with media clears it.
    let first = Instant::now();
    let later = first + Duration::from_secs(10);
    assert_eq!(next_inline_media_absent_since(None, false, first), Some(first));
    assert_eq!(next_inline_media_absent_since(Some(first), false, later), Some(first));
    assert_eq!(next_inline_media_absent_since(Some(first), true, later), None);
    assert_eq!(next_inline_media_absent_since(None, true, later), None);
}

#[test]
fn release_deadline_exists_only_while_the_atlas_is_promoted() {
    // A placeholder atlas has nothing to release, so it arms no deadline even after long absence.
    let absent_since = Instant::now();
    assert_eq!(
        image_atlas_release_deadline_for(true, Some(absent_since)),
        Some(absent_since + IMAGE_ATLAS_IDLE_INTERVAL)
    );
    assert_eq!(image_atlas_release_deadline_for(false, Some(absent_since)), None);
    assert_eq!(image_atlas_release_deadline_for(true, None), None);
}

/// The release-body scan over `source`: one placeholder build, inside the shared body, called by both
/// triggers, and the service-time entry re-checks the rule.
fn check_release_triggers(source: &str) {
    // A CRLF checkout is read as LF, so the method-end delimiters match either way.
    let source = source.replace("\r\n", "\n");
    assert_eq!(
        source.matches("GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)").count(),
        1
    );
    let body = source.split_once("fn release_image_atlas(").unwrap().1;
    let body = body.split_once("\n    }\n").unwrap().0;
    assert!(body.contains("GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)"));
    assert!(body.contains("reason,"));
    assert!(source.contains("self.release_image_atlas(\"idle_frames\")"));
    assert!(source.contains("self.release_image_atlas(\"idle_interval\")"));
    let idle = source.split_once("pub fn release_idle_image_atlas(").unwrap().1;
    let idle = idle.split_once("\n    }\n").unwrap().0;
    assert!(idle.contains("image_atlas_release_due("), "the service-time call re-checks the rule");
}

#[test]
fn both_release_triggers_share_one_body_and_name_their_reason() {
    // The frame-count and interval triggers release through one body whose debug line carries the reason.
    // Windows CI checks sources out with CRLF line ends, so the scan runs on a CRLF copy too.
    let lf = include_str!("atlas_lifecycle.rs").replace("\r\n", "\n");
    check_release_triggers(&lf);
    check_release_triggers(&lf.replace('\n', "\r\n"));
}
