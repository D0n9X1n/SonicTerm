#![cfg(target_os = "windows")]
//! The idle image-atlas release and the GDI frame texture on a real renderer, on the hosted runner's
//! WARP device: the interval release without a frame, a still image that is never released, the re-shown
//! frame's pixels, the frame texture's size under each presenter, and both on a stopped device and
//! after recovery onto a new device.

use sonicterm_gpu::{
    core::{GpuRenderer, RendererSettings, SurfaceAppearance},
    device_errors::{DeviceStateWaker, GpuFaultKind},
};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{ScrollbarMode, SoftwareRenderMode},
            theme::Theme,
        },
        grid::grid::Grid,
        ui::tabs::TabBar,
    },
    CursorStyle, InlineImage, PaneRender, PixelRect,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

/// Past the 30 s interval, so a due release runs when serviced.
const AFTER_INTERVAL: Duration = Duration::from_secs(31);

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so every case runs inside this one.
        self.outcome = Some(run_cases(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// A visible window and a renderer in `mode`: `Off` presents through wgpu, `Force` through GDI.
fn renderer(
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
    role: &'static str,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(320, 180))
                    .with_title("SonicTerm idle image atlas"),
            )
            .map_err(|error| error.to_string())?,
    );
    let mut renderer = GpuRenderer::new(
        window.clone(),
        active,
        &Theme::default(),
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
                software_render_mode: mode,
            },
            role,
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    Ok((window, renderer))
}

/// Rebind `renderer` onto a newly requested device, in production's order: request, run, prepare, commit.
///
/// The request runs on this thread rather than a worker; it only blocks, which a test may do. The new
/// objects are built from `mode` and the renderer's current atlases, as production recovery builds them.
fn recover(
    renderer: &mut GpuRenderer,
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
) -> Result<(), String> {
    let request = renderer.recovery_request(active).map_err(|error| error.to_string())?;
    let recovered = request
        .run(|_generation| -> DeviceStateWaker { Arc::new(|| {}) })
        .map_err(|failure| failure.error().to_string())?;
    let (context, surface) = recovered.into_parts();
    let prepared = renderer
        .prepare_rebind(&context, Some(surface), mode)
        .map_err(|error| error.to_string())?;
    renderer.commit_rebind(prepared).map_err(|error| error.to_string())?;
    check(renderer.device_accepts_gpu_work(), "the recovered device accepts work")
}

/// Assemble and present one frame of a single pane, with a 32x32 image at its origin when `image`.
fn render(renderer: &mut GpuRenderer, image: bool) -> Result<(), String> {
    let mut grid = Grid::new(10, 4);
    let mut panes = [PaneRender {
        id: 1,
        rect_px: PixelRect { x: 0, y: 0, w: 200, h: 120 },
        grid: &mut grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: if image {
            vec![InlineImage {
                id: 1,
                row: 0,
                col: 0,
                width: 32,
                height: 32,
                bgra: Arc::from([0, 0, 255, 255].repeat(32 * 32)),
            }]
        } else {
            Vec::new()
        },
    }];
    renderer
        .render(
            &mut panes,
            &Theme::default(),
            false,
            None,
            None,
            &TabBar::new(),
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

fn software_pixels(renderer: &GpuRenderer, window: &Window) -> Result<Vec<[u8; 4]>, String> {
    let size = window.inner_size();
    (0..size.height)
        .flat_map(|pixel_y| (0..size.width).map(move |pixel_x| (pixel_x, pixel_y)))
        .map(|(pixel_x, pixel_y)| {
            renderer
                .__test_software_frame_pixel_bgra(pixel_x, pixel_y)
                .ok_or_else(|| String::from("software frame pixel unavailable"))
        })
        .collect()
}

/// An interval release without a frame shrinks the CPU atlas and its GPU mirror to 1x1, and the
/// renderer's own reading drops at once; the base build left both at 2048x2048.
fn timer_release(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) = renderer(active, SoftwareRenderMode::Off, "idle-atlas-timer")?;
    render(&mut renderer, true)?;
    let (cpu, gpu) = renderer.__test_image_atlas_dimensions();
    check(cpu != (1, 1) && gpu == cpu, "an image frame promotes the atlas and its GPU mirror")?;
    render(&mut renderer, false)?;
    let frames = renderer.successful_frame_count();
    check(renderer.release_idle_image_atlas(Instant::now() + AFTER_INTERVAL), "the release ran")?;
    check(
        renderer.__test_image_atlas_dimensions() == ((1, 1), (1, 1)),
        "the CPU atlas and its GPU mirror are 1x1 after the release",
    )?;
    check(renderer.retained_amounts().image_atlas.bytes == 4, "the reading drops to 4 B at once")?;
    check(renderer.successful_frame_count() == frames, "the release presents no frame")?;
    check(renderer.image_atlas_release_deadline().is_none(), "a released atlas arms no deadline")
}

/// An image still on screen has no absence instant, so even long after promotion it is never released.
fn still_image(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) = renderer(active, SoftwareRenderMode::Off, "idle-atlas-still")?;
    render(&mut renderer, true)?;
    check(renderer.image_atlas_release_deadline().is_none(), "visible media arms no deadline")?;
    check(!renderer.release_idle_image_atlas(Instant::now() + AFTER_INTERVAL), "nothing released")?;
    check(renderer.__test_image_atlas_dimensions().0 != (1, 1), "the atlas stays promoted")
}

