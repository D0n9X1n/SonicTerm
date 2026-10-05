//! The idle image-atlas release and the GDI frame texture on a real renderer, on the hosted runner's
//! WARP device: the interval release without a frame, a still image that is never released, the re-shown
//! frame's pixels, the frame texture's size under each presenter, and both on a stopped device and
//! after recovery onto a new device. A covered-window trim's frame texture survives a stop and a
//! recovery commit, and its image-atlas release restores the media on the next frame, on GDI and
//! read back from wgpu. A trim the stopped device refuses changes nothing.
//!
//! Only the event-loop entry point is Windows-only (winit allows a test-thread event loop there);
//! the case logic compiles on every host, so a non-Windows lint pass type-checks it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use sonicterm_gpu::{
    core::{unpad_readback_rows, GpuRenderer, RendererSettings, SurfaceAppearance},
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
    event_loop::ActiveEventLoop,
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
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
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
    recover_checked(renderer, active, mode, |_| Ok(()))
}

/// [`recover`], running `between` after the rebind is prepared and before it is committed, so a
/// case can check that preparation changed nothing.
fn recover_checked(
    renderer: &mut GpuRenderer,
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
    between: impl FnOnce(&GpuRenderer) -> Result<(), String>,
) -> Result<(), String> {
    let request = renderer.recovery_request(active).map_err(|error| error.to_string())?;
    let recovered = request
        .run(|_generation| -> DeviceStateWaker { Arc::new(|| {}) })
        .map_err(|failure| failure.error().to_string())?;
    let (context, surface) = recovered.into_parts();
    let prepared = renderer
        .prepare_rebind(&context, Some(surface), mode)
        .map_err(|error| error.to_string())?;
    between(renderer)?;
    renderer.commit_rebind(prepared).map_err(|error| error.to_string())?;
    check(renderer.device_accepts_gpu_work(), "the recovered device accepts work")
}

/// GPU-work scopes `renderer`'s device gate has admitted so far.
fn admitted(renderer: &GpuRenderer) -> u64 {
    renderer.device_error_snapshot().admitted_work
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

/// The retained wgpu frame's BGRA bytes, copied out through the test hook and read back.
fn wgpu_pixels(renderer: &mut GpuRenderer) -> Result<Vec<u8>, String> {
    let readback = renderer.__copy_retained_frame().ok_or("readback is enabled")?;
    let slice = readback.buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    readback
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("poll the readback: {error}"))?;
    let mapped = slice.get_mapped_range().map_err(|error| format!("map the readback: {error}"))?;
    let bytes = unpad_readback_rows(
        &mapped,
        readback.width_px,
        readback.height_px,
        readback.padded_row_bytes,
    );
    drop(mapped);
    readback.buffer.unmap();
    Ok(bytes)
}

/// Every pixel of a GDI renderer's software frame. The hook exists only on Windows.
#[cfg(not(target_os = "windows"))]
fn software_pixels(_renderer: &GpuRenderer, _window: &Window) -> Result<Vec<[u8; 4]>, String> {
    Err(String::from("the GDI presenter exists only on Windows"))
}

/// Every pixel of a GDI renderer's software frame, each read through the test hook.
#[cfg(target_os = "windows")]
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
/// while stopped; recovery builds it at 1x1 when degraded and at the surface size when not. Neither the
/// release nor either switch admits GPU work on the stopped device: its admission count does not move.
fn stopped_device(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut stopped) = renderer(active, SoftwareRenderMode::Off, "idle-atlas-stopped")?;
    render(&mut stopped, true)?;
    let promoted_mirror = stopped.__test_image_atlas_dimensions().1;
    render(&mut stopped, false)?;
    stopped.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    check(!stopped.device_accepts_gpu_work(), "the device is stopped")?;
    let before = admitted(&stopped);
    check(stopped.release_idle_image_atlas(Instant::now() + AFTER_INTERVAL), "the release ran")?;
    check(admitted(&stopped) == before, "the stopped release admits no GPU work")?;
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
    let before = admitted(&degraded);
    degraded.set_software_render_degrade(false);
    check(admitted(&degraded) == before, "the stopped switch off admits no GPU work")?;
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
    let before = admitted(&hardware);
    hardware.set_software_render_degrade(true);
    check(admitted(&hardware) == before, "the stopped switch on admits no GPU work")?;
    check(hardware.frame_texture_extent() == full, "a stopped device keeps the full texture")?;
    recover(&mut hardware, active, SoftwareRenderMode::Force)?;
    check(hardware.frame_texture_extent() == (1, 1), "recovery under degrade builds 1x1")
}

