//! Renderer atlas policy contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn atlas_frame_detector_qualifies_equal_content_by_allocation_and_device() {
    // Equal atlas-local identities cannot authorize UVs belonging to another allocation or device.
    let atlas = GlyphAtlas::new(2, 1);
    let replacement = GlyphAtlas::new(2, 1);
    let device = DeviceErrorState::new();
    let other_device = DeviceErrorState::new();
    let before = GlyphContentStamp::capture(device.generation(), 3, &atlas);
    assert_eq!(atlas.identity(), replacement.identity());
    for after in [
        GlyphContentStamp::capture(device.generation(), 4, &replacement),
        GlyphContentStamp::capture(other_device.generation(), 3, &replacement),
    ] {
        assert_eq!(before.content_identity, after.content_identity);
        assert!(atlas_changed_during_frame(before, after));
        let cache = PreeditGlyphCache {
            text: "preedit".to_string(),
            font_size: 14.0,
            start_x: 0.0,
            top_y: 0.0,
            color_bits: 0xFFFFFFFF,
            atlas_stamp: before,
            glyphs: Vec::new(),
            missing_boxes: Vec::new(),
            missing_chrome_chars: Vec::new(),
        };
        assert!(!cache.matches("preedit", 14.0, 0.0, 0.0, 0xFFFFFFFF, after));
    }
}

#[test]
fn atlas_frame_detector_production_capture_and_retry_precede_presentation() {
    // Frame and preedit checks share qualified identity; diagnostic eviction counts cannot admit a frame.
    let source = crate::core::CORE_SOURCE;
    let start = source.find("let atlas_stamp_at_frame_start = self.glyph_atlas_stamp();").unwrap();
    // `assemble_frame` hands the start stamp and the current one to the shared pass-end helper,
    // which checks them before any batch reaches the presenter.
    let handoff = source.find("atlas_stamp_now: self.glyph_atlas_stamp(),").unwrap();
    let call = source.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    assert!(start < handoff && handoff < call);
    let guard = source
        .find("if atlas_changed_during_frame(pass.atlas_stamp_at_start, pass.atlas_stamp_now)")
        .unwrap();
    let retry = source[guard..].find("return Err(Assembled::AtlasRetry {").unwrap() + guard;
    let present = source.find("let outcome = outcome?;").unwrap();
    let acknowledge = source.find("self.finish_successful_frame(").unwrap();
    assert!(guard < retry && present < acknowledge);
    let releasing = method_body(source, "    pub fn render_releasing(");
    assert!(
        releasing.find("lend_and_assemble(").unwrap()
            < releasing.find("self.present_layers(").unwrap()
    );
    let assembly = method_body(source, "    pub(super) fn assemble_frame(");
    assert!(
        assembly.find("Self::finish_assembly_pass(").unwrap()
            < assembly.find("Ok(Assembled::Layers(").unwrap()
    );
    assert!(source.contains("let atlas_evictions_at_frame_start = self.glyph_atlas.evictions();"));
    let lifecycle = include_str!("atlas_lifecycle.rs");
    let capture = lifecycle.split_once("fn glyph_atlas_stamp(&self)").unwrap().1;
    let capture = capture.split_once("fn mark_glyph_atlas_replaced").unwrap().0;
    assert!(capture.contains("self.device_errors.generation()"));
    assert!(capture.contains("self.glyph_atlas_generation"));
    assert!(capture.contains("&self.glyph_atlas"));
    assert_eq!(source.matches("atlas_stamp: self.glyph_atlas_stamp()").count(), 1);
}

#[test]
fn atlas_reset_during_frame_requires_retry() {
    // Reset can recycle UVs without changing eviction counts, so the assembled frame must retry.
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
        .expect("first glyph fills the atlas");
    let frame_epoch = atlas.evictions();
    let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);
    let frame_identity = atlas.identity();
    assert!(!atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)));

    atlas.reset_in_place();
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
        .expect("replacement glyph reuses the reset atlas");

    assert_eq!(atlas.evictions(), frame_epoch);
    assert_ne!(atlas.identity(), frame_identity);
    assert!(
        atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)),
        "a frame must retry when reset recycled its glyph coordinates"
    );
}

