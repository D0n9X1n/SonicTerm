//! Dirty-rectangle coalescing and bounded staging retention.

use super::super::atlas_upload_tests::*;
use super::*;

#[test]
fn coalesces_touching_rows_and_columns() {
    let input = [
        coverage_rect(0, 0, 2, 1),
        coverage_rect(2, 0, 2, 1),
        coverage_rect(0, 1, 4, 1),
        coverage_rect(7, 7, 1, 1),
    ];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, [coverage_rect(0, 0, 4, 2), coverage_rect(7, 7, 1, 1)]);
}

#[test]
fn separate_dirty_regions_remain_separate() {
    let input = [coverage_rect(0, 0, 1, 1), coverage_rect(2, 0, 1, 1)];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, input);
}

/// Touching writes coalesce within one interpretation but never across coverage and color.
#[test]
fn coalescing_keeps_touching_pixel_kinds_separate() {
    let input = [coverage_rect(0, 0, 1, 1), coverage_rect(1, 0, 1, 1), color_rect(2, 0, 1, 1)];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, [coverage_rect(0, 0, 2, 1), color_rect(2, 0, 1, 1)]);
}

/// Coverage uploads preserve every CPU atlas byte while reusing retained staging.
#[test]
fn coverage_copies_tightly_packed_subrect_and_reuses_capacity() {
    let pixels: Vec<u8> = (0..48).collect();
    let rect = coverage_rect(1, 0, 2, 2);
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, 4, rect, &mut scratch);
    let capacity = scratch.capacity();

    assert_eq!(scratch, [4, 5, 6, 7, 8, 9, 10, 11, 20, 21, 22, 23, 24, 25, 26, 27]);

    copy_rect_into_scratch(&pixels, 4, coverage_rect(0, 0, 1, 1), &mut scratch);
    assert_eq!(scratch, [0, 1, 2, 3]);
    assert_eq!(scratch.capacity(), capacity);
}

/// DirectWrite subpixel coverage remains byte-exact through kind-directed GPU staging.
#[test]
fn subpixel_coverage_staging_preserves_bgra_channels() {
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&[10, 20, 30, 40], 1, coverage_rect(0, 0, 1, 1), &mut scratch);

    assert_eq!(scratch, [10, 20, 30, 40]);
}

