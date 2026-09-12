#![cfg(target_os = "windows")]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
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
use std::sync::Arc;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
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
        self.outcome = Some(run_probe(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn hwnd_pixel(window: &Window, x: u32, y: u32) -> Result<u32, String> {
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(String::from("test window is not Win32"));
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let dc =
        // SAFETY: `hwnd` belongs to the live test window; its borrowed DC is released below.
        unsafe { GetDC(Some(hwnd)) };
    if dc.0.is_null() {
        return Err(String::from("GetDC returned null"));
    }
    let value =
        // SAFETY: `dc` is live and the scalar sample coordinates are inside the test client area.
        unsafe { GetPixel(dc, x as i32, y as i32) }.0;
    let _ =
        // SAFETY: this releases the exact DC returned by `GetDC` to the same live window.
        unsafe { ReleaseDC(Some(hwnd), dc) };
    if value == CLR_INVALID {
        return Err(String::from("GetPixel returned CLR_INVALID"));
    }
    Ok(value)
}

fn render(
    renderer: &mut GpuRenderer,
    panes: &mut [PaneRender<'_>],
    theme: &Theme,
) -> Result<(), String> {
    renderer
        .render(
            panes,
            theme,
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

fn frame_pixels(renderer: &GpuRenderer, size: PhysicalSize<u32>) -> Result<Vec<[u8; 4]>, String> {
    (0..size.height)
        .flat_map(|y| (0..size.width).map(move |x| (x, y)))
        .map(|(x, y)| {
            renderer
                .__test_software_frame_pixel_bgra(x, y)
                .ok_or_else(|| String::from("software frame pixel unavailable"))
        })
        .collect()
}

fn run_probe(active: &ActiveEventLoop) -> Result<(), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(320, 180))
                    .with_title("SonicTerm image content-bound regression"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut renderer = GpuRenderer::new(
        window.clone(),
        active,
        &theme,
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
            role: "small-pane-image-test",
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    let size = window.inner_size();
    let mut failures = Vec::new();
    let cases = [
        ("normal", 80, 64, [2.0, 2.0, 2.0, 2.0]),
        ("narrow", 4, 64, [0.0; 4]),
        ("short", 80, 4, [0.0; 4]),
        ("consumed-by-padding", 4, 4, [6.0, 6.0, 6.0, 6.0]),
    ];
    for (label, width, height, padding) in cases {
        renderer.set_padding(padding);
        let left = renderer.padding_left_px();
        let right = renderer.padding_right_px();
        let top = renderer.padding_top_px();
        let bottom = renderer.padding_bottom_px();
        let mut grid = Grid::new(1, 1);
        let mut peer_grid = Grid::new(1, 1);
        let bounds = PixelRect { x: 20, y: 20, w: width, h: height };
        let mut panes = [
            PaneRender {
                id: 1,
                rect_px: bounds,
                grid: &mut grid,
                viewport_top_abs: None,
                is_active: true,
                cursor_style: CursorStyle::BlockSteady,
                is_broadcast_receiver: false,
                scrollbar_alpha: 0.0,
                inline_images: Vec::new(),
            },
            PaneRender {
                id: 2,
                rect_px: PixelRect { x: 20 + width as i32, y: 20, w: 96, h: 96 },
                grid: &mut peer_grid,
                viewport_top_abs: None,
                is_active: false,
                cursor_style: CursorStyle::BlockSteady,
                is_broadcast_receiver: false,
                scrollbar_alpha: 0.0,
                inline_images: Vec::new(),
            },
        ];
        render(&mut renderer, &mut panes, &theme)?;
        let baseline = frame_pixels(&renderer, size)?;
        let sample_x = 22 + width;
        let sample_y = 24;
        let baseline_hwnd = hwnd_pixel(&window, sample_x, sample_y)?;
        let frame_count = renderer.successful_frame_count();
        panes[0].inline_images.push(InlineImage {
            id: 1,
            row: 0,
            col: 0,
            width: 32,
            height: 32,
            bgra: Arc::from([0, 0, 255, 255].repeat(32 * 32)),
        });
        render(&mut renderer, &mut panes, &theme)?;
        if renderer.successful_frame_count() != frame_count + 1 {
            return Err(format!("{label}: image did not trigger a new presented frame"));
        }
        let actual = frame_pixels(&renderer, size)?;
        let content_left = bounds.x as f32 + left;
        let content_top = bounds.y as f32 + top;
        let content_right = bounds.x as f32 + bounds.w as f32 - right;
        let content_bottom = bounds.y as f32 + bounds.h as f32 - bottom;
        let mut changed_inside = false;
        for y in 0..size.height {
            for x in 0..size.width {
                let i = (y * size.width + x) as usize;
                let inside = x as f32 + 0.5 >= content_left
                    && (x as f32 + 0.5) < content_right
                    && y as f32 + 0.5 >= content_top
                    && (y as f32 + 0.5) < content_bottom;
                if actual[i] != baseline[i] {
                    if inside {
                        changed_inside = true;
                    } else {
                        failures.push(format!(
                            "{label}: outside pixel ({x},{y}) changed from {:?} to {:?}",
                            baseline[i], actual[i]
                        ));
                        break;
                    }
                }
            }
            if failures.last().is_some_and(|failure| failure.starts_with(label)) {
                break;
            }
        }
        if label == "normal" && !changed_inside {
            return Err(String::from("normal image control did not draw inside content"));
        }
        if hwnd_pixel(&window, sample_x, sample_y)? != baseline_hwnd {
            failures.push(format!("{label}: peer HWND sample changed"));
        }
        // Repeated frames must retain the same fast path; timing uses this exact renderer before and after planning changes.
        let presented = renderer.successful_frame_count();
        let started = std::time::Instant::now();
        for _ in 0..64 {
            render(&mut renderer, &mut panes, &theme)?;
        }
        println!(
            "unchanged_frame_probe case={label} calls=64 elapsed_ns={}",
            started.elapsed().as_nanos()
        );
        assert_eq!(
            renderer.successful_frame_count(),
            presented,
            "unchanged frames must not rebuild"
        );
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Cell-layout minimums must never enlarge image clipping beyond a narrow or padding-exhausted pane.
#[test]
fn windows_inline_images_respect_actual_small_pane_content() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("small-pane image event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