#[test]
fn atlas_frame_detector_rejects_repeated_eviction_count_after_reset() {
    // A reset followed by matching diagnostic counts must not resurrect an earlier frame's UVs.
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    for ch in ['a', 'b'] {
        atlas.get_or_insert(sonicterm_types::GlyphKey::new(ch, false, false), &mut raster).unwrap();
        atlas.tick_frame();
    }
    let frame_evictions = atlas.evictions();
    let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);
    assert_eq!(frame_evictions, 1);
    atlas.reset_in_place();
    for ch in ['c', 'd'] {
        atlas.get_or_insert(sonicterm_types::GlyphKey::new(ch, false, false), &mut raster).unwrap();
        atlas.tick_frame();
    }
    assert_eq!(atlas.evictions(), frame_evictions);
    assert_ne!(atlas.identity(), frame_stamp.content_identity);
    assert!(atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)));
}

#[test]
fn atlas_frame_detector_accepts_stable_contents_and_nonrecycling_admission() {
    // Lookup, aging and unused-space admission preserve UVs both before and after a reset.
    let mut atlas = GlyphAtlas::new(2, 1);
    let mut raster = OnePixelAtlasGlyph;
    for reset in [false, true] {
        if reset {
            atlas.reset_in_place();
            assert_ne!(atlas.identity(), atlas.evictions());
        }
        atlas
            .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
            .unwrap();
        let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);
        atlas.tick_frame();
        atlas
            .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
            .unwrap();
        atlas
            .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
            .unwrap();
        assert!(!atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)));
    }
}

#[test]
fn atlas_eviction_during_frame_requires_retry() {
    // Recycling a real atlas slot must reject previously emitted UVs before presentation.
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
        .expect("first glyph fills the atlas");
    let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);

    atlas.tick_frame();
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
        .expect("second glyph evicts and reuses the only slot");

    assert!(
        atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)),
        "a frame must not present instances whose UV rectangles may have been recycled"
    );
}

/// Row-cache UVs must miss after reset even when the eviction counter repeats.
#[test]
fn row_cache_uses_nonrepeating_atlas_identity() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut cache = sonicterm_text::row_glyph_cache::RowGlyphCache::new();
    cache.begin_frame(&[(7, 1, 8)]);
    let stale_identity = row_cache_atlas_identity(&atlas);
    let stale_evictions = atlas.evictions();
    assert!(cache.insert(
        7,
        13,
        stale_identity,
        sonicterm_text::row_glyph_cache::CachedRow::default()
    ));

    atlas.reset_in_place();

    assert_eq!(atlas.evictions(), stale_evictions, "the diagnostic counter repeats after reset");
    assert_ne!(row_cache_atlas_identity(&atlas), stale_identity);
    assert!(cache.get(7, 13, row_cache_atlas_identity(&atlas), |_| true).is_none());
}

#[test]
fn same_size_atlas_reset_reuses_gpu_texture() {
    assert!(!atlas_texture_rebuild_required((2048, 2048), (2048, 2048)));
    assert!(atlas_texture_rebuild_required((1024, 1024), (2048, 2048)));
}

#[test]
fn equal_scale_factor_does_not_rebuild_atlas() {
    assert!(!scale_factor_rebuild_required(1.0, 1.0));
    assert!(!scale_factor_rebuild_required(0.1, 0.0));
    assert!(scale_factor_rebuild_required(1.0, 1.25));
}

/// CPU color composition decodes the original atlas without rewriting its storage.
#[test]
fn windows_software_presenter_keeps_cpu_color_atlas_storage_unchanged() {
    const SOURCE: &str = include_str!("software_frame.rs");

    assert!(SOURCE.contains("let atlas_pixels = atlas.pixels_bgra();"));
    assert!(SOURCE.contains("premultiplied_srgb_bgra_to_linear_rgba"));
    assert!(SOURCE.contains("blend_premul_linear_over_srgb_bgra("));
    assert!(!SOURCE.contains("copy_rect_into_scratch"));
}

