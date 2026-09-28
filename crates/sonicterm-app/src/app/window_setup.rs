//! Native window creation: attributes, icon, class background, title, size,
//! minimum size, and DPI transitions.

use super::*;

#[cfg(not(target_os = "windows"))]
mod unix;
// Not named `windows`: a module of that name here would collide with the
// `windows` crate's paths on Windows builds.
#[cfg(target_os = "windows")]
#[path = "window_setup/windows.rs"]
mod windows_os;

#[cfg(not(target_os = "windows"))]
use unix::destination_available_inner_size;
#[cfg(not(target_os = "windows"))]
pub use unix::install_native_window_background;
#[cfg(target_os = "windows")]
use windows_os::destination_available_inner_size;
#[cfg(target_os = "windows")]
pub use windows_os::install_native_window_background;

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
                Ok(decoded) => decoded.to_rgba8(),
                Err(error) => {
                    // When: `image::load_from_memory` rejected the embedded PNG; warn
                    // with `error` and run iconless rather than failing window creation.
                    tracing::warn!(target: "sonicterm_app::app", "app_icon: decode sonic-256.png failed: {error}");
                    return None;
                }
            };
            let (width_px, height_px) = img.dimensions();
            match winit::window::Icon::from_rgba(img.into_raw(), width_px, height_px) {
                Ok(icon) => Some(icon),
                Err(error) => {
                    tracing::warn!(target: "sonicterm_app::app", "app_icon: Icon::from_rgba failed: {error}");
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
