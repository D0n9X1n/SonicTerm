use super::*;

/// BGRA bytes of one 1600 x 1000 drawable, the warm spare's surface at scale 2.
const DRAWABLE_BYTES: u64 = 1600 * 1000 * 4;

/// Contract: the ledger's live figure is created minus released, per class, across the sequence a
/// renderer goes through: create, resize, covered-window trim and drop. Every install releases
/// what its slot held, and the final `release_all` returns every class to zero live bytes while
/// created and released totals stay equal.
#[test]
fn ledger_arithmetic_returns_to_zero_across_create_resize_trim_and_drop() {
    let ledger = GraphicsLedgerHandle::default();
    // Create: a 3-drawable surface, a surface-sized frame texture, both atlases, present buffers.
    ledger.replace(GraphicsSlot::Surface, DRAWABLE_BYTES * 3, 3);
    ledger.replace(GraphicsSlot::FrameTexture, DRAWABLE_BYTES, 1);
    ledger.replace(GraphicsSlot::GlyphAtlas, 2048 * 2048 * 4, 1);
    ledger.replace(GraphicsSlot::ImageAtlas, 4, 1);
    ledger.replace(GraphicsSlot::PresentVertices, 4096 * 4 * 32, 1);
    ledger.replace(GraphicsSlot::PresentIndices, 4096 * 6 * 4, 1);
    ledger.replace(GraphicsSlot::PresentUniform, 32, 1);
    let created = ledger.snapshot();
    assert_eq!(created.class(GraphicsClass::Surface).live_resources, 3);
    assert_eq!(created.class(GraphicsClass::PresentBuffers).live_resources, 3);
    assert_eq!(created.live_resources(), 9);

    // Resize to half the height: surface and frame texture are replaced, nothing else moves.
    let half = DRAWABLE_BYTES / 2;
    ledger.replace(GraphicsSlot::Surface, half * 3, 3);
    ledger.replace(GraphicsSlot::FrameTexture, half, 1);
    let resized = ledger.snapshot();
    assert_eq!(resized.class(GraphicsClass::Surface).live_bytes, half * 3);
    assert_eq!(resized.class(GraphicsClass::Surface).released_bytes, DRAWABLE_BYTES * 3);
    assert_eq!(resized.class(GraphicsClass::FrameTexture).created_resources, 2);
    assert_eq!(resized.class(GraphicsClass::GlyphAtlas), created.class(GraphicsClass::GlyphAtlas));

    // Covered-window trim: frame texture to 1x1, present buffers back to their initial size.
    ledger.replace(GraphicsSlot::FrameTexture, 4, 1);
    ledger.replace(GraphicsSlot::PresentVertices, 4096 * 4 * 32, 1);
    let trimmed = ledger.snapshot();
    assert_eq!(trimmed.class(GraphicsClass::FrameTexture).live_bytes, 4);
    assert_eq!(trimmed.live_bytes(), resized.live_bytes() - half + 4);

    // Drop: every slot released.
    ledger.release_all();
    let dropped = ledger.snapshot();
    assert_eq!(dropped.live_bytes(), 0);
    assert_eq!(dropped.live_resources(), 0);
    for class in GraphicsClass::ALL {
        let tally = dropped.class(class);
        assert_eq!(tally.created_bytes, tally.released_bytes, "{}", class.as_str());
        assert_eq!(tally.created_resources, tally.released_resources, "{}", class.as_str());
    }
    // A second drop of an empty ledger releases nothing further.
    ledger.release_all();
    assert_eq!(ledger.snapshot(), dropped);
}

