//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{chrome_color_to_linear_rgba, ChromeColor};
use sonicterm_engine::FontStack;

pub(crate) static TRACKED_FONT_STACK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn tracked_font_stack(font_size: f64) -> FontStack {
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![assets],
        font_size,
        72,
        1.0,
    )
    .expect("bundled test font must load")
}

/// A primary face lacking é, a locator that answers every fallback request with Rec Mono, and the
/// temporary directory holding the primary face, removed on drop.
pub(crate) struct FallbackStack {
    pub(crate) stack: sonicterm_engine::FontStack,
    directory: std::path::PathBuf,
}

impl Drop for FallbackStack {
    // Lifecycle: dropping `FallbackStack` removes its temporary font `directory`.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Answers every fallback request with Rec Mono, which has é.
pub(crate) struct RecMonoLocator;

impl sonicterm_font::locator::FontLocator for RecMonoLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(
        &self,
        _: &[char],
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        use sonicterm_font::locator::{FontDataHandle, FontDataSource, FontOrigin};
        let handle = FontDataHandle {
            source: FontDataSource::OnDisk(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
            ),
            index: 0,
            variation: 0,
            origin: FontOrigin::BuiltIn,
            coverage: None,
        };
        Ok(vec![sonicterm_font::parser::ParsedFont::from_locator(&handle)?])
    }
}

pub(crate) fn fallback_stack(name: &str) -> FallbackStack {
    let directory =
        std::env::temp_dir().join(format!("sonicterm-gpu-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c/test.ttf"),
        directory.join("primary.ttf"),
    )
    .unwrap();
    let stack = sonicterm_engine::FontStack::try_new_with_locator_for_test(
        "Roboto",
        vec![directory.clone()],
        std::sync::Arc::new(RecMonoLocator),
        14.0,
        96,
    )
    .unwrap();
    FallbackStack { stack, directory }
}

#[test]
fn exports_color_conversion_helpers() {
    let rgba = chrome_color_to_linear_rgba(ChromeColor::rgb(255, 0, 0));
    assert_eq!(rgba[0], 1.0);
    assert_eq!(rgba[1], 0.0);
    assert_eq!(rgba[2], 0.0);
    assert_eq!(rgba[3], 1.0);
}

/// The legacy alpha-only pipeline retains its source-compatible callable API.
#[test]
fn legacy_text_pipeline_api_remains_callable() {
    use crate::text_pipeline::{GlyphInstance, TextPipeline};
    use wgpu::{BindGroup, Device, Queue, RenderPass, TextureFormat};

    fn draw<'pass>(
        pipeline: &'pass mut TextPipeline,
        device: &Device,
        queue: &Queue,
        pass: &mut RenderPass<'pass>,
        bind_group: &'pass BindGroup,
        instances: &[GlyphInstance],
    ) {
        pipeline.draw(device, queue, pass, bind_group, instances);
    }

    let _: fn(&Device, TextureFormat, u64) -> TextPipeline = TextPipeline::new;
    let _: fn(&TextPipeline) -> u64 = TextPipeline::capacity;
    let _ = draw;
}

#[test]
fn render_cleanup_uses_authored_documentation_policy_without_shadowed_deny() {
    // The authored contract checker owns documentation enforcement without a contradictory crate lint.
    let source = include_str!("lib.rs");
    assert!(source.contains("#![allow(missing_docs)]"));
    assert!(source.contains("#![forbid(unsafe_op_in_unsafe_fn)]"));
    assert!(!source.contains("#![deny(missing_docs)]"));
}

#[test]
fn render_cleanup_preserves_loader_compatibility_signatures() {
    // Explicit attachment remains observable without reintroducing production loader plumbing.
    use crate::core::GpuRenderer;
    let _: fn(&mut GpuRenderer, ()) = GpuRenderer::set_async_loader;
    let _: for<'a> fn(&'a GpuRenderer) -> Option<&'a ()> = GpuRenderer::async_loader;
}