/// A trimmed frame texture across a stop: preparation changes nothing, the commit installs the
/// presenter's texture and clears the mark, and the first recovered present rebuilds nothing. Every
/// count is a delta of the renderer's own texture installs. Covered for wgpu recovering on wgpu,
/// wgpu recovering onto GDI (the mark is read directly, since GDI never consults it), and GDI.
fn trim_stop_rebind(active: &ActiveEventLoop) -> Result<(), String> {
    for (start, target) in [
        (SoftwareRenderMode::Off, SoftwareRenderMode::Off),
        (SoftwareRenderMode::Off, SoftwareRenderMode::Force),
        (SoftwareRenderMode::Force, SoftwareRenderMode::Force),
    ] {
        let label = format!("{start:?} to {target:?}");
        let (window, mut renderer) = renderer(active, start, "trim-stop-rebind")?;
        render(&mut renderer, false)?;
        let starts_on_wgpu = matches!(start, SoftwareRenderMode::Off);
        let installs = renderer.__frame_texture_rebuilds();
        let _ = renderer.trim_for_occlusion();
        check(
            renderer.__frame_texture_trimmed() == starts_on_wgpu,
            &format!("{label}: only the GPU presenter marks its texture"),
        )?;
        check(renderer.__frame_texture_rebuilds() == installs, &format!("{label}: no install"))?;
        renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
        check(
            render(&mut renderer, false).is_err(),
            &format!("{label}: rendering is unavailable"),
        )?;
        recover_checked(&mut renderer, active, target, |prepared| {
            check(
                prepared.__frame_texture_trimmed() == starts_on_wgpu
                    && prepared.__frame_texture_rebuilds() == installs,
                &format!("{label}: preparation changes nothing"),
            )
        })?;
        check(
            !renderer.__frame_texture_trimmed(),
            &format!("{label}: the commit clears the mark"),
        )?;
        check(
            renderer.__frame_texture_rebuilds() == installs + 1,
            &format!("{label}: the commit installs one texture"),
        )?;
        let expected = if matches!(target, SoftwareRenderMode::Force) {
            (1, 1)
        } else {
            let size = window.inner_size();
            (size.width, size.height)
        };
        check(renderer.frame_texture_extent() == expected, &format!("{label}: committed extent"))?;
        let frames = renderer.successful_frame_count();
        render(&mut renderer, false)?;
        check(renderer.successful_frame_count() == frames + 1, &format!("{label}: presents"))?;
        check(
            renderer.__frame_texture_rebuilds() == installs + 1,
            &format!("{label}: the first recovered present rebuilds nothing"),
        )?;
        check(renderer.frame_texture_extent() == expected, &format!("{label}: extent kept"))?;
    }
    Ok(())
}

/// The trim releases a promoted image atlas only when no media is visible, and a placeholder is left
/// alone. Shown again after a trim, the image draws exactly the pixels of the frame before it.
fn trim_image_atlas_and_restore(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut absent) = renderer(active, SoftwareRenderMode::Off, "trim-image-absent")?;
    render(&mut absent, true)?;
    render(&mut absent, false)?;
    let _ = absent.trim_for_occlusion();
    check(
        absent.__test_image_atlas_dimensions() == ((1, 1), (1, 1)),
        "promoted with media absent: the atlas and its mirror are released",
    )?;
    let (_window, mut visible) = renderer(active, SoftwareRenderMode::Off, "trim-image-visible")?;
    render(&mut visible, true)?;
    let promoted = visible.__test_image_atlas_dimensions();
    let _ = visible.trim_for_occlusion();
    check(visible.__test_image_atlas_dimensions() == promoted, "visible media keeps its atlas")?;
    let (_window, mut placeholder) =
        renderer(active, SoftwareRenderMode::Off, "trim-image-placeholder")?;
    let before = placeholder.__test_image_atlas_dimensions();
    let _ = placeholder.trim_for_occlusion();
    check(placeholder.__test_image_atlas_dimensions() == before, "a placeholder is left alone")?;

    let (window, mut shown) = renderer(active, SoftwareRenderMode::Force, "trim-image-restore")?;
    render(&mut shown, true)?;
    let expected = software_pixels(&shown, &window)?;
    render(&mut shown, false)?;
    let _ = shown.trim_for_occlusion();
    check(shown.__test_image_atlas_dimensions().0 == (1, 1), "the trim released the CPU atlas")?;
    render(&mut shown, true)?;
    check(shown.__test_image_atlas_dimensions().0 != (1, 1), "the media promotes it again")?;
    check(software_pixels(&shown, &window)? == expected, "the restored frame equals the first")
}