/// Atlas roles keep linear-filtered images separate from dual-view nearest glyph sampling.
#[test]
fn atlas_sync_and_bind_groups_are_wired_by_role() {
    // `core.rs` builds the atlas uploads; the wgpu presenter in `present.rs` syncs and binds them.
    let source =
        [crate::core::CORE_SOURCE, include_str!("present.rs"), include_str!("atlas_lifecycle.rs")]
            .concat()
            .replace("\r\n", "\n");

    assert!(source.contains("self.image_upload.sync(&self.queue, &mut self.image_atlas)"));
    assert!(source.contains("self.glyph_upload.sync(&self.queue, &mut self.glyph_atlas)"));
    assert!(source.contains("self.image_upload.image_bind_group()"));
    assert!(source.contains("self.glyph_upload.glyph_bind_group()"));
    assert!(source.contains("AtlasBindingKind::Image"));
    assert!(source.contains("AtlasBindingKind::Glyph"));
}

// --- Atlas reset / row-cache invalidation pairing ------------------

/// Every atlas reset must flush the row glyph cache and defeat the frame-skip
/// guard, in the same function.
///
/// The row cache's monotonic atlas identity rejects stale UVs even if a future
/// reset path misses this wholesale clear. The explicit invalidation remains
/// load-bearing for prompt reclamation and bounds stale entries that can never
/// match again.
///
/// `last_frame_key` is the second half: it skips presentation when a frame is
/// byte-identical to the last one. A reset changes what that same frame
/// *renders to*, so a surviving key can skip the redraw and leave pre-reset
/// pixels on screen.
///
/// Nothing structural enforces either pairing. What holds today is that all
/// four `reset_glyph_atlas_in_place` call sites happen to do both in the same
/// function. A fifth reset path missing either call reintroduces a
/// stale-pixel class with no compile error and no failing test.
///
/// Order is deliberately not asserted. Flushing before the reset is at least
/// as safe as flushing after — one arrangement hoists the invalidation above
/// the reset so it covers both transition directions — so the requirement is
/// that both calls appear in the same function, not that they appear in a
/// particular sequence.
///
/// `GpuRenderer` needs a live wgpu device and a window, so the pairing cannot
/// be driven at runtime in a unit test. Reading the source is the available
/// check, and it is the one that would actually catch the regression: the
/// mistake being guarded is an omitted call, which is visible in the text.
#[test]
fn every_glyph_atlas_reset_invalidates_the_row_cache() {
    // Include lifecycle and recovery reset sites so moving a helper cannot hide missing invalidation.
    let core_source =
        [crate::core::CORE_SOURCE, include_str!("atlas_lifecycle.rs"), include_str!("rebind.rs")]
            .join("\n");
    const RESET: &str = "self.reset_glyph_atlas_in_place(";
    const INVALIDATE: &str = "self.row_glyph_cache.invalidate_all()";
    const CLEAR_FRAME_KEY: &str = "self.last_frame_key = None";

    let lines: Vec<&str> = core_source.lines().collect();
    let reset_sites: Vec<usize> =
        lines.iter().enumerate().filter(|(_, l)| l.contains(RESET)).map(|(i, _)| i).collect();

    // Guard against the check silently passing because the call was renamed
    // and no site matches any more.
    assert!(
        reset_sites.len() >= 4,
        "expected at least the four known atlas reset sites, found {}; \
         if `reset_glyph_atlas_in_place` was renamed, update this test",
        reset_sites.len()
    );

    for &site in &reset_sites {
        // Search the whole enclosing function, not just forward from the
        // reset. Flushing the cache *before* replacing the atlas is at least
        // as safe as flushing it after, and one call site does exactly that —
        // hoisting the invalidation above the reset so it runs on both
        // transition directions. A forward-only scan would read that safer
        // arrangement as a violation, so the invariant is "somewhere in the
        // same function", not "after".
        let start = lines[..site]
            .iter()
            .rposition(|line| {
                line.starts_with("    fn ")
                    || line.starts_with("    pub fn ")
                    || line.starts_with("    pub(crate) fn ")
                    || line.starts_with("    pub(super) fn ")
            })
            .map_or(0, |index| index + 1);
        let end = lines
            .iter()
            .enumerate()
            .skip(site + 1)
            .find(|(_, line)| {
                line.starts_with("    fn ")
                    || line.starts_with("    pub fn ")
                    || line.starts_with("    pub(crate) fn ")
                    || line.starts_with("    pub(super) fn ")
                    || line.starts_with("impl ")
                    || line.starts_with("}")
            })
            .map_or(lines.len(), |(index, _)| index);

        let body = &lines[start..end];
        let invalidated = body.iter().any(|line| line.contains(INVALIDATE));
        let frame_key_cleared = body.iter().any(|line| line.contains(CLEAR_FRAME_KEY));
        assert!(
            invalidated,
            "renderer source line {} resets the glyph atlas without calling \
             `row_glyph_cache.invalidate_all()` in the same function.\n\
             The row cache identity rejects stale UVs, but reset must also \
             reclaim entries that can never match the new atlas.\n\
             Offending line: {}",
            site + 1,
            lines[site].trim()
        );
        assert!(
            frame_key_cleared,
            "renderer source line {} resets the glyph atlas without clearing \
             `last_frame_key` in the same function.\n\
             The frame key skips presentation when a frame is unchanged. A reset \
             changes what the same frame renders to, so a stale key can skip the \
             redraw and leave pre-reset pixels on screen.\n\
             Offending line: {}",
            site + 1,
            lines[site].trim()
        );
    }
}

