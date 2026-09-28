//! Native window creation: attributes, icon, class background, title, size,
//! minimum size, and DPI transitions.

use super::*;

/// Apply WezTerm-style integrated titlebar on macOS.
///
/// The tab bar is now always bottom-pinned, so there is no top tab strip to
/// fuse with the native titlebar. Keep this helper as a no-op compatibility
/// shim so all window creation sites stay in sync.
#[doc(hidden)]
pub fn with_integrated_titlebar(attrs: WindowAttributes) -> WindowAttributes {
    attrs
}

/// Embedded application icon (256×256 PNG), used for the live window's
/// title-bar icon and taskbar button. winit creates its window class with
/// `hIcon: 0` on Windows, so the ONLY way the running window and its
/// taskbar button get our logo (instead of the generic default) is to set
/// it explicitly via `WindowAttributes::with_window_icon`. The MSI/exe
/// resource icon only covers Explorer / shortcuts, not the live window —
/// hence this runtime path. Decoded once and cached.
static APP_ICON: std::sync::OnceLock<Option<winit::window::Icon>> = std::sync::OnceLock::new();

fn app_icon() -> Option<winit::window::Icon> {
    APP_ICON
        .get_or_init(|| {
            const PNG: &[u8] = include_bytes!("../../../../assets/icons/exports/png/sonic-256.png");
            let img = match image::load_from_memory(PNG) {
                Ok(i) => i.to_rgba8(),
                Err(e) => {
                    // When: `image::load_from_memory` rejected the embedded PNG; warn
                    // with `e` and run iconless rather than failing window creation.
                    tracing::warn!(target: "sonicterm_app::app", "app_icon: decode sonic-256.png failed: {e}");
                    return None;
                }
            };
            let (w, h) = img.dimensions();
            match winit::window::Icon::from_rgba(img.into_raw(), w, h) {
                Ok(icon) => Some(icon),
                Err(e) => {
                    tracing::warn!(target: "sonicterm_app::app", "app_icon: Icon::from_rgba failed: {e}");
                    None
                }
            }
        })
        .clone()
}

/// Attach packaged platform identity and the bundled SonicTerm icon to a
/// window's attributes. Applied at every window-creation site.
#[doc(hidden)]
pub fn with_app_icon(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "linux")]
    let attrs = {
        use winit::platform::wayland::WindowAttributesExtWayland;

        attrs.with_name(LINUX_DESKTOP_ID, LINUX_INSTANCE_NAME)
    };
    let attrs = attrs.with_window_icon(app_icon());
    // winit's `with_window_icon` only sets `ICON_SMALL` (the 16px title-bar
    // icon). The taskbar button uses `ICON_BIG`, which must be set
    // separately on Windows — otherwise Windows upscales the 16px small
    // icon for the taskbar and the button looks small/blurry next to other
    // apps (Firefox, Windows Terminal).
    #[cfg(windows)]
    let attrs = {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs.with_taskbar_icon(app_icon())
    };
    attrs
}

#[cfg(target_os = "windows")]
static WINDOW_BG_BRUSHES: std::sync::OnceLock<std::sync::Mutex<HashMap<u32, isize>>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "windows")]
fn native_background_brush(rgb: (u8, u8, u8)) -> Option<isize> {
    use windows::Win32::{Foundation::COLORREF, Graphics::Gdi::CreateSolidBrush};

    // COLORREF is 0x00BBGGRR. Brushes stay alive for the process lifetime:
    // window classes can retain their handles after this call returns, so
    // deleting a superseded theme brush would leave those classes dangling.
    let (r, g, b) = rgb;
    let color = u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16);
    let brushes = WINDOW_BG_BRUSHES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut brushes = brushes.lock().ok()?;
    if let Some(brush) = brushes.get(&color) {
        // When: this `color` was already realized; reuse the cached handle so
        // repeat theme applications do not leak one GDI brush per call.
        return Some(*brush);
    }
    let brush =
        // SAFETY: `CreateSolidBrush` takes the COLORREF by value and has no
        // pointer or lifetime preconditions; failure is reported as a null handle.
        unsafe { CreateSolidBrush(COLORREF(color)) }.0 as isize;
    if brush == 0 {
        // When: GDI refused the allocation and `brush` is null; report absence so
        // callers keep the existing class brush instead of installing handle zero.
        return None;
    }
    brushes.insert(color, brush);
    Some(brush)
}