/// Contract: the surface's drawable count follows the frame latency as the backend applies it:
/// Metal holds latency + 1 drawables within its `1..=2` latency range, so latency 2 gives 3 and
/// latency 1 gives 2, exactly; an out-of-range request is clamped rather than counted as asked.
#[test]
fn drawable_count_follows_latency_per_backend() {
    let count = |backend, latency| surface_drawables(backend, latency, None);
    assert_eq!(count(wgpu::Backend::Metal, 2), SurfaceDrawables { count: 3, estimated: false });
    assert_eq!(count(wgpu::Backend::Metal, 1), SurfaceDrawables { count: 2, estimated: false });
    assert_eq!(count(wgpu::Backend::Metal, 0).count, 2, "clamped up to latency 1");
    assert_eq!(count(wgpu::Backend::Metal, 5).count, 3, "clamped down to latency 2");
    assert_eq!(count(wgpu::Backend::Dx12, 2), SurfaceDrawables { count: 3, estimated: false });
    assert_eq!(count(wgpu::Backend::Dx12, 1).count, 2);
    assert_eq!(count(wgpu::Backend::Dx12, 40).count, 16, "BufferCount is at most 16");
}

/// Contract: Vulkan's count is an estimate. Without the surface's image-count range, which wgpu
/// does not expose, it is latency + 1; with a range, latency is clamped to it first, as wgpu-hal
/// does, so a surface whose minimum is 3 images requests 3 at latency 1, not 2.
#[test]
fn vulkan_counts_are_estimates_clamped_by_a_known_capability_range() {
    let vulkan = |latency, range| surface_drawables(wgpu::Backend::Vulkan, latency, range);
    assert_eq!(vulkan(2, None), SurfaceDrawables { count: 3, estimated: true });
    assert_eq!(vulkan(1, None).count, 2);
    assert_eq!(vulkan(1, Some((3, 8))), SurfaceDrawables { count: 3, estimated: true });
    assert_eq!(vulkan(2, Some((3, 8))).count, 3);
    assert_eq!(vulkan(9, Some((2, 4))).count, 4, "clamped to the maximum image count");
    assert_eq!(vulkan(2, Some((2, 2))).count, 2, "a single-image range");
}

/// Contract: GL ignores the frame latency, so its count is a fixed estimate that does not move
/// when the software-render degrade switch changes latency from 2 to 1.
#[test]
fn gl_counts_are_fixed_estimates_independent_of_latency() {
    let gl = |latency| surface_drawables(wgpu::Backend::Gl, latency, None);
    assert_eq!(gl(2), SurfaceDrawables { count: 2, estimated: true });
    assert_eq!(gl(1), gl(2), "toggling degrade does not change GL's count");
}

/// Contract: the ledger marks the live surface as estimated only while an estimated request
/// holds the surface slot; an exact replacement or a release clears the mark.
#[test]
fn the_surface_estimate_mark_follows_the_live_request() {
    let ledger = GraphicsLedgerHandle::default();
    assert_eq!(ledger.snapshot().surface_count_kind(), "exact");
    ledger.replace_request(GraphicsSlot::Surface, 30, 3, true);
    assert!(ledger.snapshot().surface_estimated);
    assert_eq!(ledger.snapshot().surface_count_kind(), "estimated");
    ledger.replace_request(GraphicsSlot::Surface, 30, 3, false);
    assert!(!ledger.snapshot().surface_estimated);
    ledger.replace_request(GraphicsSlot::Surface, 30, 3, true);
    ledger.release_all();
    assert!(!ledger.snapshot().surface_estimated);
    assert_eq!(
        ledger.snapshot().render_per_class(|tally| (tally.created_bytes, tally.created_resources)),
        "surface=90/9 frame_texture=0/0 glyph_atlas=0/0 image_atlas=0/0 present_buffers=0/0"
    );
}

/// Contract: a drawable's requested bytes are width x height x the format's bytes per pixel.
#[test]
fn drawable_bytes_use_the_format_pixel_size() {
    assert_eq!(surface_drawable_bytes(1600, 1000, wgpu::TextureFormat::Bgra8UnormSrgb), 6_400_000);
    assert_eq!(surface_drawable_bytes(10, 10, wgpu::TextureFormat::Rgba16Float), 800);
}