// --- Image atlas promotion / demotion ------------------------------

/// A window with no inline media must not clear its image atlas on every
/// frame it draws.
///
/// The frame-assembly guard asks whether inline media *changed*, and that
/// question is answered `true` whenever the previous frame key is absent —
/// which is every frame following any of the many state changes that clear
/// it. The media hash itself is deterministic, so on a window that has never
/// shown an image the hash arm can never fire and the absent-key arm accounts
/// for every reset. The result is one reset per rendered frame on a window
/// with nothing to reset.
///
/// Resetting an atlas that holds nothing is not free: it rebuilds the packer
/// and bumps the atlas identity, which invalidates every dependent cache
/// keyed to it. Gating on whether the atlas actually holds anything is
/// therefore correct regardless of why the frame key was cleared.
#[test]
fn an_empty_placeholder_image_atlas_is_not_reset_every_frame() {
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);

    // The reported defect: a window with no media, drawing frames, whose
    // atlas is still the untouched 1x1 placeholder. Nothing to clear.
    assert!(
        !image_atlas_reset_warranted(&placeholder),
        "an empty placeholder atlas must not be reset; there is nothing in it to clear"
    );

    // A promoted atlas carries packer and eviction state even when its entry
    // map is momentarily empty, so it must still reset. Guarding on emptiness
    // alone would strand that state and let the packer refuse new inserts.
    let promoted = GlyphAtlas::default_size();
    assert!(
        image_atlas_reset_warranted(&promoted),
        "a promoted atlas must still be reset even while its entry map is empty"
    );
}

/// The frame-assembly call site actually consults the reset guard.
///
/// The predicate test above pins the decision, but nothing structural forces
/// frame assembly to ask. Dropping the call from the condition restores one
/// reset per rendered frame on a window with no media, and it does so with no
/// compile error and no other failing test — the predicate would simply go
/// unused at that site while every assertion about it still held.
///
/// `GpuRenderer` needs a live wgpu device and a window, so the composed guard
/// cannot be driven at runtime in a unit test. Reading the source is the
/// available check, and it is the one that catches this regression: the
/// mistake being guarded is an omitted call, which is visible in the text.
#[test]
fn image_atlas_reset_is_gated_on_the_atlas_holding_something() {
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
    const RESET_CALL: &str = "self.reset_image_atlas()";
    const GUARD: &str = "image_atlas_reset_warranted(";

    let lines: Vec<&str> = CORE_SRC.lines().collect();
    let reset_sites: Vec<usize> =
        lines.iter().enumerate().filter(|(_, l)| l.contains(RESET_CALL)).map(|(i, _)| i).collect();

    // Guard against the check silently passing because the call was renamed
    // and no site matches any more.
    assert!(
        reset_sites.len() >= 2,
        "expected at least the two known image-atlas reset sites, found {}; \
         if `reset_image_atlas` was renamed, update this test",
        reset_sites.len()
    );

    // The frame-assembly site is the per-frame one. Find it by the condition
    // that precedes it: the surface-transition site resets unconditionally
    // once, which is correct there and must not be required to carry a guard.
    let frame_site = reset_sites
        .iter()
        .copied()
        .find(|&site| {
            lines[site.saturating_sub(6)..site].iter().any(|l| l.contains("inline_media_changed"))
        })
        .expect(
            "no image-atlas reset site is preceded by an `inline_media_changed` condition; \
             if frame assembly was restructured, update this test",
        );

    let guard_window = &lines[frame_site.saturating_sub(6)..frame_site];
    assert!(
        guard_window.iter().any(|l| l.contains(GUARD)),
        "the per-frame image-atlas reset at line {} is not gated on \
         `image_atlas_reset_warranted`; without it a window with no inline media \
         resets an empty atlas on every frame it draws",
        frame_site + 1
    );
}