#[cfg(target_os = "windows")]
#[doc(hidden)]
/// Point the window's class background at a brush of the configured theme color.
///
/// Windows paints newly exposed client area with the class brush before the
/// swapchain presents, so leaving the default makes a resize flash white.
pub fn install_native_window_background(window: &Window, bg_hex: &str) {
    let Some(rgb) = parse_hex_rgb(bg_hex) else {
        // When: `bg_hex` is not a six-digit color, so there is nothing to realize;
        // keep whatever background the class already carries.
        return;
    };
    let Some(brush) = native_background_brush(rgb) else {
        // When: `native_background_brush` exhausted GDI, so installing its null
        // result would blank the class instead of theming it.
        return;
    };
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        // When: `window_handle` reports no live handle, so no window class exists
        // to retarget and the paint would land nowhere.
        return;
    };
    let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
        // When: `handle` is not the `Win32` variant, so this class-word write does
        // not apply to whatever backend produced it.
        return;
    };
    let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
    // SAFETY: `hwnd` is derived from a handle the window just reported as live,
    // and `brush` outlives the class because the cache never frees its brushes.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetClassLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GCLP_HBRBACKGROUND,
            brush,
        );
    }
}

#[cfg(not(target_os = "windows"))]
#[doc(hidden)]
/// Accept the theme background request on platforms with no window class to
/// retarget, so window-creation sites stay identical across platforms.
pub fn install_native_window_background(_window: &Window, _bg_hex: &str) {}

