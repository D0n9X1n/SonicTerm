#![cfg(target_os = "windows")]

use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_render_model::boundary::cfg::{
    config::{ScrollbarMode, SoftwareRenderMode},
    theme::Theme,
};
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        self.outcome = Some(run_probe(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

/// Settings for a hidden probe renderer; the probe reads device identity, never pixels.
fn probe_settings() -> RendererSettings<'static> {
    RendererSettings {
        font_family: "monospace",
        font_dirs: &[],
        font_size: 14.0,
        line_height_mult: 1.2,
        font_weight_scale: 1.0,
        subpixel_aa: Default::default(),
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: 1.0,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Force,
        },
        role: "shared-device-identity-test",
    }
}

/// Each renderer needs its own native window, because a surface belongs to one window.
fn probe_window(active: &ActiveEventLoop, title: &str) -> Result<Arc<Window>, String> {
    active
        .create_window(
            Window::default_attributes()
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(320, 180))
                .with_title(title),
        )
        .map(Arc::new)
        .map_err(|error| error.to_string())
}

fn run_probe(active: &ActiveEventLoop) -> Result<(), String> {
    let theme = Theme::default();
    let first = GpuRenderer::new(
        probe_window(active, "SonicTerm device identity: first")?,
        active,
        &theme,
        probe_settings(),
    )
    .map_err(|error| format!("first renderer: {error}"))?;
    let unshared = GpuRenderer::new(
        probe_window(active, "SonicTerm device identity: unshared")?,
        active,
        &theme,
        probe_settings(),
    )
    .map_err(|error| format!("unshared renderer: {error}"))?;
    let sibling = GpuRenderer::new_with_shared_context(
        probe_window(active, "SonicTerm device identity: sibling")?,
        active,
        &theme,
        probe_settings(),
        first.shared_context(),
    )
    .map_err(|error| format!("sibling renderer: {error}"))?;

    let mut failures = Vec::new();
    // Each unshared renderer opens its own instance, and wgpu can give both devices the same
    // id, so this pair is the control that a device-only comparison would get wrong.
    if first.shares_device_with(&unshared) || unshared.shares_device_with(&first) {
        failures.push("two GpuRenderer::new renderers reported one device");
    }
    if !first.shares_device_with(&sibling) || !sibling.shares_device_with(&first) {
        failures.push("a new_with_shared_context renderer did not report its source's device");
    }
    if sibling.shares_device_with(&unshared) {
        failures.push("a shared-context renderer reported an unrelated renderer's device");
    }
    if !first.shares_device_with(&first) {
        failures.push("a renderer did not report its own device");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Renderers built separately never compare as one device; a shared-context sibling always does.
///
/// Protects the identity check that the New Window device-sharing tests rely on. Both sides are
/// real renderers on real windows, so WARP or a hardware adapter both exercise it.
#[test]
fn shares_device_with_separates_unshared_renderers_and_joins_shared_siblings() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("device identity event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