/// A promoted image atlas is released once the window stops showing media,
/// but not on the first idle frame.
///
/// `reset_in_place` clears the map and repacker and never touches the pixel
/// buffer, so promotion is otherwise permanent: a window that displays one
/// inline image holds 16 MiB of CPU pixels — plus a matching GPU texture off
/// the software path — until it closes. Across windows that is the largest
/// retained term in the process.
///
/// The delay is the load-bearing part. Demoting on the first frame without
/// visible media would free and reallocate the atlas every time an image
/// scrolled out of view and back, re-decoding every visible image each time.
#[test]
fn an_idle_image_atlas_is_released_but_not_on_the_first_idle_frame() {
    let promoted = GlyphAtlas::default_size();
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);

    // Media visible: never demote, however long the window has been idle
    // before — the counter resets at the call site.
    assert!(
        !image_atlas_demotion_ready(&promoted, true, IMAGE_ATLAS_IDLE_FRAMES * 10),
        "an atlas must never be released while media is on screen"
    );

    // Idle, but not yet long enough.
    assert!(
        !image_atlas_demotion_ready(&promoted, false, 0),
        "the first idle frame must not release the atlas"
    );
    assert!(
        !image_atlas_demotion_ready(&promoted, false, IMAGE_ATLAS_IDLE_FRAMES - 1),
        "one frame short of the threshold must not release the atlas"
    );

    // Idle long enough.
    assert!(
        image_atlas_demotion_ready(&promoted, false, IMAGE_ATLAS_IDLE_FRAMES),
        "a sustained absence of media must release the atlas"
    );

    // Already at placeholder size: nothing to release, so no repeated work.
    assert!(
        !image_atlas_demotion_ready(&placeholder, false, IMAGE_ATLAS_IDLE_FRAMES * 10),
        "a placeholder atlas must not be re-released every frame"
    );

    // Promotion and demotion must not both fire for the same state, or a
    // window with no media would allocate and free every frame.
    for frames in [0, IMAGE_ATLAS_IDLE_FRAMES, IMAGE_ATLAS_IDLE_FRAMES * 2] {
        for has_media in [false, true] {
            let promote = image_atlas_promotion_required(&placeholder, has_media);
            let demote = image_atlas_demotion_ready(&placeholder, has_media, frames);
            assert!(
                !(promote && demote),
                "placeholder atlas: promote and demote both fired (media={has_media}, frames={frames})"
            );
            let promote_full = image_atlas_promotion_required(&promoted, has_media);
            let demote_full = image_atlas_demotion_ready(&promoted, has_media, frames);
            assert!(
                !(promote_full && demote_full),
                "promoted atlas: promote and demote both fired (media={has_media}, frames={frames})"
            );
        }
    }
}

/// Only a change that grew the atlas, on the same device and allocation and with no eviction,
/// takes the growth retry; an eviction, a reset or a device change takes the reset path.
#[test]
fn only_a_pure_growth_takes_the_growth_retry() {
    let before = growth_stamp(1, 4, 0);
    assert!(growth_only_change(before, growth_stamp(1, 5, 1), 3, 3), "growth alone");
    assert!(growth_only_change(before, growth_stamp(1, 7, 2), 3, 3), "two growths in one frame");
    assert!(!growth_only_change(before, growth_stamp(1, 6, 1), 3, 4), "growth with eviction");
    assert!(!growth_only_change(before, growth_stamp(2, 6, 1), 3, 3), "a reset in place");
    assert!(!growth_only_change(before, growth_stamp(1, 5, 0), 3, 4), "eviction alone");
    let other_device = GlyphContentStamp { device_generation: 8, ..growth_stamp(1, 5, 1) };
    assert!(!growth_only_change(before, other_device, 3, 3), "a new device");
}