#[cfg(target_os = "windows")]
fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.strip_prefix('#').unwrap_or(hex);
    if h.len() != 6 || !h.is_ascii() {
        // When: `h` is not exactly six ASCII bytes, so the fixed byte slices below
        // could panic; refuse the value instead of indexing inside a code point.
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Enable OS-window alpha composition when a non-opaque compositor backdrop
/// is requested. Without this, winit creates an opaque client area and the
/// premultiplied swapchain is composited over that instead of Mica/acrylic.
#[doc(hidden)]
pub fn with_backdrop_transparency(
    attrs: WindowAttributes,
    backdrop: BackdropKind,
    software_render_mode: SoftwareRenderMode,
) -> WindowAttributes {
    if backdrop == BackdropKind::Opaque || software_render_mode == SoftwareRenderMode::Force {
        attrs
    } else {
        // When: `backdrop` asks the compositor for Mica or acrylic and the GPU
        // path will present premultiplied alpha, which an opaque surface discards.
        attrs.with_transparent(true)
    }
}

pub(super) fn compose_window_title(key: sonicterm_types::WindowKey, name: &str) -> String {
    let title = if name.is_empty() { NATIVE_WINDOW_TITLE } else { name };
    format!("#{} {title}", key.raw())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowRequest {
    pub(super) inner_size: winit::dpi::LogicalSize<f64>,
}

pub(super) fn configured_window_size(
    config: &Config,
    tab_bar_visible: bool,
) -> winit::dpi::LogicalSize<f64> {
    let (cols, rows) = sonicterm_grid::grid::bounded_grid_size(
        u64::from(config.window.cols),
        u64::from(config.window.rows),
    );
    let width = f32::from(cols) * 9.0 + config.window.padding_left + config.window.padding_right;
    let height = f32::from(rows) * config.font.size * config.font.line_height
        + config.window.padding_top
        + config.window.padding_bottom
        + if tab_bar_visible { sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT } else { 0.0 };
    winit::dpi::LogicalSize::new(f64::from(width), f64::from(height))
}

pub(super) fn inherited_window_size(
    physical: winit::dpi::PhysicalSize<u32>,
    scale: f64,
    minimized: bool,
) -> Option<winit::dpi::LogicalSize<f64>> {
    if minimized
        || physical.width == 0
        || physical.height == 0
        || !scale.is_finite()
        || scale <= 0.0
    {
        // When: native geometry is unavailable, retain the configured fallback rather than a minimized or invalid extent.
        return None;
    }
    Some(physical.to_logical(scale))
}

pub(super) fn apply_window_request(
    window: &Window,
    renderer: &mut GpuRenderer,
    request: WindowRequest,
) -> bool {
    let (cell_w, cell_h) = renderer.cell_size();
    let minimum = minimum_terminal_inner_size(
        cell_w,
        cell_h,
        renderer.padding_left_px(),
        renderer.padding_right_px(),
        renderer.top_inset(),
        renderer.bottom_inset(),
        renderer.padding_bottom_px(),
    );
    window.set_min_inner_size(Some(minimum));
    let desired = request.inner_size.to_physical::<u32>(window.scale_factor());
    let desired = winit::dpi::PhysicalSize::new(
        desired.width.max(minimum.width),
        desired.height.max(minimum.height),
    );
    // An asynchronous request leaves the current extent authoritative until Resized supplies the accepted dimensions.
    let actual = window.request_inner_size(desired).unwrap_or_else(|| window.inner_size());
    renderer.try_resize(actual.width, actual.height)
}

/// Compute the physical inner-window floor that preserves a 30×10 terminal grid.
#[must_use]
pub fn minimum_terminal_inner_size(
    cell_w: f32,
    cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    top_inset: f32,
    bottom_inset: f32,
    padding_bottom: f32,
) -> winit::dpi::PhysicalSize<u32> {
    let width = (f32::from(MIN_WINDOW_COLS) * cell_w + padding_left + padding_right).ceil();
    let height =
        (f32::from(MIN_WINDOW_ROWS) * cell_h + top_inset + bottom_inset + padding_bottom).ceil();
    winit::dpi::PhysicalSize::new(width.max(1.0) as u32, height.max(1.0) as u32)
}

/// Select the scale that the platform uses to report its current physical inner size.
#[must_use]
pub(super) fn dpi_transition_size_scale(stored_scale: f64, native_scale: f64) -> f64 {
    if cfg!(target_os = "macos") {
        // When: target_os is macos, AppKit already reports inner_size using its current backing scale.
        native_scale
    } else {
        // When: target_os is not macos, native size retains the stored pre-transition scale contract.
        stored_scale
    }
}

/// Project an observed physical extent through its source scale, then apply terminal and monitor bounds.
#[must_use]
pub(super) fn dpi_transition_inner_size(
    current: winit::dpi::PhysicalSize<u32>,
    source_scale: f64,
    new_scale: f64,
    minimum: winit::dpi::PhysicalSize<u32>,
    available_inner: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    let suggested =
        current.to_logical::<f64>(source_scale.max(0.1)).to_physical::<u32>(new_scale.max(0.1));
    let upper_width = available_inner.width.max(minimum.width);
    let upper_height = available_inner.height.max(minimum.height);
    winit::dpi::PhysicalSize::new(
        suggested.width.max(minimum.width).min(upper_width),
        suggested.height.max(minimum.height).min(upper_height),
    )
}

#[cfg(target_os = "windows")]
fn destination_available_inner_size(
    window: &Window,
    old_scale: f64,
    new_scale: f64,
    minimum: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    let outer = window.outer_size();
    let inner = window.inner_size();
    let decoration_scale = new_scale.max(0.1) / old_scale.max(0.1);
    let decoration_width =
        (f64::from(outer.width.saturating_sub(inner.width)) * decoration_scale).ceil() as u32;
    let decoration_height =
        (f64::from(outer.height.saturating_sub(inner.height)) * decoration_scale).ceil() as u32;
    let Ok(handle) = window.window_handle() else {
        // When: no native handle is available, preserve the minimum without inventing a monitor cap.
        return winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX);
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        // When: the handle is not Win32, this Windows-only monitor query cannot classify it.
        return winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX);
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
    let monitor =
        // SAFETY: hwnd is the live winit window; the API returns an opaque monitor handle.
        unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info =
        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !
        // SAFETY: info points to initialized writable storage with cbSize set as required.
        unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
    {
        // When: GetMonitorInfoW fails, preserve the minimum without applying an unproven cap.
        return winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX);
    }
    let work_width = u32::try_from(info.rcWork.right.saturating_sub(info.rcWork.left)).unwrap_or(0);
    let work_height =
        u32::try_from(info.rcWork.bottom.saturating_sub(info.rcWork.top)).unwrap_or(0);
    winit::dpi::PhysicalSize::new(
        work_width.saturating_sub(decoration_width).max(minimum.width),
        work_height.saturating_sub(decoration_height).max(minimum.height),
    )
}

#[cfg(not(target_os = "windows"))]
fn destination_available_inner_size(
    _window: &Window,
    _old_scale: f64,
    _new_scale: f64,
    _minimum: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX)
}

