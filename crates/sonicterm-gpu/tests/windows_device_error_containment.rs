//! Device-error containment through real Windows renderers: each scenario
//! builds its own renderer with `GpuRenderer::new`, so each gets its own
//! device, and drives the forced CPU presenter on WARP or a hardware adapter.

#![cfg(target_os = "windows")]

use std::sync::Arc;

use sonicterm_gpu::core::{GpuRenderer, PresentOutcome, RendererSettings, SurfaceAppearance};
use sonicterm_gpu::device_errors::{DeviceState, GpuFaultKind};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{ScrollbarMode, SoftwareRenderMode},
            theme::Theme,
        },
        grid::grid::Grid,
        ui::tabs::TabBar,
    },
    CursorStyle, PaneRender, PixelRect,
};
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
        self.outcome = Some(run_scenarios(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn settings(role: &'static str) -> RendererSettings<'static> {
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
        role,
    }
}

/// A visible window and a forced-CPU renderer on a device of its own.
fn renderer(active: &ActiveEventLoop, role: &'static str) -> Result<GpuRenderer, String> {
    let window = active
        .create_window(
            Window::default_attributes()
                .with_visible(true)
                .with_inner_size(PhysicalSize::new(160, 96))
                .with_title(role),
        )
        .map(Arc::new)
        .map_err(|error| format!("{role}: window: {error}"))?;
    let mut renderer = GpuRenderer::new(window, active, &Theme::default(), settings(role))
        .map_err(|error| format!("{role}: renderer: {error}"))?;
    if !renderer.is_software_render_degraded() {
        return Err(format!("{role}: forced software-render mode did not engage"));
    }
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    Ok(renderer)
}

fn render(renderer: &mut GpuRenderer, grid: &mut Grid) -> Result<(), String> {
    let mut panes = [PaneRender {
        id: 1,
        rect_px: PixelRect { x: 0, y: 0, w: 160, h: 96 },
        grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    let tabs = TabBar::new();
    renderer
        .render(
            &mut panes,
            &Theme::default(),
            false,
            None,
            None,
            &tabs,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map_err(|error| error.to_string())
}

/// Exercise the additive typed entry point with the same pane fixture as `render`.
fn render_outcome(renderer: &mut GpuRenderer, grid: &mut Grid) -> PresentOutcome {
    let mut panes = [PaneRender {
        id: 1,
        rect_px: PixelRect { x: 0, y: 0, w: 160, h: 96 },
        grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    let tabs = TabBar::new();
    renderer.render_with_outcome(
        &mut panes,
        &Theme::default(),
        false,
        None,
        None,
        &tabs,
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Presentations handed to the presenter, and frames that were acknowledged.
fn counts(renderer: &GpuRenderer) -> (u64, u64) {
    (renderer.present_call_count(), renderer.successful_frame_count())
}

fn check(condition: bool, what: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(what.to_owned())
    }
}

/// After an isolated fault the device stays usable and the next frame presents.
fn isolated_fault(active: &ActiveEventLoop) -> Result<u64, String> {
    let mut renderer = renderer(active, "containment: isolated")?;
    let mut grid = Grid::new(8, 4);
    render(&mut renderer, &mut grid)?;
    let (presents, frames) = counts(&renderer);
    renderer.__inject_gpu_fault(GpuFaultKind::IsolatedOperation);
    let snapshot = renderer.device_error_snapshot();
    check(snapshot.state == DeviceState::Usable, "the isolated fault stopped the device")?;
    check(snapshot.counts.isolated == 1, "the isolated fault was not recorded")?;
    render(&mut renderer, &mut grid)?;
    check(counts(&renderer) == (presents + 1, frames + 1), "the next frame did not present")?;
    Ok(renderer.device_generation())
}

/// A retained-resource fault stops the device: the next frame reports it once
/// and keeps its dirty rows, later frames issue no GPU work, and one record is
/// logged.
fn retained_resource_fault(active: &ActiveEventLoop) -> Result<u64, String> {
    let mut renderer = renderer(active, "containment: retained")?;
    let mut grid = Grid::new(8, 4);
    render(&mut renderer, &mut grid)?;
    let before = counts(&renderer);
    renderer.__inject_gpu_fault(GpuFaultKind::RetainedResourceCreation);
    renderer.force_rebuild_for_scale(renderer.scale_factor());
    let stopped = renderer.device_error_snapshot();
    check(stopped.state == DeviceState::Unusable, "the invalid upload did not stop the device")?;
    grid.mark_all_dirty();
    check(render(&mut renderer, &mut grid).is_err(), "the first stopped frame returned Ok")?;
    check(grid.dirty_rows().next().is_some(), "a stopped frame acknowledged dirty rows")?;
    check(render(&mut renderer, &mut grid).is_ok(), "a later stopped frame returned Err")?;
    let after = renderer.device_error_snapshot();
    check(counts(&renderer) == before, "a stopped frame presented")?;
    check(after.admitted_work == stopped.admitted_work, "GPU work ran after the stop")?;
    check(after.records_logged == 1, "the stop logged more than one record")?;
    Ok(renderer.device_generation())
}

/// A frame-validation fault fails the frame's own submission: that frame is
/// neither presented nor acknowledged, and later frames return Ok without
/// presenting or logging.
fn frame_validation_fault(active: &ActiveEventLoop) -> Result<u64, String> {
    let mut renderer = renderer(active, "containment: frame validation")?;
    let mut grid = Grid::new(8, 4);
    render(&mut renderer, &mut grid)?;
    let before = counts(&renderer);
    // The baseline acknowledged its dirt. Seed new dirt so a forbidden acknowledgement is observable.
    grid.mark_all_dirty();
    let dirty_rows: Vec<_> = grid.dirty_rows().collect();
    check(!dirty_rows.is_empty(), "the frame fault fixture has no dirty rows")?;
    renderer.__inject_gpu_fault(GpuFaultKind::FrameValidation);
    check(render(&mut renderer, &mut grid).is_err(), "the invalid frame returned Ok")?;
    check(
        grid.dirty_rows().collect::<Vec<_>>() == dirty_rows,
        "the invalid frame acknowledged dirt",
    )?;
    check(counts(&renderer) == before, "the invalid frame reached the presenter")?;
    let stopped = renderer.device_error_snapshot();
    check(stopped.state == DeviceState::Unusable, "the invalid frame did not stop the device")?;
    for _ in 0..3 {
        check(render(&mut renderer, &mut grid).is_ok(), "a later stopped frame returned Err")?;
        check(
            grid.dirty_rows().collect::<Vec<_>>() == dirty_rows,
            "a later stopped frame acknowledged dirt",
        )?;
    }
    check(counts(&renderer) == before, "a later stopped frame presented")?;
    let after = renderer.device_error_snapshot();
    check(after.records_logged == stopped.records_logged, "a later frame logged again")?;
    Ok(renderer.device_generation())
}

/// After an intentional destroy the setters and `render` issue no GPU work:
/// admitted work and presentations stay flat while refused work rises.
fn destroyed_device(active: &ActiveEventLoop) -> Result<u64, String> {
    let mut renderer = renderer(active, "containment: destroy")?;
    let mut grid = Grid::new(8, 4);
    render(&mut renderer, &mut grid)?;
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    let lost = renderer.device_error_snapshot();
    check(lost.state == DeviceState::Lost && lost.lost.is_some(), "destroy recorded no loss")?;
    let presents = renderer.present_call_count();
    check(renderer.try_resize(200, 120), "a stopped device refused a valid size")?;
    renderer.set_scale_factor(renderer.scale_factor() * 2.0);
    renderer.set_software_render_degrade(false);
    renderer.set_software_render_degrade(true);
    let _ = render(&mut renderer, &mut grid);
    check(render(&mut renderer, &mut grid).is_ok(), "a later stopped frame returned Err")?;
    // Device loss is never a surface retry or a present in the additive typed API either.
    let typed = render_outcome(&mut renderer, &mut grid);
    check(
        matches!(&typed, PresentOutcome::RenderingUnavailable(context) if context.gate.state == DeviceState::Lost && context.gate.destroy_requested && !context.reports_stop && context.generation == renderer.device_generation()),
        "the destroyed device was misclassified by the typed entry point",
    )?;
    check(typed.into_render_result().is_ok(), "a later typed lost frame returned Err")?;
    let after = renderer.device_error_snapshot();
    check(after.admitted_work == lost.admitted_work, "GPU work ran after the destroy")?;
    check(after.refused_work > lost.refused_work, "the setters were not refused")?;
    check(renderer.present_call_count() == presents, "a frame presented after the destroy")?;
    Ok(renderer.device_generation())
}

/// A cached reblit of an unchanged frame passes the device gate: a stop between
/// `render`'s entry check and the cached present hides the reblit, reports the
/// stop once, and acknowledges nothing.
fn cached_reblit_stop(active: &ActiveEventLoop) -> Result<u64, String> {
    let mut renderer = renderer(active, "containment: cached reblit")?;
    let mut grid = Grid::new(8, 4);
    render(&mut renderer, &mut grid)?;
    let (presents, frames) = counts(&renderer);
    render(&mut renderer, &mut grid)?;
    check(counts(&renderer) == (presents + 1, frames), "the unchanged frame was not reblitted")?;
    renderer.__stop_device_before_cached_present();
    check(render(&mut renderer, &mut grid).is_err(), "the stopped reblit returned Ok")?;
    check(counts(&renderer) == (presents + 1, frames), "the stopped reblit reached the presenter")?;
    let stopped = renderer.device_error_snapshot();
    check(stopped.state == DeviceState::Unusable, "the injected stop was not recorded")?;
    check(stopped.records_logged == 1, "the injected stop logged more than one record")?;
    check(render(&mut renderer, &mut grid).is_ok(), "a later stopped frame returned Err")?;
    check(counts(&renderer) == (presents + 1, frames), "a later stopped frame presented")?;
    Ok(renderer.device_generation())
}

/// The typed entry point reports a stopped cached reblit with the exact device
/// identity, maps its first stop to Err, and preserves later silent results.
fn typed_cached_reblit_stop(active: &ActiveEventLoop) -> Result<u64, String> {
    let mut renderer = renderer(active, "containment: typed cached reblit")?;
    let mut grid = Grid::new(8, 4);
    check(
        matches!(render_outcome(&mut renderer, &mut grid), PresentOutcome::Presented),
        "the first typed frame did not present",
    )?;
    check(
        matches!(render_outcome(&mut renderer, &mut grid), PresentOutcome::CachedReblit),
        "the typed unchanged frame was not a cached reblit",
    )?;
    let before = counts(&renderer);
    renderer.__stop_device_before_cached_present();
    let first = render_outcome(&mut renderer, &mut grid);
    match &first {
        PresentOutcome::RenderingUnavailable(context) => {
            check(
                context.generation == renderer.device_generation(),
                "the suspended generation changed",
            )?;
            check(
                context.gate.state == DeviceState::Unusable && context.reports_stop,
                "the cached stop lost its first report or gate",
            )?;
        }
        other => return Err(format!("a stopped cached reblit was misclassified: {other:?}")),
    }
    check(first.into_render_result().is_err(), "the typed first stop mapped to Ok")?;
    let stopped = renderer.device_error_snapshot();
    // The source no longer enters the unchanged path; new dirt still must not be acknowledged.
    grid.mark_all_dirty();
    let later = render_outcome(&mut renderer, &mut grid);
    check(
        matches!(&later, PresentOutcome::RenderingUnavailable(context) if !context.reports_stop && context.generation == renderer.device_generation()),
        "the later typed stop lost its silent outcome",
    )?;
    check(later.into_render_result().is_ok(), "the later typed stop mapped to Err")?;
    check(grid.dirty_rows().next().is_some(), "the typed stop acknowledged dirty rows")?;
    check(counts(&renderer) == before, "the typed stopped reblit reached the presenter")?;
    let after = renderer.device_error_snapshot();
    check(after.admitted_work == stopped.admitted_work, "a typed stopped frame admitted GPU work")?;
    check(after.records_logged == stopped.records_logged, "a typed stopped frame logged again")?;
    Ok(renderer.device_generation())
}

fn run_scenarios(active: &ActiveEventLoop) -> Result<(), String> {
    type Scenario = fn(&ActiveEventLoop) -> Result<u64, String>;
    let scenarios: [(&str, Scenario); 6] = [
        ("isolated fault", isolated_fault),
        ("retained-resource fault", retained_resource_fault),
        ("frame-validation fault", frame_validation_fault),
        ("destroyed device", destroyed_device),
        ("cached reblit stop", cached_reblit_stop),
        ("typed cached reblit stop", typed_cached_reblit_stop),
    ];
    let mut generations = Vec::new();
    let mut failures = Vec::new();
    for (name, scenario) in scenarios {
        match scenario(active) {
            Ok(generation) => generations.push(generation),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    generations.sort_unstable();
    generations.dedup();
    if failures.is_empty() && generations.len() != scenarios.len() {
        failures.push("two scenarios shared a device".to_owned());
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Containment through real renderers on their own devices: an isolated fault
/// keeps presenting; retained-resource, frame-validation, and cached-reblit
/// stops present nothing more and log once; a destroyed device does no GPU work.
/// The cached-stop scenario also checks the typed payload and its compatibility mapping.
///
/// winit allows one event loop per process, so every scenario runs inside one
/// test.
#[test]
fn renderers_contain_device_errors_on_their_own_devices() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("containment event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
