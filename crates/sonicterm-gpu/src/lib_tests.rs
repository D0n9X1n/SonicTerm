//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{chrome_color_to_linear_rgba, ChromeColor};
use sonicterm_engine::FontStack;

pub(crate) static TRACKED_FONT_STACK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One headless device and queue for the whole test process, created once on the host's default
/// adapter (WARP on Windows) with `features` the adapter offers. Tests share it instead of each
/// creating its own: a Linux software adapter (lavapipe) can crash when many devices are created
/// and destroyed at once across test threads. `Device` and `Queue` are cheap handles, and wgpu
/// keeps error scopes per thread, so tests sharing the device never see each other's scopes.
fn shared_device(
    slot: &'static std::sync::OnceLock<(wgpu::Device, wgpu::Queue)>,
    features: impl FnOnce(&wgpu::Adapter) -> wgpu::Features,
) -> (wgpu::Device, wgpu::Queue) {
    slot.get_or_init(|| {
        #[cfg(target_os = "windows")]
        let (descriptor, force_fallback_adapter) = (
            wgpu::InstanceDescriptor {
                backends: wgpu::Backends::DX12,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            },
            true,
        );
        #[cfg(not(target_os = "windows"))]
        let (descriptor, force_fallback_adapter) =
            (wgpu::InstanceDescriptor::new_without_display_handle(), false);
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter,
            apply_limit_buckets: false,
        }))
        .expect("headless test adapter");
        let software = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
        let features = features(&adapter);
        pollster::block_on(
            adapter.request_device(&crate::core::device_descriptor_for(software, features)),
        )
        .expect("headless test device")
    })
    .clone()
}

/// The process's shared headless device with no optional features.
pub(crate) fn shared_headless_device() -> (wgpu::Device, wgpu::Queue) {
    static PLAIN: std::sync::OnceLock<(wgpu::Device, wgpu::Queue)> = std::sync::OnceLock::new();
    shared_device(&PLAIN, |_| wgpu::Features::empty())
}