/// Apply one scale-factor transition to native, renderer, and pane geometry.
pub(super) fn apply_window_dpi_transition(
    window: &mut WindowState,
    dpi_scale: f64,
    inner_size_writer: &mut InnerSizeWriter,
) -> Option<winit::dpi::PhysicalSize<u32>> {
    let old_scale = window.dpi_scale;
    // A window without a renderer must retain the event scale for its later initialization.
    window.dpi_scale = dpi_scale;
    let native = window.window.as_ref()?.clone();
    let renderer = window.renderer.as_mut()?;
    let native_scale = native.scale_factor();
    let old_inner = native.inner_size();
    let size_scale = dpi_transition_size_scale(old_scale, native_scale);
    let renderer_before = renderer.logical_size();
    let cell_before = renderer.cell_size();
    renderer.set_scale_factor(dpi_scale as f32);
    let suggested =
        old_inner.to_logical::<f64>(size_scale.max(0.1)).to_physical::<u32>(dpi_scale.max(0.1));

    let (cell_w, cell_h) = renderer.cell_size();
    let minimum = minimum_terminal_inner_size(
        cell_w,
        cell_h,
        renderer.padding_left_px(),
        renderer.padding_right_px(),
        renderer.top_inset(),
        renderer.bottom_inset(),
        renderer.padding_bottom_px(),
    );
    native.set_min_inner_size(Some(minimum));
    if native.is_maximized() || native.fullscreen().is_some() {
        // When: native is maximized or fullscreen, propagate new metrics while Windows owns native sizing.
        child_window::resize_visible_panes_in_child(window);
        window.ime_cursor_throttle.reset();
        native.request_redraw();
        return None;
    }
    let available = destination_available_inner_size(&native, old_scale, dpi_scale, minimum);
    let target = dpi_transition_inner_size(old_inner, size_scale, dpi_scale, minimum, available);
    if !renderer.try_resize(target.width, target.height) {
        // When: try_resize rejects target, leave the native writer untouched and await Resized.
        return None;
    }
    if let Err(error) = inner_size_writer.request_inner_size(target) {
        // When: request_inner_size returns error, restore the renderer extent before returning.
        let _ = renderer.try_resize(old_inner.width, old_inner.height);
        tracing::warn!(
            target: "sonicterm_app::app",
            ?error,
            window_id = ?native.id(),
            old_scale,
            new_scale = dpi_scale,
            native_scale,
            size_scale,
            ?old_inner,
            ?target,
            "DPI transition size rejected"
        );
        return None;
    }
    let renderer_after = renderer.logical_size();
    child_window::resize_visible_panes_in_child(window);
    window.ime_cursor_throttle.reset();
    tracing::info!(
        target: "sonicterm_app::app",
        window_id = ?native.id(),
        old_scale,
        new_scale = dpi_scale,
        native_scale,
        size_scale,
        ?old_inner,
        ?suggested,
        ?minimum,
        ?available,
        ?target,
        ?renderer_before,
        ?renderer_after,
        ?cell_before,
        cell_after = ?(cell_w, cell_h),
        "DPI transition synchronized"
    );
    window.request_redraw();
    Some(target)
}

/// Refresh one native window's minimum from its live renderer geometry.
pub fn apply_terminal_window_minimum(
    window: &Window,
    renderer: &mut GpuRenderer,
) -> winit::dpi::PhysicalSize<u32> {
    let (cell_w, cell_h) = renderer.cell_size();
    let minimum = minimum_terminal_inner_size(
        cell_w,
        cell_h,
        renderer.padding_left_px(),
        renderer.padding_right_px(),
        renderer.top_inset(),
        renderer.bottom_inset(),
        renderer.padding_bottom_px(),
    );
    window.set_min_inner_size(Some(minimum));
    let current = window.inner_size();
    let target = winit::dpi::PhysicalSize::new(
        current.width.max(minimum.width),
        current.height.max(minimum.height),
    );
    if target != current {
        // When: `target != current`, grow the undersized axes without shrinking the others.
        let _ = window.request_inner_size(target);
        let _ = renderer.try_resize(target.width, target.height);
    }
    target
}

pub(super) fn apply_window_state_minimum(window: &mut WindowState) {
    if let (Some(native), Some(renderer)) = (window.window.as_ref(), window.renderer.as_mut()) {
        // When: both native window and renderer exist, refresh their shared minimum geometry.
        let _ = apply_terminal_window_minimum(native, renderer);
    }
}