/// The rest of the method holding `offset` in `source`: up to the next method-closing brace.
///
/// `source` must use `\n` line endings; [`hook_violations`] normalizes before calling this, since a
/// Windows checkout has `\r\n` and the closing-brace pattern would otherwise never match.
fn method_rest(source: &str, offset: usize) -> &str {
    let rest = &source[offset..];
    &rest[..rest.find("\n    }\n").unwrap_or(rest.len())]
}

/// Check every `install` in `source` reaches `record` before its method ends, adding a violation
/// for each that does not; returns how many installs there were.
fn check_installs_record(
    violations: &mut Vec<String>,
    name: &str,
    source: &str,
    install: &str,
    record: &str,
) -> usize {
    let offsets: Vec<usize> = source.match_indices(install).map(|(offset, _)| offset).collect();
    for offset in &offsets {
        if !method_rest(source, *offset).contains(record) {
            violations.push(format!("{name}: `{install}` at byte {offset} misses `{record}`"));
        }
    }
    offsets.len()
}

/// The production sources the hook guard reads, in [`hook_violations`] order.
const SOURCES: [(&str, &str); 5] = [
    ("core.rs", include_str!("core.rs")),
    ("present.rs", include_str!("present.rs")),
    ("rebind.rs", include_str!("rebind.rs")),
    ("atlas_lifecycle.rs", include_str!("atlas_lifecycle.rs")),
    ("wezterm_pipeline.rs", include_str!("wezterm_pipeline.rs")),
];

/// Every missing ledger hook in `sources` (core, present, rebind, atlas lifecycle, pipeline).
///
/// Line endings are normalized to `\n` first, so the guard reads a Windows CRLF checkout the same.
fn hook_violations(sources: [&str; 5]) -> Vec<String> {
    let normalized = sources.map(|source| source.replace("\r\n", "\n"));
    let [core, present, rebind, atlas, pipeline] = [
        normalized[0].as_str(),
        normalized[1].as_str(),
        normalized[2].as_str(),
        normalized[3].as_str(),
        normalized[4].as_str(),
    ];
    let mut violations = Vec::new();
    let mut require = |found: bool, what: &str| {
        if !found {
            violations.push(what.to_string());
        }
    };
    // Construction attaches the pipeline and records every resource it built.
    require(
        core.contains("present_pipeline.attach_graphics_ledger(graphics_ledger.clone());"),
        "construction attaches the present pipeline",
    );
    for record in [
        "renderer.record_surface_request();",
        "renderer.record_frame_texture_request();",
        "renderer.record_glyph_atlas_request();",
        "renderer.record_image_atlas_request();",
    ] {
        require(core.contains(record), record);
    }
    // Recovery attaches the candidate pipeline to the same ledger.
    require(
        rebind.contains(
            "self.present_pipeline.attach_graphics_ledger(self.graphics_ledger.clone());",
        ),
        "recovery attaches the candidate pipeline",
    );
    // Attaching reports all three present buffers.
    let attach =
        pipeline.find("fn attach_graphics_ledger").map(|offset| method_rest(pipeline, offset));
    for record in
        ["PresentUniform", "self.record_vertex_request();", "self.record_index_request();"]
    {
        require(attach.is_some_and(|body| body.contains(record)), "attach reports its buffers");
    }
    // The drop logs its teardown record, then releases the ledger.
    let drop = core.find("impl Drop for GpuRenderer").map(|offset| method_rest(core, offset));
    let teardown = drop.and_then(|body| body.find("\"renderer graphics teardown\""));
    let release = drop.and_then(|body| body.find("self.graphics_ledger.release_all();"));
    require(
        matches!((teardown, release), (Some(logged), Some(released)) if logged < released),
        "the drop logs its teardown before releasing the ledger",
    );

    let configure = "self.surface.configure(&self.device, &self.config);";
    let mut configures = 0;
    for (name, source) in [("core.rs", core), ("present.rs", present), ("rebind.rs", rebind)] {
        for (offset, _) in source.match_indices(configure) {
            if !source[offset + configure.len()..]
                .trim_start()
                .starts_with("self.record_surface_request();")
            {
                violations.push(format!("{name}: a configure at byte {offset} is not recorded"));
            }
            configures += 1;
        }
    }
    if configures != 7 {
        violations.push(format!("{configures} surface configures, expected 7"));
    }
    let frame = "self.frame_texture = frame_texture;";
    let frame_record = "record_frame_texture_request()";
    let mut counts = vec![
        check_installs_record(&mut violations, "core.rs", core, frame, frame_record),
        check_installs_record(&mut violations, "rebind.rs", rebind, frame, frame_record),
    ];
    for (name, source) in [("atlas_lifecycle.rs", atlas), ("rebind.rs", rebind)] {
        counts.push(check_installs_record(
            &mut violations,
            name,
            source,
            "self.glyph_upload = ",
            "record_glyph_atlas_request()",
        ));
        counts.push(check_installs_record(
            &mut violations,
            name,
            source,
            "self.image_upload = ",
            "record_image_atlas_request()",
        ));
    }
    if counts != [2, 1, 1, 1, 1, 1] {
        violations.push(format!("install counts {counts:?}, expected [2, 1, 1, 1, 1, 1]"));
    }
    let vertices = check_installs_record(
        &mut violations,
        "wezterm_pipeline.rs",
        pipeline,
        "self.vertex_buf = ",
        "record_vertex_request()",
    );
    let indices = check_installs_record(
        &mut violations,
        "wezterm_pipeline.rs",
        pipeline,
        "self.index_buf = ",
        "record_index_request()",
    );
    if vertices < 2 || indices < 2 {
        violations.push(format!("present buffer installs {vertices}/{indices}, expected 2+"));
    }
    violations
}

