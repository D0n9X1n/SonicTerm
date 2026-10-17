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
fn method_rest(source: &str, offset: usize) -> &str {
    let rest = &source[offset..];
    &rest[..rest.find("\n    }\n").unwrap_or(rest.len())]
}

/// Assert every `install` in `source` is followed by `record` before its method ends; returns how
/// many installs there were.
fn assert_installs_record(name: &str, source: &str, install: &str, record: &str) -> usize {
    let offsets: Vec<usize> = source.match_indices(install).map(|(offset, _)| offset).collect();
    for offset in &offsets {
        assert!(
            method_rest(source, *offset).contains(record),
            "{name}: `{install}` at byte {offset} does not reach `{record}` in its method"
        );
    }
    offsets.len()
}

/// Contract: every production site that installs a GPU resource records it in the ledger: each
/// `surface.configure` is followed at once by `record_surface_request`, each frame-texture,
/// atlas-upload and present-buffer install reaches its record call in the same method, and the
/// renderer's drop releases the whole ledger. A new install site without its hook fails here on
/// every host, where the real-renderer case runs only on Windows CI.
#[test]
fn every_production_install_site_records_in_the_ledger() {
    const CORE: &str = include_str!("core.rs");
    const PRESENT: &str = include_str!("present.rs");
    const REBIND: &str = include_str!("rebind.rs");
    const ATLAS: &str = include_str!("atlas_lifecycle.rs");
    const PIPELINE: &str = include_str!("wezterm_pipeline.rs");
    let configure = "self.surface.configure(&self.device, &self.config);";
    let mut configures = 0;
    for (name, source) in [("core.rs", CORE), ("present.rs", PRESENT), ("rebind.rs", REBIND)] {
        for (offset, _) in source.match_indices(configure) {
            let next = source[offset + configure.len()..].trim_start();
            assert!(
                next.starts_with("self.record_surface_request();"),
                "{name}: a configure at byte {offset} is not followed by record_surface_request"
            );
            configures += 1;
        }
    }
    assert_eq!(configures, 7, "resize, degrade, rebind and four present recovery arms");
    assert!(
        CORE.contains("renderer.record_surface_request();"),
        "construction records the surface"
    );

    let frame = "self.frame_texture = frame_texture;";
    let frame_record = "record_frame_texture_request()";
    assert_eq!(assert_installs_record("core.rs", CORE, frame, frame_record), 2);
    assert_eq!(assert_installs_record("rebind.rs", REBIND, frame, frame_record), 1);
    for (name, source, expected) in [("atlas_lifecycle.rs", ATLAS, 1), ("rebind.rs", REBIND, 1)] {
        let glyph = "self.glyph_upload = ";
        let image = "self.image_upload = ";
        assert_eq!(
            assert_installs_record(name, source, glyph, "record_glyph_atlas_request()"),
            expected
        );
        assert_eq!(
            assert_installs_record(name, source, image, "record_image_atlas_request()"),
            expected
        );
    }
    assert!(
        assert_installs_record(
            "pipeline",
            PIPELINE,
            "self.vertex_buf = ",
            "record_vertex_request()"
        ) >= 2
    );
    assert!(
        assert_installs_record("pipeline", PIPELINE, "self.index_buf = ", "record_index_request()")
            >= 2
    );
    let drop_body = &CORE[CORE.find("impl Drop for GpuRenderer").expect("renderer drop")..];
    assert!(method_rest(drop_body, 0).contains("self.graphics_ledger.release_all();"));
}
