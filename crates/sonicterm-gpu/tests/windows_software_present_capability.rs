#![cfg(target_os = "windows")]

use std::sync::Arc;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::SoftwareRenderMode,
            theme::{Hex, Theme},
        },
        grid::grid::Grid,
        ui::tabs::TabBar,
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
    let settings = RendererSettings {
        font_family: "monospace",
        font_size: 14.0,
        line_height_mult: 1.2,
        font_weight_scale: 1.0,
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: 1.0,
            scrollbar: Default::default(),
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
    renderer
        .render(
            &mut panes,
            &theme,
            false,
            None,
            None,
            &TabBar::new(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map_err(|error| format!("software render failed: {error}"))?;

    let handle =
        window.window_handle().map_err(|error| format!("window handle failed: {error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(String::from("created window did not expose a Win32 handle"));
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        return Ok(Capability::HostIncapable(String::from("GetDC returned null")));
    }
    let observed = unsafe { GetPixel(hdc, 8, 8) }.0;
    let _ = unsafe { ReleaseDC(Some(hwnd), hdc) };
    if observed == CLR_INVALID {
        return Ok(Capability::HostIncapable(String::from("GetPixel returned CLR_INVALID")));
    }
    if observed != EXPECTED_COLORREF {
        return Err(format!(
            "software-present round trip changed the known background: expected COLORREF \
             {EXPECTED_COLORREF:#010x}, observed {observed:#010x}"
        ));
    }

    Ok(Capability::Exercised {
        detected_software_adapter: renderer.is_software_rendering(),
        observed,
    })
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