/// Contract: every production site that installs a GPU resource records it in the ledger: each
/// `surface.configure` is followed at once by `record_surface_request`; each frame-texture,
/// atlas-upload and present-buffer install reaches its record call in the same method; the
/// constructor and the recovery commit attach the present pipeline, which reports its buffers;
/// the constructor records its frame texture and atlases; and the drop logs its teardown before
/// releasing the ledger. The real-renderer case runs only on Windows CI; this runs on every host.
#[test]
fn every_production_install_site_records_in_the_ledger() {
    let violations = hook_violations(SOURCES.map(|(_, source)| source));
    assert!(violations.is_empty(), "{violations:#?}");
}

/// Each pinned hook: its source's index in [`SOURCES`], the unique text that opens the function
/// holding it, and the call. The function anchor names the occurrence, so a call that appears in
/// two functions is deleted where this case pins it, not wherever the text first appears.
const PINNED_HOOKS: [(usize, &str, &str); 25] = [
    (0, "fn new_async(", "present_pipeline.attach_graphics_ledger(graphics_ledger.clone());"),
    (0, "fn new_async(", "renderer.record_surface_request();"),
    (0, "fn new_async(", "renderer.record_frame_texture_request();"),
    (0, "fn new_async(", "renderer.record_glyph_atlas_request();"),
    (0, "fn new_async(", "renderer.record_image_atlas_request();"),
    (0, "fn rebuild_frame_texture(", "self.record_frame_texture_request();"),
    (0, "fn trim_for_occlusion(", "self.record_frame_texture_request();"),
    (0, "fn try_resize_outcome(", "self.record_surface_request();"),
    (0, "fn set_software_render_degrade(", "self.record_surface_request();"),
    (0, "impl Drop for GpuRenderer", "\"renderer graphics teardown\""),
    (0, "impl Drop for GpuRenderer", "self.graphics_ledger.release_all();"),
    (1, "fn present_wgpu_frame(", "self.record_surface_request();"),
    (1, "fn probe_surface_availability(", "self.record_surface_request();"),
    (
        2,
        "fn commit_rebind(",
        "self.present_pipeline.attach_graphics_ledger(self.graphics_ledger.clone());",
    ),
    (2, "fn commit_rebind(", "self.record_surface_request();"),
    (2, "fn commit_rebind(", "self.record_frame_texture_request();"),
    (2, "fn commit_rebind(", "self.record_glyph_atlas_request();"),
    (2, "fn commit_rebind(", "self.record_image_atlas_request();"),
    (3, "fn rebuild_glyph_upload_if_needed(", "self.record_glyph_atlas_request();"),
    (3, "fn rebuild_image_upload_if_needed(", "self.record_image_atlas_request();"),
    (4, "fn attach_graphics_ledger(", "crate::graphics_ledger::GraphicsSlot::PresentUniform,"),
    (4, "fn recreate_buffers(", "self.record_vertex_request();"),
    (4, "fn recreate_buffers(", "self.record_index_request();"),
    (4, "fn ensure_capacity(", "self.record_vertex_request();"),
    (4, "fn ensure_capacity(", "self.record_index_request();"),
];