/// The process's shared headless device with the optional features production selects, such as
/// dual-source blending where the adapter offers it. Off Windows production selects none, so this
/// is the plain device itself: one device, created once, so no two test threads ever create one.
pub(crate) fn shared_featured_device() -> (wgpu::Device, wgpu::Queue) {
    #[cfg(target_os = "windows")]
    {
        static FEATURED: std::sync::OnceLock<(wgpu::Device, wgpu::Queue)> =
            std::sync::OnceLock::new();
        shared_device(&FEATURED, |adapter| {
            crate::core::selected_optional_device_features(adapter.features(), true)
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        shared_headless_device()
    }
}

pub(crate) fn tracked_font_stack(font_size: f64) -> FontStack {
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sonicterm-font/test-fonts/rec-mono");
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
                    .join("../sonicterm-font/test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
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
    fallback_stack_with_locator(name, std::sync::Arc::new(RecMonoLocator))
}

/// [`fallback_stack`] with `locator` answering fallback requests, so a test can delay or
/// replace the face discovery publishes.
pub(crate) fn fallback_stack_with_locator(
    name: &str,
    locator: std::sync::Arc<dyn sonicterm_font::locator::FontLocator + Send + Sync>,
) -> FallbackStack {
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
        locator,
        14.0,
        96,
    )
    .unwrap();
    FallbackStack { stack, directory }
}

/// Off Windows, every device a lib test uses comes from `shared_device`, so no two test threads
/// create or destroy a software device at once. The scan reads code only (comments and literals
/// blanked), finds each call by its tokens, and takes its enclosing function as the innermost one
/// whose body span holds it. The call is allowed only inside this file's own `shared_device`, or
/// when that function's own live attributes include exactly `#[cfg(target_os = "windows")]`.
#[test]
fn only_the_shared_helper_creates_a_device_off_windows() {
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut unshared = Vec::new();
    for entry in std::fs::read_dir(&source).expect("source directory") {
        let path = entry.expect("source entry").path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_owned();
        if !(name.ends_with("_tests.rs") || name.contains("fixture")) {
            // When: the file is not test code, production's own device creation is not this rule's.
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("test source");
        let calls = unshared_device_calls(&text, name == "lib_tests.rs");
        unshared.extend(calls.into_iter().map(|line| format!("{name}:{line}")));
    }
    assert!(unshared.is_empty(), "test devices outside the shared helper: {unshared:?}");
}

/// The 1-based lines of `.request_device(` calls in `text` that run off Windows outside the shared
/// initializer, which only the file holding it (`holds_shared`) may contain.
fn unshared_device_calls(text: &str, holds_shared: bool) -> Vec<usize> {
    use crate::source_scan::{block_span, code_only, line_number, token_matches};
    let code = code_only(text);
    // Every function in masked code: its header offset, its name and its body span.
    let functions: Vec<(usize, &str, std::ops::Range<usize>)> = token_matches(&code, "fn")
        .into_iter()
        .filter_map(|header| {
            let after = code[header + 2..].trim_start();
            let name_end = after.find(|char: char| !char.is_alphanumeric() && char != '_')?;
            let body = block_span(&code, header)?;
            Some((header, &after[..name_end], body))
        })
        .collect();
    let mut lines = Vec::new();
    for at in token_matches(&code, ".request_device(") {
        // The innermost function whose body holds the call.
        let enclosing = functions
            .iter()
            .filter(|(_, _, body)| body.contains(&at))
            .max_by_key(|(header, _, _)| *header);
        let allowed = enclosing.is_some_and(|(header, name, _)| {
            (holds_shared && *name == "shared_device") || windows_only(text, &code, *header)
        });
        if !allowed {
            // When: the call's own function is neither the shared initializer nor Windows-only.
            lines.push(line_number(text, at));
        }
    }
    lines
}

/// Whether the function at `header` carries `#[cfg(target_os = "windows")]` among its own live
/// attributes: the lines between the previous item's end and its header. `code_only` blanks the
/// cfg's string, so the value is read from `text`, but only on a line `code` shows is live code.
fn windows_only(text: &str, code: &str, header: usize) -> bool {
    let item_start = code[..header].rfind(['}', ';', '{']).map_or(0, |end| end + 1);
    let mut offset = item_start;
    for raw in text[item_start..header].split_inclusive('\n') {
        let masked = &code[offset..offset + raw.len()];
        offset += raw.len();
        // A live attribute keeps `#[cfg(` in masked code; a commented one is blanked to spaces.
        let live = masked.trim_start().starts_with("#[cfg(");
        if live && raw.trim() == "#[cfg(target_os = \"windows\")]" {
            // When: this line is a live, exactly Windows-only cfg, the function is Windows-only.
            return true;
        }
    }
    false
}

/// The scan reports a Linux-reachable call, a negated cfg, a cfg borrowed from an earlier item, a
/// call after a closed nested Windows-only helper, a commented-out cfg, and a `shared_device`
/// outside its file; it accepts a Windows-only function, the shared helper in its own file, and
/// the call's text inside a string or a comment.
#[test]
fn the_device_scan_reads_each_calls_own_cfg() {
    let scan = |source: &str| unshared_device_calls(source, false);
    let linux = "fn helper() {\n    adapter.request_device(&descriptor);\n}\n";
    assert_eq!(scan(linux), vec![2]);
    let negated = "#[cfg(not(target_os = \"windows\"))]\nfn helper() {\n    adapter\n        \
                   .request_device (&descriptor);\n}\n";
    assert_eq!(scan(negated).len(), 1, "a negated cfg runs on Linux");
    let borrowed = "#[cfg(target_os = \"windows\")]\nfn warp() {}\nfn linux() {\n    \
                    adapter.request_device(&descriptor);\n}\n";
    assert_eq!(scan(borrowed).len(), 1, "an earlier item's cfg is not its own");
    let nested = "fn outer() {\n    #[cfg(target_os = \"windows\")]\n    fn warp() {}\n    \
                  adapter.request_device(&descriptor);\n}\n";
    assert_eq!(scan(nested).len(), 1, "a closed nested helper's cfg is not the outer call's");
    let commented = "/*\n#[cfg(target_os = \"windows\")]\n*/\nfn linux() {\n    \
                     adapter.request_device(&descriptor);\n}\n";
    assert_eq!(scan(commented).len(), 1, "a commented-out cfg is not live");
    let windows = "#[cfg(target_os = \"windows\")]\nfn warp() {\n    \
                   adapter.request_device(&descriptor);\n}\n";
    assert!(scan(windows).is_empty());
    let shared = "fn shared_device() {\n    adapter.request_device(&descriptor);\n}\n";
    assert!(unshared_device_calls(shared, true).is_empty(), "the initializer in its own file");
    assert_eq!(scan(shared).len(), 1, "a `shared_device` in another file is not the initializer");
    let quoted = "fn scan() {\n    let text = \".request_device(\"; // .request_device(\n}\n";
    assert!(scan(quoted).is_empty());
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