#[test]
fn viewport_compatibility_names_preserve_grid_and_clamping_contract() {
    // Both public names operate on the same Grid identity and clamp already-rebased history positions.
    use crate::core::GpuRenderer;
    use sonicterm_render_model::boundary::grid::grid::Grid;
    let canonical: fn(&Grid, Option<u64>) -> u64 = GpuRenderer::resolved_view_top_abs;
    let legacy: fn(&Grid, Option<u64>) -> u64 = GpuRenderer::resolved_view_top_abs_legacy;
    for retained in [0, 1, 3] {
        let mut grid = Grid::new(8, 2);
        grid.set_scrollback_limit(retained);
        for _ in 0..5 {
            grid.scroll_up(1);
        }
        assert_eq!(grid.scrollback_len(), retained);
        let live = retained as u64;
        for (view, expected) in
            [(None, live), (Some(0), 0), (Some(1), live.min(1)), (Some(u64::MAX), live)]
        {
            assert_eq!(canonical(&grid, view), expected);
            assert_eq!(legacy(&grid, view), expected);
        }
    }
}

#[test]
fn render_cleanup_startup_omits_loader_attachment_and_keeps_device_waker() {
    // Startup retains GPU-stop notifications without claiming a background font loader.
    let source = include_str!("../../sonicterm-app/src/app/event_loop.rs");
    assert_eq!(source.matches("renderer.set_device_state_waker(").count(), 1);
    assert!(source.contains("super::gpu_device_state_waker("));
    assert!(!source.contains("build_async_fallback_loader_for_proxy("));
    assert!(!source.contains(".set_async_loader("));
}

#[test]
fn render_cleanup_child_setup_omits_loader_attachment_and_keeps_device_waker() {
    // Child setup keeps generation-tagged recovery wakes without attaching the compatibility slot.
    let source = include_str!("../../sonicterm-app/src/app/tear_out.rs");
    assert_eq!(source.matches("renderer.set_device_state_waker(").count(), 1);
    assert!(source.contains("super::gpu_recovery::generation_waker("));
    assert!(!source.contains("build_async_fallback_loader_for_proxy("));
    assert!(!source.contains(".set_async_loader("));
}

#[test]
fn render_cleanup_main_frame_omits_inactive_cursor_sink() {
    // Main frames do not allocate an empty vector for a compatibility-only cursor setter.
    let source = include_str!("../../sonicterm-app/src/app/window_event.rs");
    // Main renders through exactly one releasing call and never through the compatibility wrapper.
    assert_eq!(source.matches("r.render_releasing(").count(), 1);
    assert!(!source.contains("render_with_outcome("));
    assert!(!source.contains(".set_inactive_pane_cursors("));
}

#[test]
fn software_frame_composes_without_a_native_presenter() {
    // Every host composes real layers and checks pixels without a native window or GDI.
    use crate::{quad::QuadInstance, software_frame::SoftwareFrame};
    use sonicterm_text::glyph_atlas::GlyphAtlas;

    let atlas = GlyphAtlas::new(1, 1);
    let mut frame = SoftwareFrame::new(2, 1, [1.0, 0.0, 0.0, 1.0]).unwrap();
    let quad = QuadInstance {
        rect: crate::quad::px_to_ndc(0.0, 0.0, 1.0, 1.0, 2.0, 1.0),
        color: [0.0, 1.0, 0.0, 1.0],
        ..Default::default()
    };
    frame.draw_layers(&atlas, &atlas, &[quad], &[], &[], &[], &[]);
    assert_eq!(frame.pixel_bgra_at(0, 0), Some([0, 255, 0, 255]));
    assert_eq!(frame.pixel_bgra_at(1, 0), Some([0, 0, 255, 255]));
    assert_eq!(frame.pixel_bgra_at(2, 0), None);
    assert_eq!(frame.pixel_bgra_at(0, 1), None);
}