/// `source` with the first `needle` inside the function opened by the unique `anchor` removed.
///
/// The function ends at its closing brace with either line ending, so the deletion lands in the
/// same function in an LF and a CRLF copy.
fn delete_in_function(source: &str, anchor: &str, needle: &str) -> String {
    assert_eq!(source.matches(anchor).count(), 1, "`{anchor}` must name one function");
    let start = source.find(anchor).expect("anchor present");
    let body = &source[start..];
    let end = ["\n    }\n", "\r\n    }\r\n"]
        .iter()
        .filter_map(|closing| body.find(closing))
        .min()
        .unwrap_or(body.len());
    let offset = body[..end].find(needle).unwrap_or_else(|| panic!("`{needle}` not in `{anchor}`"));
    let at = start + offset;
    format!("{}{}", &source[..at], &source[at + needle.len()..])
}

/// Borrow five owned sources as the guard's string slices.
fn borrow(sources: &[String; 5]) -> [&str; 5] {
    sources.each_ref().map(String::as_str)
}

/// Assert the guard finds no violation in `sources`, and one after deleting each pinned hook.
fn assert_guard_catches_every_deletion(sources: [String; 5], label: &str) {
    let clean = hook_violations(borrow(&sources));
    assert!(clean.is_empty(), "{label}: unmodified sources report {clean:#?}");
    for (index, anchor, needle) in PINNED_HOOKS {
        let mut mutated = sources.clone();
        mutated[index] = delete_in_function(&sources[index], anchor, needle);
        assert!(
            !hook_violations(borrow(&mutated)).is_empty(),
            "{label}: deleting `{needle}` from `{anchor}` in {} went unnoticed",
            SOURCES[index].0
        );
    }
}

/// Contract: the guard catches the deletion of each hook it pins. Every case removes one call from
/// the function that holds it, in a copy of its source; the guard must then report a violation, so
/// a hook that could be deleted unnoticed is not counted as pinned.
#[test]
fn deleting_any_pinned_hook_fails_the_guard() {
    let sources = SOURCES.map(|(_, source)| source.replace("\r\n", "\n"));
    assert_guard_catches_every_deletion(sources, "LF");
}

/// Contract: the guard does not depend on line endings. A Windows checkout has CRLF, which once
/// made a method's end unfindable, so a deleted `rebuild_frame_texture` record was hidden by the
/// one in `trim_for_occlusion`; the same deletions in a CRLF copy must each still be reported.
#[test]
fn the_guard_catches_every_deletion_in_crlf_sources() {
    let sources = SOURCES.map(|(_, source)| source.replace("\r\n", "\n").replace('\n', "\r\n"));
    assert_guard_catches_every_deletion(sources, "CRLF");
}