/// Color uploads convert encoded premultiplication to linear-light premultiplication for an sRGB view.
#[test]
fn premultiplied_srgb_converts_representative_pixels() {
    let pixels = [128, 128, 128, 255, 64, 64, 64, 128, 16, 32, 64, 128, 77, 88, 99, 0];
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, 4, color_rect(0, 0, 4, 1), &mut scratch);

    assert_eq!(&scratch[0..4], &[128, 128, 128, 255]);
    for (actual, expected) in scratch[4..8].iter().zip([92, 92, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
    for (actual, expected) in scratch[8..12].iter().zip([20, 44, 92, 128]) {
        assert!(actual.abs_diff(expected) <= 1, "actual={actual}, expected about {expected}");
    }
    assert_eq!(&scratch[12..16], &[0, 0, 0, 0]);
}

/// Both encoding modes reuse one bounded staging allocation rather than retaining converted atlases.
#[test]
fn staging_capacity_is_reused_and_bounded_in_both_modes() {
    let width = 64u32;
    let height = 8u32;
    let pixels = vec![128u8; width as usize * height as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, width, color_rect(0, 0, width, height), &mut scratch);
    let capacity = scratch.capacity();
    copy_rect_into_scratch(&pixels, width, coverage_rect(0, 0, width, height), &mut scratch);

    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;
    assert_eq!(scratch.capacity(), capacity);
    assert!(scratch.capacity() <= whole_atlas);
}

/// Growing staging to one full atlas never retains geometric spare capacity above that bound.
#[test]
fn full_atlas_staging_growth_stays_at_the_atlas_bound() {
    let width = ATLAS_DIM;
    let pixels = vec![0u8; width as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();

    copy_rect_into_scratch(
        &pixels,
        width,
        coverage_rect(0, 0, width, ATLAS_DIM * 3 / 4),
        &mut scratch,
    );
    copy_rect_into_scratch(&pixels, width, coverage_rect(0, 0, width, ATLAS_DIM), &mut scratch);

    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;
    assert_eq!(scratch.capacity(), whole_atlas);
}

/// What the retained staging buffer can hold, measured rather than assumed.
///
/// The test above establishes that capacity survives the call — deliberately,
/// because reallocating per frame on this path would cost more than the memory
/// does. What it does not say is how much memory that is, and the class is
/// recorded `UnchargedRetention` on exactly that figure, so it is worth having
/// under a test rather than only in a comment.
///
/// Derived from the atlas constants rather than typed, because a dirty rect
/// cannot exceed the atlas it comes from.
#[test]
fn retained_staging_is_bounded_by_one_whole_atlas() {
    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;

    assert_eq!(
        whole_atlas,
        16 * 1024 * 1024,
        "a full-atlas dirty rect stages this much, and it is retained after the copy"
    );

    // Measured on the real function rather than argued from the constants: a
    // copy of a given size leaves at least that much capacity behind.
    let width = 512u32;
    let height = 64u32;
    let pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();
    copy_rect_into_scratch(&pixels, width, coverage_rect(0, 0, width, height), &mut scratch);

    let copied = width as usize * height as usize * BYTES_PER_PIXEL as usize;
    assert!(
        scratch.capacity() >= copied,
        "a {copied}-byte copy left {} bytes of capacity; the retained figure scales with \
         the rect, up to {whole_atlas} for a whole atlas",
        scratch.capacity()
    );
}

/// The recorded figure must be the atlas arithmetic, not a number beside it.
///
/// `sonicterm-types` is a contract crate and cannot depend on
/// `sonicterm-text`, so the figure in the coverage table is typed rather than
/// derived — the same constraint that makes the pane-charge list a hand-kept
/// mirror. A typed figure drifts silently when `ATLAS_DIM` changes, and every
/// accounting defect this milestone found had that shape.
///
/// This crate sees both, so the check belongs here: it recomputes the figure
/// from the constants and compares it to what the table records. Change the
/// atlas and this fails until the row is updated.
#[test]
fn the_recorded_class_figure_matches_the_atlas_constants() {
    use sonicterm_types::{ClassCoverage, ResourceClass};

    let per_upload = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;
    // One for glyphs, one for images; `GpuRenderer` holds both for its life.
    let uploads_per_renderer = 2;
    // Each upload also holds a dirty and a coalesced rect list of at most MAX_ATLAS_ENTRIES rects,
    // doubled for Vec growth, until its sync releases them.
    let rect_lists = uploads_per_renderer
        * 2
        * 2
        * sonicterm_text::glyph_atlas::MAX_ATLAS_ENTRIES
        * std::mem::size_of::<DirtyRect>();
    let derived = per_upload * uploads_per_renderer + rect_lists;

    let ClassCoverage::UnchargedRetention { per_owner_bytes } =
        ResourceClass::UploadStaging.coverage()
    else {
        panic!(
            "`UploadStaging` retains its staging buffer between copies and nothing charges \
             it, so it must record `UnchargedRetention` with what it holds"
        );
    };

    assert_eq!(
        per_owner_bytes, derived,
        "the coverage table records {per_owner_bytes} bytes for `UploadStaging` but the \
         atlas constants give {derived} ({uploads_per_renderer} x {per_upload}); the typed \
         figure has drifted from the atlas it describes"
    );
}

/// After an earlier sync, growth re-uploads every tile typed by its own kind: the coverage tile is
/// copied, the translucent color tile converted, and the bytes equal a fresh atlas's first upload.
#[test]
fn a_growth_reupload_is_typed_and_equals_a_fresh_first_upload() {
    let mono = list_tile(4, false, [0, 0, 0, 90]);
    let color = list_tile(4, true, [40, 30, 20, 128]);
    let fill = |atlas: &mut GlyphAtlas| {
        atlas.get_or_insert(list_key(0), &mut TestTileRasterizer(mono.clone()));
        atlas.get_or_insert(list_key(1), &mut TestTileRasterizer(color.clone()));
    };
    let mut grown = GlyphAtlas::growable(sonicterm_text::glyph_atlas::MIN_ATLAS_DIM, ATLAS_DIM);
    fill(&mut grown);
    let _earlier_sync = staged_writes(&mut grown);
    assert!(grown.grow_to(512), "the next doubling is allowed");
    let reupload = staged_writes(&mut grown);
    let mut fresh = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    fill(&mut fresh);
    assert_eq!(reupload, staged_writes(&mut fresh), "the re-upload equals a first upload");
    let kinds: Vec<AtlasPixelKind> = reupload.iter().map(|(rect, _)| rect.kind).collect();
    assert_eq!(kinds, [AtlasPixelKind::Coverage, AtlasPixelKind::Color]);
    assert_eq!(reupload[0].1, vec![90; 4 * 4 * 4], "coverage is copied into all four channels");
    assert_ne!(reupload[1].1, [40, 30, 20, 128].repeat(16), "translucent color is converted");
}

/// A sync of 5,000 rects reports its work, then leaves both lists empty with capacity at most 64,
/// so the reported figures fall; the glyph upload and the image upload behave alike.
#[test]
fn a_large_sync_reports_its_work_then_releases_both_lists() {
    let growable = GlyphAtlas::growable(sonicterm_text::glyph_atlas::MIN_ATLAS_DIM, ATLAS_DIM);
    for (name, mut atlas) in [("glyph", growable), ("image", GlyphAtlas::default_size())] {
        let mut rasterizer = TestTileRasterizer(list_tile(8, false, [0, 0, 0, 200]));
        for index in 0..5000 {
            atlas.get_or_insert(list_key(index), &mut rasterizer);
        }
        let dim = atlas.width() as usize;
        let atlas_before = atlas.retained_amount().bytes;
        assert_eq!(atlas_before, dim * dim * 4 + atlas.dirty_capacity() * 20, "{name}");
        let mut lists = UploadLists::default();
        // Lists left at 5,000 capacity, as a growth re-upload leaves them during its sync.
        lists.dirty_rects.reserve(5000);
        lists.coalesced_rects.reserve(5000);
        let lists_before = lists.retained_list_bytes();
        let (mut writes, mut written_bytes) = (0, 0);
        let stats = lists.sync_with(&mut atlas, |_, staged| {
            writes += 1;
            written_bytes += staged.len();
        });
        assert_eq!(stats.dirty_rects, 5000, "{name}");
        assert_eq!((stats.upload_calls, stats.uploaded_bytes), (writes, written_bytes), "{name}");
        assert!(stats.upload_calls > 0 && stats.uploaded_bytes >= 5000 * 8 * 8 * 4, "{name}");
        assert!(lists.dirty_rects.is_empty() && lists.coalesced_rects.is_empty(), "{name}");
        assert!(lists.dirty_rects.capacity() <= UPLOAD_LIST_RETAINED, "{name}");
        assert!(lists.coalesced_rects.capacity() <= UPLOAD_LIST_RETAINED, "{name}");
        assert!(atlas.dirty_capacity() <= UPLOAD_LIST_RETAINED, "{name}");
        assert!(lists.retained_list_bytes() < lists_before, "{name}: UploadStaging falls");
        assert!(atlas.retained_amount().bytes < atlas_before, "{name}: glyph_atlas falls");
        let glyph_envelope = envelope_bytes(sonicterm_types::ResourceClass::GlyphAtlas);
        assert!(atlas_before <= glyph_envelope, "{name}");
        let staging_envelope = envelope_bytes(sonicterm_types::ResourceClass::UploadStaging);
        assert!(2 * lists_before <= staging_envelope, "{name}: two uploads' lists fit");
    }
}

/// A sync with nothing dirty writes nothing, reports idle stats, and still releases lists left
/// oversized by an earlier sync.
#[test]
fn a_zero_dirty_sync_releases_oversized_lists() {
    let mut lists = UploadLists::default();
    lists.dirty_rects.reserve(5000);
    lists.coalesced_rects.reserve(5000);
    let mut atlas = GlyphAtlas::new(16, 16);
    let stats = lists.sync_with(&mut atlas, |_, _| panic!("nothing is dirty"));
    assert_eq!(stats, AtlasUploadStats::default());
    assert!(lists.dirty_rects.capacity() <= UPLOAD_LIST_RETAINED);
    assert!(lists.coalesced_rects.capacity() <= UPLOAD_LIST_RETAINED);
}

/// The envelopes restate the growth maximum plus the rect lists: one atlas and its dirty list
/// for `GlyphAtlas`; two atlas scratches and two uploads' two lists for `UploadStaging`. The
/// renderer reports both uploads' lists inside the `UploadStaging` part.
#[test]
fn envelopes_and_the_staging_report_include_the_rect_lists() {
    use sonicterm_text::glyph_atlas::MAX_ATLAS_ENTRIES;
    let rect_bytes = std::mem::size_of::<DirtyRect>();
    assert_eq!(rect_bytes, 20);
    let atlas_bytes = (ATLAS_DIM * ATLAS_DIM * BYTES_PER_PIXEL) as usize;
    assert_eq!(
        envelope_bytes(sonicterm_types::ResourceClass::GlyphAtlas),
        atlas_bytes + 2 * MAX_ATLAS_ENTRIES * rect_bytes
    );
    assert_eq!(
        envelope_bytes(sonicterm_types::ResourceClass::UploadStaging),
        2 * atlas_bytes + 2 * 2 * 2 * MAX_ATLAS_ENTRIES * rect_bytes
    );
    let core: String = include_str!("core.rs").split_whitespace().collect();
    assert!(core.contains("vertex_scratch:self.upload_staging_retained(),"));
    let report = core.split_once("fnupload_staging_retained(&self)").unwrap().1;
    let report = report.split_once("pubfninvalidate_pane_caches").expect("next method").0;
    let call = "upload_staging_amount(self.present_pipeline.vertex_scratch_retained(),\
                &self.glyph_upload,&self.image_upload,)";
    assert!(report.contains(call), "the renderer reports through the tested sum");
}

/// The renderer's `UploadStaging` part counts every byte its two uploads keep after a real sync,
/// the staging buffer included: after a 5,000-tile glyph sync and an image sync on a headless
/// queue, it equals the vertex scratch plus both uploads' lists and staging buffers.
#[test]
fn the_staging_part_counts_each_uploads_staging_buffer_after_a_large_sync() {
    let (device, queue) = headless_device();
    let pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        1,
    );
    let mut glyphs = GlyphAtlas::growable(sonicterm_text::glyph_atlas::MIN_ATLAS_DIM, ATLAS_DIM);
    let mut rasterizer = TestTileRasterizer(list_tile(8, false, [0, 0, 0, 200]));
    for index in 0..5000 {
        glyphs.get_or_insert(list_key(index), &mut rasterizer);
    }
    let mut images = GlyphAtlas::new(64, 64);
    images.get_or_insert(list_key(0), &mut TestTileRasterizer(list_tile(16, true, [9, 8, 7, 255])));
    let mut glyph_upload = AtlasUpload::new_sized(
        &device,
        glyphs.width(),
        glyphs.height(),
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );
    let mut image_upload = AtlasUpload::new(
        &device,
        &images,
        pipeline.image_bind_group_layout(),
        AtlasBindingKind::Image,
    );
    let glyph_stats = glyph_upload.sync(&queue, &mut glyphs);
    let _image_stats = image_upload.sync(&queue, &mut images);
    assert_eq!(glyph_stats.dirty_rects, 5000, "the large sync ran");
    assert!(glyph_upload.lists.scratch.capacity() > 0, "precondition: staging is retained");

    let vertex = pipeline.vertex_scratch_retained();
    let reported = crate::core::upload_staging_amount(vertex, &glyph_upload, &image_upload);
    let kept = upload_retained_bytes(&glyph_upload) + upload_retained_bytes(&image_upload);
    assert_eq!(reported.bytes, vertex.bytes + kept, "every retained upload byte is reported");
    let staging_envelope = envelope_bytes(sonicterm_types::ResourceClass::UploadStaging);
    assert!(kept <= staging_envelope, "the uploads stay inside the UploadStaging envelope");
}

/// Releasing a promoted image atlas replaces its upload with a 1x1 mirror, which also frees the old
/// upload's staging buffer and rect lists, so the renderer's `UploadStaging` part drops by them as
/// well as the image atlas part dropping. After a 32x32 image sync, the old upload keeps at least one
/// 32x32 staged write and the 1x1 replacement keeps nothing, and the reported part moves by exactly
/// what the old upload kept. A caller checking the aggregate must count both parts.
#[test]
fn releasing_the_image_upload_frees_its_staging_from_the_upload_staging_part() {
    let (device, queue) = headless_device();
    let pipeline = crate::wezterm_pipeline::WeztermPipeline::new(
        &device,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        1,
    );
    let glyph_upload = AtlasUpload::new_sized(
        &device,
        1,
        1,
        pipeline.glyph_bind_group_layout(),
        AtlasBindingKind::Glyph,
    );
    let mut images = GlyphAtlas::default_size();
    images.get_or_insert(list_key(0), &mut TestTileRasterizer(list_tile(32, true, [9, 8, 7, 255])));
    let mut promoted = AtlasUpload::new(
        &device,
        &images,
        pipeline.image_bind_group_layout(),
        AtlasBindingKind::Image,
    );
    let _stats = promoted.sync(&queue, &mut images);
    let vertex = pipeline.vertex_scratch_retained();
    let before = crate::core::upload_staging_amount(vertex, &glyph_upload, &promoted).bytes;
    let promoted_kept = upload_retained_bytes(&promoted);
    assert!(promoted_kept >= 32 * 32 * 4, "the 32x32 write stays staged: {promoted_kept}");
    // The release body: the image mirror is rebuilt at the placeholder size.
    let released = AtlasUpload::new_sized(
        &device,
        1,
        1,
        pipeline.image_bind_group_layout(),
        AtlasBindingKind::Image,
    );
    assert_eq!(upload_retained_bytes(&released), 0, "a fresh mirror keeps no staging");
    let after = crate::core::upload_staging_amount(vertex, &glyph_upload, &released).bytes;
    println!("image upload staging released: {promoted_kept} bytes ({before} -> {after})");
    assert_eq!(before - after, promoted_kept, "the part drops by everything the upload kept");
}

/// Headless wgpu draws the scrolled frame's warm (replayed) and cold (shaped) rows to the same
/// pixels. Uploading drains an atlas's pending tiles, so each side draws from its own atlas,
/// built by the same deterministic sequence; the atlases are checked equal first.
#[test]
fn gpu_readback_of_warm_and_cold_rows_is_identical() {
    let (mut cold_atlas, cold, _, (width, height)) = crate::core::warm_and_cold_row_glyphs();
    let (mut warm_atlas, _, warm, _) = crate::core::warm_and_cold_row_glyphs();
    assert_eq!(cold_atlas.pixels_bgra(), warm_atlas.pixels_bgra(), "both atlases packed alike");
    let clear = [0.05, 0.05, 0.08, 1.0];
    let cold_pixels =
        render_instances_readback(&mut cold_atlas, &[], &cold, (width, height), clear);
    let warm_pixels =
        render_instances_readback(&mut warm_atlas, &[], &warm, (width, height), clear);
    let first = cold_pixels[..4].to_vec();
    assert!(cold_pixels.chunks(4).filter(|pixel| *pixel != first.as_slice()).count() > 50);
    assert!(warm_pixels == cold_pixels, "warm and cold GPU readbacks differ");
}

/// The real sequence: a full write, a growth re-upload of every resident tile, then a sync with
/// nothing dirty. The zero-dirty sync releases the staging buffer the re-upload left behind.
#[test]
fn a_zero_dirty_sync_after_a_growth_reupload_releases_the_staging_buffer() {
    let (mut lists, mut atlas) = after_large_sync();
    assert!(atlas.grow_to(ATLAS_DIM), "the next doubling is allowed");
    let reupload = recorded_sync(&mut lists, &mut atlas);
    assert_eq!(reupload.iter().max(), Some(&LARGE_TILE_BYTES), "growth re-uploads the tile");
    assert!(lists.scratch.capacity() > MIB, "precondition: staging over 1 MiB before the sync");
    assert!(recorded_sync(&mut lists, &mut atlas).is_empty(), "nothing is dirty");
    assert_eq!(lists.scratch.capacity(), 0);
}

/// A large sync then a small one: the small sync releases the large buffer to at most twice
/// its own write.
#[test]
fn a_small_sync_after_a_large_one_shrinks_the_staging_buffer() {
    let (mut lists, mut atlas) = after_large_sync();
    insert_coverage(&mut atlas, 1, 64);
    let capacity = lists.scratch.capacity();
    // Over 1 MiB is also over four small tiles (64 KiB), so the shrink rule applies.
    assert!(capacity > MIB, "precondition: {capacity}");
    assert_eq!(recorded_sync(&mut lists, &mut atlas), [SMALL_TILE_BYTES]);
    assert!(lists.scratch.capacity() <= 2 * SMALL_TILE_BYTES, "{}", lists.scratch.capacity());
}

/// The rule keys on the largest write, not the last: a sync whose large coverage write comes
/// before a small colour write keeps the large capacity.
#[test]
fn the_staging_rule_keys_on_the_largest_write_not_the_last() {
    let mut atlas = GlyphAtlas::growable(1024, ATLAS_DIM);
    insert_coverage(&mut atlas, 0, 768);
    let color = list_tile(32, true, [9, 8, 7, 255]);
    assert!(atlas.get_or_insert(list_key(1), &mut TestTileRasterizer(color)).is_some());
    let mut lists = UploadLists::default();
    assert_eq!(recorded_sync(&mut lists, &mut atlas), [LARGE_TILE_BYTES, 32 * 32 * 4]);
    assert!(lists.scratch.capacity() >= LARGE_TILE_BYTES, "{}", lists.scratch.capacity());
}

/// Repeated small syncs, each writing a new glyph, reuse one staging allocation.
#[test]
fn repeated_small_syncs_reuse_one_staging_allocation() {
    let mut atlas = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    let mut lists = UploadLists::default();
    insert_coverage(&mut atlas, 0, 64);
    assert_eq!(recorded_sync(&mut lists, &mut atlas), [SMALL_TILE_BYTES]);
    let (pointer, capacity) = (lists.scratch.as_ptr(), lists.scratch.capacity());
    for index in 1..5 {
        insert_coverage(&mut atlas, index, 64);
        assert_eq!(recorded_sync(&mut lists, &mut atlas), [SMALL_TILE_BYTES]);
        assert_eq!((lists.scratch.as_ptr(), lists.scratch.capacity()), (pointer, capacity));
    }
}

/// The floor is strict: a staging buffer of exactly 1 MiB survives a zero-dirty sync.
#[test]
fn a_staging_buffer_of_exactly_one_mib_is_kept() {
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let mut lists = UploadLists::default();
    insert_coverage(&mut atlas, 0, 512);
    assert_eq!(recorded_sync(&mut lists, &mut atlas), [MIB]);
    assert_eq!(lists.scratch.capacity(), MIB, "precondition: exactly 1 MiB");
    assert!(recorded_sync(&mut lists, &mut atlas).is_empty());
    assert_eq!(lists.scratch.capacity(), MIB);
}

/// A trim releases the staging buffer and both rect lists entirely.
#[test]
fn release_scratch_leaves_nothing_retained() {
    let (mut lists, _atlas) = after_large_sync();
    lists.dirty_rects.reserve(100);
    lists.coalesced_rects.reserve(100);
    assert!(lists.retained_bytes() > MIB, "precondition: storage is retained");
    lists.release_scratch();
    assert_eq!(lists.retained_bytes(), 0);
}