/// After an interval release, showing the image again promotes the atlas and draws exactly the pixels
/// of a frame from a renderer that never released.
fn reshow_pixels(active: &ActiveEventLoop) -> Result<(), String> {
    let (window, mut never) = renderer(active, SoftwareRenderMode::Force, "idle-atlas-never")?;
    render(&mut never, true)?;
    let expected = software_pixels(&never, &window)?;
    let (window, mut released) =
        renderer(active, SoftwareRenderMode::Force, "idle-atlas-released")?;
    render(&mut released, true)?;
    render(&mut released, false)?;
    check(released.release_idle_image_atlas(Instant::now() + AFTER_INTERVAL), "the release ran")?;
    render(&mut released, true)?;
    check(released.__test_image_atlas_dimensions().0 != (1, 1), "the reshow promotes again")?;
    check(software_pixels(&released, &window)? == expected, "the re-shown frame matches")
}

/// Under the software presenter the frame texture is 1x1 at construction and after a resize; leaving it
/// grows the texture to the surface and presents a full frame, re-entering shrinks it again, and a
/// recovery rebind while degraded builds it at 1x1 too.
fn frame_texture(active: &ActiveEventLoop) -> Result<(), String> {
    let (window, mut renderer) = renderer(active, SoftwareRenderMode::Force, "frame-texture")?;
    check(renderer.frame_texture_extent() == (1, 1), "1x1 at construction under GDI")?;
    check(renderer.try_resize(400, 240), "the resize is accepted")?;
    check(renderer.frame_texture_extent() == (1, 1), "1x1 after a resize under GDI")?;
    render(&mut renderer, false)?;
    let frames = renderer.successful_frame_count();
    renderer.set_software_render_degrade(false);
    let size = window.inner_size();
    check(renderer.frame_texture_extent() != (1, 1), "leaving GDI grows the texture")?;
    check(
        renderer.frame_texture_extent() == (400, 240)
            || renderer.frame_texture_extent() == (size.width, size.height),
        "the texture covers the configured surface",
    )?;
    render(&mut renderer, false)?;
    check(renderer.successful_frame_count() == frames + 1, "a full frame follows the switch")?;
    renderer.set_software_render_degrade(true);
    check(renderer.frame_texture_extent() == (1, 1), "re-entering GDI shrinks it to 1x1")?;
    recover(&mut renderer, active, SoftwareRenderMode::Force)?;
    check(renderer.frame_texture_extent() == (1, 1), "a recovery rebind while degraded builds 1x1")
}

/// On a stopped device the release still frees the CPU atlas, admits no GPU work and keeps the old
/// mirror, and recovery then builds a 1x1 image upload. The degrade switch leaves the frame texture alone
/// while stopped; recovery builds it at 1x1 when degraded and at the surface size when not.
fn stopped_device(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut stopped) = renderer(active, SoftwareRenderMode::Off, "idle-atlas-stopped")?;
    render(&mut stopped, true)?;
    let promoted_mirror = stopped.__test_image_atlas_dimensions().1;
    render(&mut stopped, false)?;
    stopped.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    check(!stopped.device_accepts_gpu_work(), "the device is stopped")?;
    check(stopped.release_idle_image_atlas(Instant::now() + AFTER_INTERVAL), "the release ran")?;
    let (cpu, gpu) = stopped.__test_image_atlas_dimensions();
    check(cpu == (1, 1), "the CPU atlas is released while stopped")?;
    check(gpu == promoted_mirror, "the stopped device keeps its image upload")?;
    recover(&mut stopped, active, SoftwareRenderMode::Off)?;
    check(
        stopped.__test_image_atlas_dimensions() == ((1, 1), (1, 1)),
        "recovery builds a 1x1 image upload for the released atlas",
    )?;

    // Degraded, stopped, switched off: nothing is rebuilt until recovery, which builds the full size.
    let (window, mut degraded) =
        renderer(active, SoftwareRenderMode::Force, "frame-texture-stopped")?;
    degraded.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    degraded.set_software_render_degrade(false);
    check(
        degraded.frame_texture_extent() == (1, 1),
        "a stopped device does not rebuild the texture",
    )?;
    recover(&mut degraded, active, SoftwareRenderMode::Off)?;
    let size = window.inner_size();
    check(
        degraded.frame_texture_extent() == (size.width, size.height),
        "recovery without degrade builds the surface size",
    )?;

    // Not degraded, stopped, switched on: the full texture stays until recovery builds it at 1x1.
    let (_window, mut hardware) =
        renderer(active, SoftwareRenderMode::Off, "frame-texture-stopped-off")?;
    let full = hardware.frame_texture_extent();
    check(full != (1, 1), "the GPU presenter starts at the surface size")?;
    hardware.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    hardware.set_software_render_degrade(true);
    check(hardware.frame_texture_extent() == full, "a stopped device keeps the full texture")?;
    recover(&mut hardware, active, SoftwareRenderMode::Force)?;
    check(hardware.frame_texture_extent() == (1, 1), "recovery under degrade builds 1x1")
}

#[test]
fn windows_idle_image_atlas_and_gdi_frame_texture() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("idle atlas event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let mut failures = Vec::new();
    for (name, case) in [
        ("timer release", timer_release as fn(&ActiveEventLoop) -> Result<(), String>),
        ("still image", still_image),
        ("reshow pixels", reshow_pixels),
        ("frame texture", frame_texture),
        ("stopped device", stopped_device),
    ] {
        if let Err(error) = case(active) {
            failures.push(format!("{name}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