/// On the GPU presenter, an image shown, removed, trimmed away and shown again presents at once, its
/// atlas and GPU mirror promoted again, with exactly the pixels the frame before the trim read back.
fn trim_restores_the_image_on_wgpu(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) = renderer(active, SoftwareRenderMode::Off, "trim-image-wgpu")?;
    renderer.__enable_retained_frame_readback();
    let frames = renderer.successful_frame_count();
    render(&mut renderer, true)?;
    check(renderer.successful_frame_count() > frames, "the reference frame presents")?;
    let reference = wgpu_pixels(&mut renderer)?;
    render(&mut renderer, false)?;
    let _ = renderer.trim_for_occlusion();
    check(
        renderer.__test_image_atlas_dimensions() == ((1, 1), (1, 1)),
        "the trim released the image atlas and its GPU mirror",
    )?;
    check(renderer.__frame_texture_trimmed(), "the frame texture is trimmed")?;
    let frames = renderer.successful_frame_count();
    render(&mut renderer, true)?;
    check(renderer.successful_frame_count() == frames + 1, "the first recovered frame presents")?;
    let (cpu, gpu) = renderer.__test_image_atlas_dimensions();
    check(cpu != (1, 1) && gpu == cpu, "the atlas and its GPU mirror are promoted again")?;
    check(wgpu_pixels(&mut renderer)? == reference, "the recovered image equals the reference")
}

/// A trim on a stopped device is refused before anything is released: the report says so and gives
/// back nothing, and the retained parts, frame texture, image atlas and trim mark are unchanged.
fn refused_trim_changes_nothing(active: &ActiveEventLoop) -> Result<(), String> {
    let (_window, mut renderer) = renderer(active, SoftwareRenderMode::Off, "trim-refused")?;
    render(&mut renderer, true)?;
    render(&mut renderer, false)?;
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    check(!renderer.device_accepts_gpu_work(), "the device is stopped")?;
    let amounts = renderer.retained_amounts();
    let extent = renderer.frame_texture_extent();
    let atlas = renderer.__test_image_atlas_dimensions();
    let installs = renderer.__frame_texture_rebuilds();
    let report = renderer.trim_for_occlusion();
    check(report.refused && report.gpu_released_requested_bytes == 0, "the trim is refused")?;
    check(renderer.retained_amounts() == amounts, "the retained parts are unchanged")?;
    check(renderer.frame_texture_extent() == extent, "the frame texture is unchanged")?;
    check(renderer.__test_image_atlas_dimensions() == atlas, "the image atlas is unchanged")?;
    check(!renderer.__frame_texture_trimmed(), "the texture is not marked trimmed")?;
    check(renderer.__frame_texture_rebuilds() == installs, "no texture was installed")
}

#[cfg(target_os = "windows")]
#[test]
fn windows_idle_image_atlas_and_gdi_frame_texture() {
    use winit::{event_loop::EventLoop, platform::windows::EventLoopBuilderExtWindows};
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
        ("trim, stop and rebind", trim_stop_rebind),
        ("trim image atlas and restore", trim_image_atlas_and_restore),
        ("trim restores the image on wgpu", trim_restores_the_image_on_wgpu),
        ("refused trim changes nothing", refused_trim_changes_nothing),
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