/// `Normal` takes the scale-1 start up to 1.5 and the scale-2 start above it; `Minimum` is the floor.
#[test]
fn start_dim_follows_scale_and_start_kind() {
    use sonicterm_text::glyph_atlas::{MIN_ATLAS_DIM, START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};
    for scale in [1.0, 1.25, 1.5] {
        assert_eq!(start_dim(scale, GlyphAtlasStart::Normal), START_ATLAS_DIM_1X, "{scale}");
    }
    for scale in [1.75, 2.0, 3.0] {
        assert_eq!(start_dim(scale, GlyphAtlasStart::Normal), START_ATLAS_DIM_2X, "{scale}");
    }
    for scale in [1.0, 2.0] {
        assert_eq!(start_dim(scale, GlyphAtlasStart::Minimum), MIN_ATLAS_DIM);
    }
    assert_eq!(GlyphAtlasStart::default(), GlyphAtlasStart::Normal);
}

/// Only the renderer's glyph atlas is growable: `GlyphAtlas::growable(` appears once in production
/// gpu sources, at the glyph atlas construction, and the image atlas keeps fixed constructors.
#[test]
fn only_the_glyph_atlas_is_built_growable() {
    let sources = [
        ("core.rs", crate::core::CORE_SOURCE),
        ("atlas_lifecycle.rs", include_str!("atlas_lifecycle.rs")),
        ("present.rs", include_str!("present.rs")),
        ("atlas_upload.rs", include_str!("atlas_upload.rs")),
        ("chrome_text.rs", include_str!("chrome_text.rs")),
    ]
    .map(|(name, text)| (name, text.replace("\r\n", "\n")));
    let calls: Vec<&str> = sources
        .iter()
        .flat_map(|(name, text)| text.matches("GlyphAtlas::growable(").map(move |_| *name))
        .collect();
    assert_eq!(calls, ["core.rs"]);
    let core = &sources[0].1;
    let at = core.find("GlyphAtlas::growable(").unwrap();
    assert!(
        core[..at].trim_end().ends_with("let glyph_atlas ="),
        "the call builds the glyph atlas"
    );
    let lifecycle = &sources[1].1;
    assert!(lifecycle.contains("self.image_atlas = GlyphAtlas::default_size();"));
}

/// The promoted image atlas is the fixed 2048 atlas and never grows.
#[test]
fn the_promoted_image_atlas_is_fixed_at_the_maximum() {
    let promoted = GlyphAtlas::default_size();
    assert_eq!((promoted.width(), promoted.height()), (2048, 2048));
    assert_eq!(promoted.growth_policy(), sonicterm_text::glyph_atlas::GrowthPolicy::Fixed);
    assert_eq!(promoted.growths(), 0);
}

/// The snapshot facts read the glyph atlas as it is: a grown atlas reports its new dimension,
/// its growth, the packed area and largest tile of its resident tiles, and its fit label.
#[test]
fn glyph_atlas_facts_read_a_grown_atlas() {
    use sonicterm_text::glyph_atlas::{RasterTile, ATLAS_DIM, MIN_ATLAS_DIM};
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    assert_eq!(GlyphAtlasFacts::of(&atlas).dim, MIN_ATLAS_DIM, "a fresh atlas is at its start");
    // A 30×40 coverage tile, so the packed area and largest tile are known.
    struct FactsTile;
    impl sonicterm_text::glyph_atlas::Rasterizer for FactsTile {
        fn rasterize(&mut self, _: sonicterm_types::GlyphKey) -> Option<RasterTile> {
            Some(RasterTile {
                width: 30,
                height: 40,
                offset_x: 0,
                offset_y: 0,
                advance: 30.0,
                coverage: vec![200; 30 * 40],
                is_color: false,
                is_subpixel: false,
            })
        }
    }
    let key = sonicterm_types::GlyphKey::new('x', false, false);
    let _info = atlas.get_or_insert(key, &mut FactsTile);
    assert!(atlas.grow_to(512), "the next doubling is allowed");
    let facts = GlyphAtlasFacts::of(&atlas);
    assert_eq!((facts.dim, facts.growths, facts.evictions), (512, 1, 0));
    assert_eq!((facts.packed_pixels, facts.max_tile), (30 * 40, [30, 40]));
    assert_eq!(facts.fit, atlas.fit_outcome().label());
}
