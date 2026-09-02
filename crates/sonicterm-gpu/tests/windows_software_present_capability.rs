#![cfg(target_os = "windows")]

use std::sync::Arc;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{ScrollbarMode, SoftwareRenderMode},
            theme::{Hex, Theme},
        },
        grid::grid::Grid,
        ui::{selection::Selection, tabs::TabBar},
    },
    CursorStyle, PaneRender, PixelRect,
};
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

const BACKGROUND_HEX: &str = "#123456";
const SELECTION_HEX: &str = "#e04020";
const EXPECTED_COLORREF: u32 = 0x0056_3412;

#[derive(Debug)]
enum Capability {
    Exercised { detected_software_adapter: bool, observed: u32 },
    HostIncapable(String),
}

struct Probe {
    outcome: Option<Result<Capability, String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        self.outcome = Some(run_probe(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn run_probe(active: &ActiveEventLoop) -> Result<Capability, String> {
    let window = match active.create_window(
        Window::default_attributes()
            .with_inner_size(PhysicalSize::new(96, 64))
            .with_visible(true)
            .with_title("SonicTerm software-present capability"),
    ) {
        Ok(window) => Arc::new(window),
        Err(error) => {
            return Ok(Capability::HostIncapable(format!("window creation failed: {error}")))
        }
    };

    let mut theme = Theme::default();
    theme.colors.background = Hex(BACKGROUND_HEX.to_string());
    theme.colors.selection_bg = Hex(SELECTION_HEX.to_string());
    let settings = RendererSettings {
        font_family: "monospace",
        font_dirs: &[],
        font_size: 14.0,
        line_height_mult: 1.2,
        font_weight_scale: 1.0,
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: 1.0,
            scrollbar: ScrollbarMode::Auto,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Force,
        },
        role: "software-present-capability",
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, &theme, settings) {
        Ok(renderer) => renderer,
        Err(error) => {
            return Ok(Capability::HostIncapable(format!("renderer creation failed: {error}")))
        }
    };
    if !renderer.is_software_render_degraded() {
        return Err(String::from("forced software-render mode did not engage"));
    }
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);

    let size = window.inner_size();
    let mut grid = Grid::new(8, 4);
    grid.scroll_region_up(0, 3, 1);
    if grid.scrollback_len() == 0 {
        return Err(String::from("scrollbar fixture did not create primary-screen scrollback"));
    }
    let mut panes = [PaneRender {
        id: 1,
        rect_px: PixelRect { x: 0, y: 0, w: size.width, h: size.height },
        grid: &mut grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_receiver: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    let tabs = TabBar::new();
    render_frame(&mut renderer, &mut panes, &theme, None, &tabs)?;

    let handle =
        window.window_handle().map_err(|error| format!("window handle failed: {error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(String::from("created window did not expose a Win32 handle"));
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let hdc =
        // SAFETY: `hwnd` belongs to the live `window`; this DC is released once below with the same `hwnd`.
        unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        return Ok(Capability::HostIncapable(String::from("GetDC returned null")));
    }
    let observed =
        // SAFETY: `hdc` is the live non-null DC for `hwnd`; `GetPixel` borrows it and retains no pointer.
        unsafe { GetPixel(hdc, 8, 8) }.0;
    let _ =
        // SAFETY: pairs once with the successful `GetDC` above, using the exact same live `hwnd` and `hdc`.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    if observed == CLR_INVALID {
        return Ok(Capability::HostIncapable(String::from("GetPixel returned CLR_INVALID")));
    }
    if observed != EXPECTED_COLORREF {
        return Err(format!(
            "software-present round trip changed the known background: expected COLORREF \
             {EXPECTED_COLORREF:#010x}, observed {observed:#010x}"
        ));
    }

    let scrollbar_x = size.width.saturating_sub(5);
    let scrollbar_y = size.height / 2;
    let baseline_count = renderer.successful_frame_count();
    let baseline_cpu = renderer
        .__test_software_frame_pixel_bgra(scrollbar_x, scrollbar_y)
        .ok_or_else(|| String::from("baseline software scrollbar pixel unavailable"))?;
    let baseline_hwnd = read_hwnd_pixel(hwnd, scrollbar_x as i32, scrollbar_y as i32)?;
    panes[0].scrollbar_alpha = 1.0;
    render_frame(&mut renderer, &mut panes, &theme, None, &tabs)?;
    if renderer.successful_frame_count() != baseline_count + 1 {
        return Err(String::from("alpha-only scrollbar show did not present a new frame"));
    }
    let visible_cpu = renderer
        .__test_software_frame_pixel_bgra(scrollbar_x, scrollbar_y)
        .ok_or_else(|| String::from("visible software scrollbar pixel unavailable"))?;
    let visible_hwnd = read_hwnd_pixel(hwnd, scrollbar_x as i32, scrollbar_y as i32)?;
    if visible_cpu == baseline_cpu || visible_hwnd == baseline_hwnd {
        return Err(String::from("visible scrollbar did not change CPU and HWND edge pixels"));
    }
    panes[0].scrollbar_alpha = 0.0;
    render_frame(&mut renderer, &mut panes, &theme, None, &tabs)?;
    if renderer.successful_frame_count() != baseline_count + 2 {
        return Err(String::from("alpha-only scrollbar hide did not present a new frame"));
    }
    let restored_cpu = renderer
        .__test_software_frame_pixel_bgra(scrollbar_x, scrollbar_y)
        .ok_or_else(|| String::from("restored software scrollbar pixel unavailable"))?;
    let restored_hwnd = read_hwnd_pixel(hwnd, scrollbar_x as i32, scrollbar_y as i32)?;
    if restored_cpu != baseline_cpu || restored_hwnd != baseline_hwnd {
        return Err(String::from("hidden scrollbar did not restore the exact baseline pixels"));
    }

    panes[0].grid.enter_alt_screen();
    let (cell_w, cell_h) = renderer.cell_size();
    let sample_x = (cell_w * 1.5).round() as i32;
    let sample_y = (cell_h * 1.5).round() as i32;
    let mut selection = Selection::new(1, 1);
    selection.extend(1, 2);
    selection.anchored = true;
    selection = selection.with_content_state(1, panes[0].grid.content_seq(), true, 0);
    render_frame(&mut renderer, &mut panes, &theme, Some(&selection), &tabs)?;
    let selected = read_hwnd_pixel(hwnd, sample_x, sample_y)?;
    if selected == observed {
        return Err(String::from("selection fixture did not change the sampled HWND pixel"));
    }

    selection = selection.with_content_state(1, panes[0].grid.content_seq(), true, 0);
    panes[0].grid.scroll_up(1);
    if !sonicterm_render_model::boundary::ui::selection::revalidate_selection(
        &mut selection,
        1,
        panes[0].grid,
    ) {
        return Err(String::from(
            "intersecting alternate-screen scroll did not invalidate selection",
        ));
    }
    render_frame(&mut renderer, &mut panes, &theme, None, &tabs)?;
    let deselected = read_hwnd_pixel(hwnd, sample_x, sample_y)?;
    if deselected != observed {
        return Err(format!(
            "first deselected HWND frame retained selection pixels: reference \
             {observed:#010x}, selected {selected:#010x}, deselected {deselected:#010x}"
        ));
    }

    Ok(Capability::Exercised {
        detected_software_adapter: renderer.is_software_rendering(),
        observed,
    })
}

fn render_frame(
    renderer: &mut GpuRenderer,
    panes: &mut [PaneRender<'_>],
    theme: &Theme,
    selection: Option<&Selection>,
    tabs: &TabBar,
) -> Result<(), String> {
    renderer
        .render(panes, theme, false, selection, None, tabs, None, None, None, None, None, None)
        .map_err(|error| format!("software render failed: {error}"))
}

fn read_hwnd_pixel(hwnd: HWND, x: i32, y: i32) -> Result<u32, String> {
    let hdc =
        // SAFETY: the caller supplies the live window's `hwnd`; this DC is released once below with the same handle.
        unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        return Err(String::from("GetDC returned null"));
    }
    let observed =
        // SAFETY: `hdc` is the live non-null DC for `hwnd`; `GetPixel` borrows it and retains no pointer.
        unsafe { GetPixel(hdc, x, y) }.0;
    let _ =
        // SAFETY: pairs once with the successful `GetDC` above, using the exact same live `hwnd` and `hdc`.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    if observed == CLR_INVALID {
        return Err(String::from("GetPixel returned CLR_INVALID"));
    }
    Ok(observed)
}

#[test]
fn report_windows_software_present_round_trip() {
    let event_loop = match EventLoop::builder().with_any_thread(true).build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            println!("capability=HOST_INCAPABLE reason=event-loop:{error}");
            return;
        }
    };
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("event loop runs");

    match probe.outcome.expect("resumed must run") {
        Ok(Capability::Exercised { detected_software_adapter, observed }) => println!(
            "capability=EXERCISED presenter=windows-software \
             detected_software_adapter={detected_software_adapter} observed={observed:#010x}"
        ),
        Ok(Capability::HostIncapable(reason)) => {
            println!("capability=HOST_INCAPABLE reason={reason}")
        }
        Err(error) => panic!("capability=INCORRECT_OUTPUT {error}"),
    }
}
