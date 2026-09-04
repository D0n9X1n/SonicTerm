//! App loop. Owns the window, the GPU renderer, all tab/pane state, the
//! per-pane PTYs and parsers, selection state, and clipboard. Drives keymap
//! dispatch.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use arboard::Clipboard;
use parking_lot::Mutex;
use sonicterm_cfg::config::{BackdropKind, Config, SoftwareRenderMode};
use sonicterm_cfg::keymap::{Action, BroadcastScope, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_resource::ResourceGovernor;
use sonicterm_types::{
    GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceClass, ResourceOwnerId,
};
use sonicterm_vt::vt::{CommandEvent, MouseTracking, Parser};
use winit::{
    application::ApplicationHandler,
    event::{InnerSizeWriter, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Window, WindowAttributes, WindowId},
};

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
                    tracing::warn!("app_icon: decode sonic-256.png failed: {e}");
                    return None;
                }
            };
            let (w, h) = img.dimensions();
            match winit::window::Icon::from_rgba(img.into_raw(), w, h) {
                Ok(icon) => Some(icon),
                Err(e) => {
                    tracing::warn!("app_icon: Icon::from_rgba failed: {e}");
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
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;

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

use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::broadcast::BroadcastState;
use sonicterm_ui::command_palette::CommandPalette;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::ime::ImeState;
use sonicterm_ui::overlays::{NotificationBubble, NotificationLevel};
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::search::SearchState;
use sonicterm_ui::selection::{SelectMode, Selection};
use sonicterm_ui::tabs::{CommandStatus, Tab, TabBar};

/// Classification of a window tracked in the app's role-tagged window map.
///
/// Each tracked window carries a role, so callers can count or select windows
/// by kind rather than by identity. Only terminals are tracked today, so the
/// enum has a single variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRole {
    /// A terminal window created by tearing a tab off the bar.
    ///
    /// The detached pane's PTY threads keep running across the tear-out; their
    /// redraw target is repointed at the child's surface so shell output
    /// redraws the window that now contains it rather than the parent.
    Terminal,
}

#[derive(Debug, Clone)]
pub struct SplitterDragState {
    pub splitter: sonicterm_ui::pane::SplitterId,
    pub axis: sonicterm_ui::pane::SplitAxis,
    pub last_pos: (f32, f32),
}

/// Native OS window title. Keep static; terminal/tab titles render inside
/// SonicTerm's own tab bar.
pub const NATIVE_WINDOW_TITLE: &str = "SonicTerm";

/// Linux desktop entry, AppStream component, and Wayland application ID.
pub const LINUX_DESKTOP_ID: &str = "com.d0n9x1n.SonicTerm";

/// Linux X11 `WM_CLASS` instance paired with [`LINUX_DESKTOP_ID`].
pub const LINUX_INSTANCE_NAME: &str = "sonicterm";

/// Maximum gap (ms) between consecutive left-presses on the same cell for
/// them to count as a double/triple click. Beyond this the streak resets
/// to a single click.
pub const MULTI_CLICK_MS: u128 = 400;

/// Hard minimum terminal content width in cells for every native window.
pub const MIN_WINDOW_COLS: u16 = 30;
/// Hard minimum terminal content height in cells for every native window.
pub const MIN_WINDOW_ROWS: u16 = 10;

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

/// Resolve one DPI transition to a bounded physical inner size.
///
/// The current physical size is projected through the old and new scales to
/// preserve logical geometry. The live terminal minimum wins, while the
/// destination monitor's available inner area caps the result so a low-to-high
/// DPI move cannot create an unreachable native window.
#[must_use]
fn dpi_transition_inner_size(
    current: winit::dpi::PhysicalSize<u32>,
    old_scale: f64,
    new_scale: f64,
    minimum: winit::dpi::PhysicalSize<u32>,
    available_inner: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    let suggested =
        current.to_logical::<f64>(old_scale.max(0.1)).to_physical::<u32>(new_scale.max(0.1));
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
fn apply_window_dpi_transition(
    window: &mut WindowState,
    dpi_scale: f64,
    inner_size_writer: &mut InnerSizeWriter,
) -> Option<winit::dpi::PhysicalSize<u32>> {
    let old_scale = window.dpi_scale;
    window.dpi_scale = dpi_scale;
    let native = window.window.as_ref()?.clone();
    let renderer = window.renderer.as_mut()?;
    let old_inner = native.inner_size();
    renderer.set_scale_factor(dpi_scale as f32);
    let suggested =
        old_inner.to_logical::<f64>(old_scale.max(0.1)).to_physical::<u32>(dpi_scale.max(0.1));

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
    let target = dpi_transition_inner_size(old_inner, old_scale, dpi_scale, minimum, available);
    if !renderer.try_resize(target.width, target.height) {
        // When: try_resize rejects target, leave the native writer untouched and await Resized.
        return None;
    }
    if let Err(error) = inner_size_writer.request_inner_size(target) {
        // When: request_inner_size returns error, restore the renderer extent before returning.
        let _ = renderer.try_resize(old_inner.width, old_inner.height);
        tracing::warn!(
            ?error,
            old_scale,
            new_scale = dpi_scale,
            ?old_inner,
            ?target,
            "DPI transition size rejected"
        );
        return None;
    }
    child_window::resize_visible_panes_in_child(window);
    window.ime_cursor_throttle.reset();
    tracing::info!(
        old_scale,
        new_scale = dpi_scale,
        ?old_inner,
        ?suggested,
        ?minimum,
        ?available,
        ?target,
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

fn apply_window_state_minimum(window: &mut WindowState) {
    if let (Some(native), Some(renderer)) = (window.window.as_ref(), window.renderer.as_mut()) {
        // When: both native window and renderer exist, refresh their shared minimum geometry.
        let _ = apply_terminal_window_minimum(native, renderer);
    }
}

/// Multi-click counter. Returns the new click count (1, 2, 3, then wraps
/// back to 1 after a triple). A click counts as a continuation when it
/// lands on the same cell within the multi-click interval; otherwise the
/// streak restarts at 1. Pure so it is unit-testable without a real
/// pointer event sequence.
pub fn next_click_count(prev: u8, same_cell: bool, within_interval: bool) -> u8 {
    if same_cell && within_interval && (1..3).contains(&prev) {
        prev + 1
    } else {
        // When: the press landed elsewhere, arrived after the gap, or `prev` already
        // reached a triple, so it opens a fresh streak rather than extending one.
        1
    }
}

/// Vsync coalescing gate shared by the main-window (`window_event.rs`) and
/// torn-out child-window (`child_window.rs`) `RedrawRequested` arms.
///
/// Returns `true` when a `RedrawRequested` should be DEFERRED to the next
/// frame boundary instead of rendering now. A redraw is deferred only when
/// both hold:
/// - it is *streaming-driven* — a fresh PTY burst, or not input-driven at
///   all. A PURE input redraw (`was_dirty` with no concurrent `pty_burst`:
///   resize/selection-drag/IME/theme) renders immediately; gating those adds
///   perceptible latency. Crucially a typing echo is BOTH dirty and
///   a burst, and counts as streaming so it coalesces rather
///   than rendering per echo chunk.
/// - `since_last_render < frame_period` — we already drew inside this vsync
///   window, so another draw now would just burn a frame.
///
/// Extracted as a pure fn so main and child use byte-identical
/// coalescing logic AND it is unit-testable without a winit loop. Deferral
/// is what lets a bursty `ls -al` coalesce to one frame per vsync; on a
/// torn-out child the same gate also stops the render path from busy-spinning
/// and starving the VT thread's parser lock.
#[must_use]
pub fn should_defer_streaming_redraw(
    was_dirty: bool,
    pty_burst: bool,
    software_render: bool,
    since_last_render: std::time::Duration,
    frame_period: std::time::Duration,
) -> bool {
    // A redraw is coalesce-able when it is streaming-driven: either a fresh
    // PTY burst, or not input-driven at all. Only a *pure* input redraw
    // (input_dirty with NO concurrent PTY burst — resize/selection-drag/IME/
    // theme) renders immediately.
    //
    // The decisive case is typing: a keystroke sets `input_dirty`, and the
    // char only becomes visible via its PTY echo, which arrives as a burst.
    // So the echo's redraw is BOTH `was_dirty` and `pty_burst`. Keying the
    // gate on `!was_dirty` alone let that echo short-circuit coalescing and
    // render per echo chunk — a redraw storm under fast typing and streaming
    // apps like Claude Code. Treating a burst as streaming work
    // (even when input_dirty is also set) coalesces it to the frame boundary;
    // `about_to_wait` re-requests at `last_render + frame_period`, so latency
    // is bounded by one frame and nothing is dropped.
    //
    // `software_render`: on a CPU rasterizer EVERY frame is
    // expensive (full-screen software raster), so even *pure* input redraws
    // are coalesced to the frame cap — fast typing in a TUI like Claude Code
    // would otherwise force a full-screen raster per keystroke and peg the
    // CPU. Costs at most one frame (~33ms) of extra input latency, which is
    // an acceptable trade only because rendering is already slow here. The
    // hardware-GPU path passes `false` and keeps input redraws immediate.
    let streaming = software_render || pty_burst || !was_dirty;
    streaming && since_last_render < frame_period
}

pub const PTY_REDRAW_QUIESCENT: Duration = Duration::from_millis(3);
pub const PTY_REDRAW_MAX_LATENCY: Duration = Duration::from_millis(8);
pub const PTY_REDRAW_FLUSH_BYTES: usize = 128 * 1024;
pub const PTY_REPLY_QUEUE_CAPACITY: usize = 64;
pub const MAX_PANE_COMMAND_EVENTS: usize = 1024;

/// Sum of every per-pane seam cap, computed from the caps themselves.
///
/// Not a chosen figure. Each term is the constant the owning seam already
/// enforces and tests, so this cannot drift from what the seams do — changing
/// a cap changes this automatically, and a test asserts the arithmetic.
///
/// **The rule for what belongs.** A term for exactly the classes a pane owner
/// can be charged for, because this is compared against that owner's ledger
/// total. Both directions matter: a missing term puts the backstop below memory
/// the seams legitimately permit, where it fires during correct operation, and
/// a term for a class that never charges a pane inflates the backstop with
/// memory that cannot appear in the figure it guards. Which classes those are
/// is [`ResourceClass::pane_seam_term`], decided by an exhaustive match that
/// fails to compile until a new class is classified.
///
/// | Class | Cap |
/// | --- | --- |
/// | `GridVisible` + `GridHistory` + `GridAlternate` | `MAX_GRID_CELLS × size_of::<Cell>()` |
/// | `InlineMediaRetained` | `MAX_RETAINED_INLINE_IMAGE_BYTES` |
/// | `ProtocolMetadata` | `MAX_HYPERLINK_METADATA_BYTES` |
/// | `ParserCapture` | `MAX_MEDIA_PAYLOAD_BYTES` + `MAX_ESCAPE_SEQUENCE_BYTES` |
/// | `PtyOutput` | `max_queued_output_ring_bytes()` |
/// | `PtyInput` | `max_pty_queued_input_bytes()` |
///
/// The three grid classes share one term because `MAX_GRID_CELLS` bounds them
/// together rather than each separately. `PtyOutput` carries the structural
/// ceiling of one reader ring per queue slot, not the single ring a real shell
/// pins: a backstop has to sit above what the seam permits, not above what it
/// usually uses. `PtyInput` carries its queue depth times the per-message cap
/// for the same reason — a paste is admitted at its full size, so the seam
/// permits far more than a session typically holds.
///
/// `CommandEvents` is deliberately absent. Its queue is bounded and its
/// retention is real, but no production site charges it, so it cannot appear in
/// the ledger total this is compared against — a term for it would raise the
/// tripwire without raising what the tripwire can see.
pub const PANE_SEAM_CAP_SUM_BYTES: usize = (sonicterm_grid::grid::MAX_GRID_CELLS as usize
    * std::mem::size_of::<sonicterm_types::Cell>())
    + media::MAX_RETAINED_INLINE_IMAGE_BYTES
    + sonicterm_grid::hyperlink::MAX_HYPERLINK_METADATA_BYTES
    + sonicterm_vt::vt::MAX_MEDIA_PAYLOAD_BYTES
    + sonicterm_vt::vt::MAX_ESCAPE_SEQUENCE_BYTES
    + sonicterm_io::pty::max_queued_output_ring_bytes()
    + sonicterm_io::pty::max_pty_queued_input_bytes();

/// Headroom multiplier between the seam caps and the governor's backstop.
///
/// The backstop exists to catch a seam that has *stopped* bounding, so it must
/// sit far enough above correct operation that it never fires there. Two times
/// the sum leaves room for allocator slack, capacity overshoot, and the
/// deliberate residual where a pane keeps one oversized image rather than
/// rendering nothing — while still being a small multiple rather than an
/// unbounded curve.
const BACKSTOP_HEADROOM: usize = 2;

/// The committed budget an `AppPane` owner is held to.
///
/// **A tripwire, not a second enforcement point.** That distinction is what
/// makes it safe, and it is the whole design:
///
/// The objection to a governor limit was that two limits which must agree and
/// are maintained separately will drift, and the one that stops agreeing keeps
/// reporting itself as enforced. That objection holds for a limit that shares
/// the enforcement job. It does not hold for one derived from the other limits
/// and set above all of them: this cannot disagree with the seam caps, because
/// it is computed from them, and it cannot silently stop enforcing, because it
/// was never the thing enforcing.
///
/// What it catches is the failure the seam caps cannot: a seam that has stopped
/// bounding while still reporting itself as bounded. Today produced two of
/// those — a retained figure under-reporting by 1.67×, and a charging path that
/// never ran — and neither would have tripped any per-seam assertion, because
/// each seam was behaving correctly on its own terms.
pub const PANE_COMMITTED_BUDGET_BYTES: usize = PANE_SEAM_CAP_SUM_BYTES * BACKSTOP_HEADROOM;

/// Owner limits: seam caps enforce, the governor backstops.
///
/// Enforcement stays with the per-seam caps that are already tested and
/// falsified. The governor's limit is [`PANE_COMMITTED_BUDGET_BYTES`], derived
/// from those caps and set above them, so it is a tripwire for a seam that has
/// stopped bounding rather than a second bound that must agree with the first.
///
/// Window and process owners stay untracked: their content is the sum of their
/// panes, each already held to its own budget, and a second aggregate limit
/// would be the drift surface this design avoids.
fn pane_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: PANE_COMMITTED_BUDGET_BYTES,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    }
}

/// Owner limits that track without constraining.
///
/// Used for window and process owners, whose retention is the sum of the panes
/// beneath them. Each pane is already held to
/// [`PANE_COMMITTED_BUDGET_BYTES`], so an aggregate limit here would add a
/// second figure to keep in agreement without catching anything the per-pane
/// backstop misses.
fn tracking_only_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    }
}

/// Append parsed command events, bounding both the entries kept and the
/// memory the queue holds.
///
/// Trimming the length is not enough on its own. `Vec::drain` lowers the
/// length and keeps the allocation, so one oversized batch — a 64 KiB parse
/// chunk of `OSC 133` prompt markers is roughly eight thousand events — leaves
/// the queue trimmed to the cap while still holding the peak buffer for as
/// long as the pane lives. Releasing the overshoot keeps the memory the class
/// records and the memory the pane holds the same figure.
///
/// The release runs only when a batch actually overshot, so the steady state,
/// where the queue sits at or below the cap, does not reallocate.
fn append_bounded_command_events(
    queue: &mut Vec<PaneCommandEvent>,
    events: impl IntoIterator<Item = PaneCommandEvent>,
) {
    queue.extend(events);
    if queue.len() > MAX_PANE_COMMAND_EVENTS {
        let excess = queue.len() - MAX_PANE_COMMAND_EVENTS;
        queue.drain(0..excess);
        queue.shrink_to(MAX_PANE_COMMAND_EVENTS);
    }
}

/// Whether coalesced PTY output is due for a redraw.
///
/// Output is held back to batch a burst into one frame; it is released once it
/// has grown past the byte threshold or waited out the latency cap, so a slow
/// trickle still reaches the screen instead of waiting for more bytes.
#[must_use]
pub fn should_flush_pending_pty_redraw(pending_bytes: usize, pending_for: Duration) -> bool {
    pending_bytes >= PTY_REDRAW_FLUSH_BYTES || pending_for >= PTY_REDRAW_MAX_LATENCY
}

/// Frame period cap applied when rendering on a CPU/software rasterizer
/// (~40 fps). On a real GPU the monitor's refresh period is used as-is.
///
/// The DirectWrite + full-screen software path looks smooth at this cadence
/// without asking the CPU rasterizer to track the monitor's full refresh rate.
pub const SOFTWARE_RENDER_FRAME_PERIOD: Duration = Duration::from_micros(25_000);

/// Frame period cap while an IME composition is in flight on the software
/// rasterizer (~12 fps). Each preedit keystroke forces a full-surface raster
/// composing is interactive but doesn't need a high rate, so we
/// cap it lower to roughly halve the whole-surface presents while the user
/// types a long pinyin run. Only applied when BOTH software-render and
/// composing.
pub const SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD: Duration = Duration::from_micros(83_333);

/// Effective frame period given the software-render and IME-composing state.
/// On the hardware path this is the monitor period unchanged. On the software
/// path it's the 40 fps cap, dropped lower only while an IME composition is
/// active.
#[must_use]
pub fn effective_frame_period(
    software_render: bool,
    composing: bool,
    monitor_period: Duration,
) -> Duration {
    if software_render && composing {
        SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD
    } else if software_render {
        // When: `software_render` rasterizes on the CPU with no preedit in
        // flight, so the 40 fps cap applies rather than the lower composing one.
        SOFTWARE_RENDER_FRAME_PERIOD
    } else {
        // When: `software_render` is unset, so a real GPU presents and the panel's
        // own refresh governs rather than any CPU-oriented cap.
        monitor_period
    }
}

/// Resolve the effective frame period for the no-GPU case.
///
/// When `degrade` is true the result is [`SOFTWARE_RENDER_FRAME_PERIOD`],
/// whatever the monitor reports. This is an override, not a `max()`: a monitor
/// slower than the cap — a 30 Hz panel in a VM or over RDP, which is where
/// software rendering usually runs — is resolved to the cap too, asking for
/// more frames than the panel presents. That is long-standing and deliberate;
/// the wording is written this way because "clamped to at least" describes a
/// `max()` this function does not perform.
///
/// With `degrade` false the monitor period passes through unchanged, so the
/// hardware-GPU path is untouched.
///
/// `monitor_period` must be the monitor's own period, never a previously
/// resolved value — passing the resolved period back in makes the decision
/// one-way, because a resolution taken while degrading returns the cap.
#[must_use]
pub fn software_render_frame_period(degrade: bool, monitor_period: Duration) -> Duration {
    if degrade {
        SOFTWARE_RENDER_FRAME_PERIOD
    } else {
        // When: `degrade` is unset, so the hardware path presents at whatever
        // cadence the panel reports and nothing here narrows it.
        monitor_period
    }
}

/// Whether to engage the no-GPU degrade path, combining the config mode with
/// runtime detection. `Auto` follows detection; `Force` always
/// degrades; `Off` never does.
#[must_use]
pub fn should_degrade_for_software_render(
    mode: sonicterm_cfg::config::SoftwareRenderMode,
    detected: bool,
) -> bool {
    use sonicterm_cfg::config::SoftwareRenderMode as M;
    match mode {
        M::Auto => detected,
        M::Force => true,
        M::Off => false,
    }
}

#[derive(Debug, Clone)]
pub struct TearOutTiming {
    pub source: &'static str,
    pub start: Instant,
    pub create_window_ms: f32,
    pub renderer_init_ms: f32,
    pub resize_ms: f32,
    pub install_ms: f32,
}

impl TearOutTiming {
    /// Start a timing record for one tear-out, with every phase still unmeasured.
    ///
    /// `source` names the gesture that began the tear-out, so timings from
    /// different entry points stay distinguishable in the logs.
    #[must_use]
    pub fn new(source: &'static str, start: Instant) -> Self {
        Self {
            source,
            start,
            create_window_ms: 0.0,
            renderer_init_ms: 0.0,
            resize_ms: 0.0,
            install_ms: 0.0,
        }
    }

    /// Milliseconds from the tear-out gesture to the child window's first frame.
    ///
    /// This is the user-visible latency of the whole tear-out, so it spans every
    /// phase rather than any single one. A first render recorded before the
    /// start instant saturates to zero instead of wrapping.
    #[must_use]
    pub fn total_until_first_render_ms(&self, first_render_at: Instant) -> f32 {
        first_render_at.saturating_duration_since(self.start).as_secs_f32() * 1000.0
    }
}

pub const WARM_WINDOW_POOL_MAX: usize = 5;

/// How many pre-created windows to hold ready, from the configured request.
///
/// Prewarmed windows trade idle memory for tear-out latency, so the request is
/// capped and reduced when that trade is a poor one.
#[must_use]
pub fn warm_window_pool_target(configured: u8, software_rendering: bool) -> usize {
    if configured == 0 {
        // When: `configured` opts out of prewarming, so no window is held and each
        // tear-out pays full creation cost.
        return 0;
    }
    if software_rendering {
        // When: `software_rendering` makes every spare window a full CPU surface,
        // so hold one rather than the configured count.
        return 1;
    }
    usize::from(configured).min(WARM_WINDOW_POOL_MAX)
}

/// Whether another window should be prewarmed into the pool right now.
#[must_use]
pub fn warm_window_pool_should_spawn(
    current_len: usize,
    configured: u8,
    software_rendering: bool,
) -> bool {
    current_len < warm_window_pool_target(configured, software_rendering)
}

pub struct WarmWindow {
    pub window: Arc<Window>,
    pub renderer: GpuRenderer,
    pub created_at: Instant,
}

/// Runs the two-phase governor close and returns any refusal to the caller.
fn close_owner(
    governor: &ResourceGovernor,
    owner: ResourceOwnerId,
) -> Result<(), sonicterm_types::BudgetError> {
    governor.begin_close(owner).and_then(|()| governor.finish_close(owner))
}

fn open_url_effect(url: &str) -> std::io::Result<()> {
    sonicterm_cfg::url_open::open(url)
}

/// Closes a governor owner when the thing that owned it drops.
///
/// The charge on a pane is released by `CommittedReservation::Drop`, and its
/// doc comment states why that is correct: *there is no teardown site to
/// forget*. The owner beside it had no such guarantee — it was a plain
/// `Option<ResourceOwnerId>` that vanished when the pane dropped, leaving the
/// governor holding a record that never closed.
///
/// Measured before this: 80 of 80 owners still `Open` after 40 create/destroy
/// cycles, and `OwnerRegistry` has `get` and `insert` and **no `remove`**, so
/// each one is retained for the life of the process along with its `RwLock`,
/// `Mutex`, and two `EnumMap`s over every resource class.
///
/// Six pane-removal sites across four files reach `panes.remove`. Patching
/// each is how the original defect happened; this makes the close a property
/// of ownership instead.
pub(crate) struct OwnerGuard {
    governor: ResourceGovernor,
    owner: ResourceOwnerId,
}

impl OwnerGuard {
    /// Take responsibility for closing `owner` when this drops.
    pub(crate) fn new(governor: ResourceGovernor, owner: ResourceOwnerId) -> Self {
        Self { governor, owner }
    }

    /// The owner this guard will close.
    pub(crate) fn id(&self) -> ResourceOwnerId {
        self.owner
    }
}

impl std::fmt::Debug for OwnerGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OwnerGuard").field("owner", &self.owner).finish()
    }
}

// Lifecycle: dropping an `OwnerGuard` closes `owner` in the governor, releasing
// its ledger record; a refusal leaves that record retained rather than retried.
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        // Charges must already be gone: `finish_close` refuses an owner still
        // holding them. `PaneState` declares `charges` before `owner`, and
        // Rust drops fields in declaration order, so the reservations release
        // before this runs.
        if let Err(error) = close_owner(&self.governor, self.owner) {
            tracing::warn!(
                target: "memory",
                ?error,
                owner = ?self.owner,
                "owner did not close on drop; its record is retained for the process lifetime"
            );
        }
    }
}

/// Install a provisional pane owner only after every committed charge moves.
fn install_transferred_pane_owner(
    pane: &mut PaneState,
    provisional: OwnerGuard,
) -> Result<Option<OwnerGuard>, sonicterm_resource::CommittedBatchTransferError> {
    let owner = provisional.id();
    sonicterm_resource::CommittedReservation::transfer_batch(pane.charges.values_mut(), owner)?;
    Ok(pane.owner.replace(provisional))
}

/// One validated active-pane transition awaiting its visual feedback frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneFocusChange {
    pub(crate) pane_id: u64,
}

/// Pane-local cell resolved from one renderer layout snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PointerCell {
    pub(super) pane_id: u64,
    pub(super) row: u16,
    pub(super) col: u16,
}

/// Owner chosen once when a left-button grid gesture begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerGestureOwner {
    /// SonicTerm selection owns the gesture until release.
    Local,
    /// The terminal owns the gesture with its press-time protocol profile.
    Terminal { tracking: MouseTracking, sgr: bool },
}

/// Left-button gesture retained independently by each terminal window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PointerGesture {
    pub(super) owner: PointerGestureOwner,
    pub(super) press_pane: u64,
    pub(super) last_cell: PointerCell,
}

pub struct WindowState {
    /// Window classification — see [`WindowRole`].
    pub role: WindowRole,
    /// promoted from `Arc<Window>` to
    /// `Option<Arc<Window>>` so test seeders can build a `WindowState`
    /// without running `do_resumed`. In production this is `Some(_)`
    /// the moment `do_resumed` (main) or `create_child_window`
    /// (torn-out) finishes; every call site either short-circuits via
    /// `if let Some(w) = ws.window.as_ref()` or early-returns via
    /// `ws.window.as_ref()?` when the window is gone.
    pub window: Option<Arc<Window>>,
    /// Per-window wgpu renderer. `Some(_)` once `do_resumed` (main
    /// window) or `create_child_window` (torn-out) populates it.
    /// the main window's renderer now lives here too —
    /// the legacy `App.renderer` field was deleted. Read through
    /// [`Self::renderer`] / [`Self::renderer_mut`] which unwrap (always
    /// safe after `do_resumed`).
    pub renderer: Option<GpuRenderer>,
    pub tabs: TabBar,
    pub tab_states: Vec<TabState>,
    pub panes: HashMap<u64, PaneState>,
    /// This window's owner in the governor hierarchy.
    ///
    /// `None` for synthetic windows built by tests that never registered one.
    /// Production windows always have it; the option exists so a test seam
    /// cannot silently register a phantom owner that never closes.
    ///
    /// An [`OwnerGuard`] for the same reason the pane's is: the window is
    /// removed from `self.windows` at three sites across two files, and the
    /// close has to be a property of ownership rather than something each of
    /// them remembers.
    ///
    /// **Declared after `panes`, and the order is load-bearing.** Rust drops
    /// fields in declaration order, and a pane's owner is a child of this one.
    /// `finish_close` refuses an owner that still has live children, so a
    /// window dropped with this field first would strand its own owner part
    /// closed while the panes beneath it were still open. Panes first means
    /// each child guard has already closed by the time this one runs.
    pub(crate) owner: Option<OwnerGuard>,
    pub cursor_pos: (f64, f64),
    pub mouse_down: bool,
    /// Press-latched routing for the current left-button grid gesture.
    pub(crate) pointer_gesture: Option<PointerGesture>,
    pub selection: Option<Selection>,
    /// Multi-click tracking for word/line selection. `last_click_time` is
    /// the timestamp of the most recent left-press; `last_click_cell` is
    /// the grid cell it landed on; `click_count` is the current streak
    /// (1 = single, 2 = double, 3 = triple, then wraps to 1). Updated via
    /// [`WindowState::register_click`].
    pub last_click_time: Option<Instant>,
    pub last_click_cell: (u16, u16),
    pub click_count: u8,
    /// WezTerm-style drag granularity, set on left-press from the click
    /// count: `Cell` (single), `Word` (double), `Line` (triple). While the
    /// button is held, `CursorMoved` extends the selection at this
    /// granularity. See [`SelectMode`] and `Selection::word_drag` /
    /// `Selection::line_drag`.
    pub select_mode: SelectMode,
    /// The grid cell of the press that started the current drag, as a
    /// scrollback-ABSOLUTE row (so word/line drags stay pinned to the same
    /// TEXT as the viewport scrolls). Word/line drags recompute the anchor
    /// word/line from THIS cell against the live grid on every move (robust
    /// to scrollback), so only the cell — not the resolved word/line bounds
    /// — needs to be retained.
    pub select_anchor: (u64, u16),
    pub copy_mode: Option<CopyModeState>,
    pub modifiers: ModifiersState,
    // `cursor_visible` lives on `PaneState`, not here: its per-pane Arc travels
    // with the pane through tear-out. Read it via
    // `ws.panes.get(&active_pane).map(|p| p.cursor_visible.load(...))`.
    pub last_render: Instant,
    /// pointer-cursor-is-link latch. Mirrors
    /// `App.hover_link` (now deleted). Per-window so a torn-out child can
    /// flip its own cursor independently of the main window.
    pub hover_link: bool,
    /// Tab index pressed in the child's bar — same role as
    /// `App::pressed_tab` but for the child window. Used for
    /// drag-from-child merging.
    pub pressed_tab: Option<usize>,
    /// Live drag session for a held-tab gesture in this child window.
    pub drag_session: Option<crate::tab_drag::DragSession>,
    /// Pending cross-window drop target chosen during a drag in the
    /// child's bar; consumed on mouse-up.
    pub drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// Per-window DPI multiplier retained for renderer rasterization
    /// rebuilds when winit reports monitor changes. Cursor/layout math is
    /// raster-px and must not read this field.
    pub dpi_scale: f64,
    /// Per-window IME composition state, so torn-out windows compose CJK
    /// input independently of the main window.
    pub ime: ImeState,
    /// Per-window throttle for
    /// `Window::set_ime_cursor_area`, so each torn-out window throttles its
    /// own IMK runloop traffic independently. Every read path goes through
    /// `self.main()?.ime_cursor_throttle`.
    pub ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle,
    /// Per-window hovered URL or validated local-path span.
    pub hovered_url: Option<hovered_url::HoveredUrl>,
    /// Epoch-guarded openability decision for the local target under this pointer.
    pub(in crate::app) path_probe: path_target::PathProbeState,
    pub notification: Option<NotificationBubble>,
    /// "this window is hidden / drained" latch.
    /// Promoted from the App-level `main_hidden` bool so the visibility
    /// state lives next to the `Window` Arc it gates. Today only the main
    /// window flips this to `true` (when its last tab is torn out and
    /// child windows keep the event loop alive); child windows leave it
    /// `false` and reap on empty instead.
    pub hidden: bool,
    /// Active scrollbar-drag gesture. `Some(_)` between a
    /// thumb mouse-down and the matching release; cursor moves while
    /// set route to the scrollbar instead of extending a selection.
    pub scrollbar_drag: Option<scrollbar_input::ScrollbarDragState>,
    /// Active split-pane divider drag. While set, cursor moves resize the
    /// captured split ratio instead of extending text selection.
    pub splitter_drag: Option<SplitterDragState>,
    /// Current split-divider hover axis, used to restore the OS cursor when
    /// the pointer leaves the divider.
    pub splitter_hover: Option<sonicterm_ui::pane::SplitAxis>,
    /// Per-pane scrollbar visibility/fade state. Inserted
    /// lazily on first interaction; entries for closed panes are
    /// pruned opportunistically on the next render.
    pub scrollbar_vis: HashMap<u64, scrollbar_visibility::ScrollbarVisState>,
    pub pending_tear_out_timing: Option<TearOutTiming>,
    /// Test-only mirror of the renderer's `drag_chip` overlay.
    /// Production code leaves this `None`. Headless tests use
    /// [`App::__test_set_window_drag_chip_marker`] to flip it `Some(true)`
    /// before calling [`App::cancel_drag_session`], then assert it is
    /// `Some(false)` afterward via [`App::__test_window_drag_chip_marker`].
    /// `cancel_drag_session` flips this in lock-step with the real
    /// `renderer.set_drag_chip(None)` call (when `Some(_)`), so the test
    /// observes the SAME loop iteration the production path runs — if
    /// someone deletes the per-window iteration the marker stays `Some(true)`
    /// and the test fails. Headless windows have `renderer: None`, so this seam
    /// is their only way to observe `set_drag_chip(None)`.
    pub test_drag_chip_marker: Option<bool>,
    /// Test-only renderer-focus marker. Headless child windows have
    /// `renderer: None`, so focus lifecycle regression tests seed this marker
    /// and expect the same focus transition that would call
    /// `GpuRenderer::set_window_focused` to update it. Production leaves this
    /// `None`.
    #[doc(hidden)]
    pub test_renderer_focus_marker: Option<bool>,
    /// Test-only viewport override for this window's pane layout, mirroring
    /// [`App::test_viewport_override`] for the MAIN window. When `Some((outer,
    /// cell_w, cell_h))`, `App::compute_pane_rects_for` uses `outer` instead
    /// of the (absent in headless tests) renderer's logical size, and
    /// `child_window::resize_visible_panes_in_child` uses
    /// `(cell_w, cell_h)` for cell metrics. Lets tests exercise the child
    /// split-pane Grid/PTY resize wiring (tear-out, Resized, close, split)
    /// without a live wgpu surface: synthetic children carry `renderer: None`,
    /// so without this override the resize helper silently no-ops.
    /// Stays `None` in release.
    #[doc(hidden)]
    pub test_pane_viewport: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
}

impl WindowState {
    /// Borrow the renderer. Panics if the renderer field is `None`
    /// (pre-`do_resumed` for the main entry; never for child entries —
    /// every child construction site initializes it to `Some(_)`).
    #[inline]
    #[track_caller]
    pub fn renderer(&self) -> &GpuRenderer {
        self.renderer
            .as_ref()
            .expect("WindowState::renderer() called before do_resumed populated it")
    }

    /// Mutable counterpart of [`Self::renderer`]. Same panic semantics.
    #[inline]
    #[track_caller]
    pub fn renderer_mut(&mut self) -> &mut GpuRenderer {
        self.renderer
            .as_mut()
            .expect("WindowState::renderer_mut() called before do_resumed populated it")
    }

    /// convenience that short-circuits when
    /// `window` is `None`. Most call sites previously did
    /// `ws.window.request_redraw()` unconditionally; after the
    /// `Option` promotion they want a no-op when the window is gone.
    #[inline]
    pub fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Change focus to a leaf in the active tab and return the feedback token.
    fn begin_pane_focus_change(&mut self, pane_id: u64) -> Option<PaneFocusChange> {
        let tab_idx = self.tabs.active_index();
        let tab = self.tab_states.get_mut(tab_idx)?;
        if tab.active_pane == pane_id || !tab.tree.leaves().contains(&pane_id) {
            // When: the target is already active or belongs to another tab, no
            // focus transition occurred and existing feedback must not restart.
            return None;
        }
        tab.active_pane = pane_id;
        Some(PaneFocusChange { pane_id })
    }

    /// Begin pointer focus and discard selection owned by the previous pane.
    fn begin_pointer_pane_focus_change(&mut self, pane_id: u64) -> Option<PaneFocusChange> {
        let change = self.begin_pane_focus_change(pane_id)?;
        self.selection = None;
        Some(change)
    }

    /// Present one validated pane-focus transition after related input work.
    fn finish_pane_focus_change(&mut self, change: PaneFocusChange) {
        mark_all_panes_dirty(&self.panes);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.flash_pane_focus(change.pane_id);
        }
        self.request_redraw();
    }

    /// Revoke any path authorization and remove pointer-owned target visuals.
    fn invalidate_path_hover(&mut self) {
        let changed =
            self.path_probe.invalidate() | self.hovered_url.take().is_some() | self.hover_link;
        self.hover_link = false;
        if changed {
            if let Some(window) = self.window.as_ref() {
                window.set_cursor(winit::window::CursorIcon::Default);
            }
            self.request_redraw();
        }
    }

    /// Record a left-press at grid cell `(row, col)` and return the
    /// resulting click count (1 = single, 2 = double, 3 = triple, then
    /// wraps back to 1). A press counts as a continuation of the previous
    /// streak when it lands on the *same* cell within
    /// [`MULTI_CLICK_MS`] of the previous press. Updates the
    /// `last_click_time` / `last_click_cell` / `click_count` fields in
    /// place. Pure counting logic lives in [`next_click_count`] so it can
    /// be unit-tested without a `WindowState`.
    pub fn register_click(&mut self, row: u16, col: u16) -> u8 {
        let now = Instant::now();
        let within_interval = self
            .last_click_time
            .map(|t| now.duration_since(t).as_millis() <= MULTI_CLICK_MS)
            .unwrap_or(false);
        let same_cell = self.last_click_cell == (row, col);
        let count = next_click_count(self.click_count, same_cell, within_interval);
        self.last_click_time = Some(now);
        self.last_click_cell = (row, col);
        self.click_count = count;
        count
    }

    /// Compute the selection for a multi-click `count` (1 = point, 2 =
    /// word, 3 = line) at grid `(row, col)` using THIS window's active
    /// pane grid. Locks that pane's parser only long enough to read the
    /// grid and build the (Copy) `Selection`, then drops it — so the
    /// caller never holds a grid lock across the selection assignment /
    /// redraw (CLAUDE.md §4). Falls back to a point selection when there
    /// is no active pane or the parser is busy. Used by the child-window
    /// mouse path; the main-window path has equivalent `App`-level
    /// helpers (`word_selection_at` / `line_selection_at`) that resolve
    /// the pane through `App::active_pane`.
    /// Convert a VIEWPORT row (0 = top visible row, from `pixel_to_cell`) to
    /// a scrollback-ABSOLUTE row for THIS window's active pane, so a
    /// `Selection` tracks the same TEXT as the viewport scrolls. Same
    /// `try_lock`-then-drop discipline as [`Self::multi_click_selection`]
    /// (CLAUDE.md §4). Returns `None` when the pane is missing or the parser
    /// is busy; the child-window mouse path then treats the viewport row as
    /// absolute (correct while unscrolled).
    pub fn viewport_row_selection_state(
        &self,
        viewport_row: u16,
    ) -> Option<(u64, u64, u64, bool, u64)> {
        let pane_id = self.tab_states.get(self.tabs.active_index()).map(|st| st.active_pane)?;
        let pane = self.panes.get(&pane_id)?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let state = (
            view_top + viewport_row as u64,
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        Some(state)
    }

    /// Absolute scrollback row under a viewport row of the active pane.
    ///
    /// Selections are anchored in absolute coordinates so they survive the
    /// viewport scrolling underneath them. `None` once the pane or its grid is
    /// unavailable.
    pub fn viewport_row_to_abs(&self, viewport_row: u16) -> Option<u64> {
        self.viewport_row_selection_state(viewport_row).map(|state| state.0)
    }

    /// Selection produced by a click streak: word at two, line at three.
    ///
    /// The result carries the grid's content state, so a later paste can tell
    /// whether the text it covers still stands. Any pane or lock failure
    /// degrades to a bare caret selection rather than a stale range.
    pub fn multi_click_selection(&self, count: u8, abs_row: u64, col: u16) -> Selection {
        let Some(pane_id) = self.tab_states.get(self.tabs.active_index()).map(|st| st.active_pane)
        else {
            // When: no `tab_states` entry backs the active index, so there is no
            // pane whose grid could widen the click into a word or line.
            return Selection::new(abs_row, col);
        };
        let Some(pane) = self.panes.get(&pane_id) else {
            // When: `pane_id` no longer resolves in `panes`, so the grid it named
            // is gone and only the caret position stays meaningful.
            return Selection::new(abs_row, col);
        };
        let Some(guard) = pane.parser.try_lock() else {
            // When: `try_lock` finds the parser busy with PTY output; yield rather
            // than block the input path, keeping the click a plain caret.
            return Selection::new(abs_row, col);
        };
        let grid = guard.grid();
        let sel = match count {
            2 => Selection::word_at(grid, abs_row, col),
            3 => Selection::line_at(grid, abs_row),
            _ => Selection::new(abs_row, col),
        }
        .with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        sel
    }

    /// Cell-mode drag for this window's active pane with an exact content fingerprint.
    pub fn cell_drag_selection(
        &self,
        anchor: (u64, u16),
        cursor_viewport_row: u16,
        col: u16,
    ) -> Option<Selection> {
        let pane_id = self.tab_states.get(self.tabs.active_index()).map(|st| st.active_pane)?;
        let pane = self.panes.get(&pane_id)?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let cursor_abs = view_top + u64::from(cursor_viewport_row);
        let mut selection = Selection::new(anchor.0, anchor.1);
        selection.extend(cursor_abs, col);
        let selection = selection
            .with_content_state(
                pane_id,
                grid.content_seq(),
                grid.is_alt(),
                grid.scrollback_evicted(),
            )
            .with_content_fingerprint(grid);
        drop(guard);
        Some(selection)
    }

    /// Word-mode drag for THIS window's active pane: union of the word at the
    /// scrollback-ABSOLUTE `anchor` cell and the word at the cursor cell.
    /// `cursor_viewport_row` is converted to an absolute row inside the same
    /// lock. Returns `None` when there is no active pane or the parser is
    /// busy, so the child-window mouse path SKIPS the move rather than
    /// shrinking an anchored word/line selection. Same `try_lock`-then-drop
    /// discipline as [`Self::multi_click_selection`] (CLAUDE.md §4).
    pub fn word_drag_selection(
        &self,
        anchor: (u64, u16),
        cursor_viewport_row: u16,
        col: u16,
    ) -> Option<Selection> {
        let pane_id = self.tab_states.get(self.tabs.active_index()).map(|st| st.active_pane)?;
        let pane = self.panes.get(&pane_id)?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::word_drag(grid, anchor, (cursor_abs, col)).with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        Some(sel)
    }

    /// Line-mode drag for THIS window's active pane: whole rows from the
    /// scrollback-ABSOLUTE `anchor_row` to the cursor row inclusive.
    /// `cursor_viewport_row` is converted to an absolute row inside the lock.
    /// Returns `None` when the pane is missing or the parser is busy (see
    /// [`Self::word_drag_selection`]).
    pub fn line_drag_selection(
        &self,
        anchor_row: u64,
        cursor_viewport_row: u16,
    ) -> Option<Selection> {
        let pane_id = self.tab_states.get(self.tabs.active_index()).map(|st| st.active_pane)?;
        let pane = self.panes.get(&pane_id)?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::line_drag(grid, anchor_row, cursor_abs).with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        drop(guard);
        Some(sel)
    }

    /// Clear the drag-chip overlay through a single call site.
    ///
    /// Two things represent the chip: the renderer's persistent overlay drawn
    /// by the per-frame emitter, and the headless-test marker
    /// (`test_drag_chip_marker`). Clearing them from separate statements lets a
    /// later refactor split them, leaving the regression test green while
    /// production keeps drawing the chip. Both clears live here so every caller
    /// flips them in lock-step.
    ///
    /// **Contract:** this helper is
    /// **tolerant** — it is safe to call on a `WindowState` whose
    /// `renderer` is `None` (e.g. a transitional window that hasn't
    /// finished initialization yet, or a headless test window) AND on
    /// a window whose `test_drag_chip_marker` is `None`. Both branches
    /// short-circuit cleanly. This matters because the deferred
    /// `pending_os_teardown` drain (see [`App::cancel_drag_session`]
    /// and `App::drain_pending_os_teardown`) iterates a snapshot of
    /// `self.windows.keys()`, and a tear-out spawn that just landed
    /// may have produced a `WindowState` whose renderer
    /// is still being constructed. Both fields are flipped together —
    /// callers MUST NOT split them, or the headless-test lock-step
    /// guarantee in `tests/os_drag_cleanup.rs` regresses.
    #[inline]
    pub(crate) fn clear_drag_chip(&mut self) {
        if let Some(r) = self.renderer.as_mut() {
            r.set_drag_chip(None);
        }
        if let Some(marker) = self.test_drag_chip_marker.as_mut() {
            *marker = false;
        }
    }

    /// — intra-window tab reorder that keeps `tabs` and
    /// `tab_states` in lock-step. Extracted from `window_event.rs`'s
    /// main-window `ReorderTab` branch so the production path and the
    /// regression tests exercise the SAME code.
    ///
    /// Semantics match `tab_transfer::reorder_within`:
    /// - `from` out of range → no-op.
    /// - `to` clamped to `len - 1` (— drop-past-last must land at
    ///   the end, not silently no-op like `TabBar::reorder` does).
    /// - `to == from` after clamp → no-op.
    /// - Otherwise: `tabs.reorder(from, to)` AND
    ///   `tab_states.remove(from) → insert(to)` so the title's TabState
    ///   (active pane id + PaneTree leaf-ids) travels WITH the title.
    ///
    /// Returns `true` if any mutation happened.
    pub fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        let len = self.tabs.len();
        if from >= len || len == 0 {
            // When: `from` names no live tab, so there is nothing to move and the
            // caller must not be told the order changed.
            return false;
        }
        let last = len - 1;
        let to = to.min(last);
        if to == from {
            // When: clamping `to` landed it back on `from`, so the drop target is
            // the tab's existing slot and reordering would be a no-op.
            return false;
        }
        self.tabs.reorder(from, to);
        if from < self.tab_states.len() && to < self.tab_states.len() {
            let st = self.tab_states.remove(from);
            self.tab_states.insert(to, st);
        }
        true
    }
}

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SYNTHETIC_CHILD_WINDOW_TAG: AtomicU64 = AtomicU64::new(1);

// Ordering: `NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add` uses `Relaxed`; only the
// uniqueness of each returned tag matters, never its order against other writes.
fn next_synthetic_child_window_id() -> WindowId {
    let tag = NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add(1, Ordering::Relaxed);
    WindowId::from(u64::MAX - tag)
}

/// Stable synthetic `WindowId` addressing the main window entry without a live
/// winit window.
///
/// Lets a test seed the main entry in the window map directly. `u64::MAX` is
/// collision-free because real OS window ids never reach it. Production never
/// constructs this id: window creation uses the real `window.id()` and clears
/// any pre-existing synthetic entry first.
#[doc(hidden)]
pub fn synthetic_main_window_id() -> WindowId {
    WindowId::from(u64::MAX)
}

/// Which terminal window currently owns the OS-frontmost focus.
///
/// Keymap dispatch and the menubar drain consume this to decide where a chord
/// like Cmd+T / Cmd+W / Cmd+\\ should land.
///
/// `Other` covers any non-terminal SonicTerm window; it explicitly does NOT
/// route terminal actions and falls back to main as a safe default.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmostKind {
    /// No window has focus, or recorded id is stale.
    None,
    /// Main terminal window is OS-frontmost.
    Main,
    /// A torn-out child terminal window is OS-frontmost. Carries the
    /// window id so the caller can index `windows`.
    Child(WindowId),
    /// A non-terminal SonicTerm window is frontmost. Terminal actions fall
    /// back to main.
    Other,
}

/// Read a window's screen-global inner origin + inner size into the
/// pure helper struct used by the drag-merge module. Falls back to
/// (0, 0) origin if the platform refuses to report position (e.g. on
/// some Wayland configurations); on such platforms the drag-merge
/// path is best-effort.
/// Screen-global inner origin and inner size, as the drag-merge module's
/// pure geometry struct.
///
/// A platform that refuses to report position reports a `(0, 0)` origin, which
/// leaves drag-merge best-effort there rather than failing the drag outright.
pub(super) fn window_geom(w: &Window) -> crate::tab_drag::WindowGeom {
    let origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
    let size = w.inner_size();
    crate::tab_drag::WindowGeom { inner_origin: origin, inner_size: (size.width, size.height) }
}

/// This window's scale factor, as the `f32` the geometry helpers expect.
#[inline]
pub(super) fn window_dpi(w: &Window) -> f32 {
    w.scale_factor() as f32
}

/// Allocate the next process-unique pane id.
// Ordering: `NEXT_PANE_ID.fetch_add` uses `Relaxed`; each caller needs a distinct
// id, and no other memory is published through this counter.
#[doc(hidden)]
pub fn next_pane_id() -> u64 {
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Wrap clipboard text for paste, applying DECSET 2004 bracketed-paste
/// guards (`ESC [ 200 ~` / `ESC [ 201 ~`) when the active pane has
/// requested bracketed paste. Pure function, exported for unit tests.
pub fn wrap_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut v = Vec::with_capacity(text.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(text.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        // When: `bracketed` is unset, so the guards would reach the shell as
        // literal escape bytes rather than being consumed as markers.
        text.as_bytes().to_vec()
    }
}

/// Quote a single path or word for POSIX-shell paste. Re-exported from the
/// shared `sonicterm-types` implementation so file drops on macOS and Windows
/// paste the same bytes. Kept at this path so existing `super::shell_quote_posix`
/// imports continue to resolve.
pub use sonicterm_types::shell_quote_posix;

/// Compute the absolute viewport-top row for "scroll to previous / next
/// prompt". Returns `None` if there is no prompt in the requested
/// direction. Pure function so tests can drive it without a window.
pub fn pick_prompt_target(
    grid: &sonicterm_grid::grid::Grid,
    current_top_abs: u64,
    forward: bool,
) -> Option<u64> {
    let pick = if forward {
        grid.prompt_after(current_top_abs)
    } else {
        // When: `forward` is unset, so the search runs backward from
        // `current_top_abs` toward older scrollback instead of newer output.
        grid.prompt_before(current_top_abs)
    };
    pick.map(|p| p.start_row)
}

/// Seed a freshly-created parser with the active theme's query-reply colours:
/// default fg/bg/cursor (OSC 10/11/12 `?`) AND the full 16-colour ANSI palette
/// (OSC 4 `?`). Centralizes what used to be duplicated at every pane-spawn site
/// so the OSC 4 palette wiring can't be added to one path and forgotten
/// on another. Per-slot colours that don't resolve are simply left unseeded
/// (the parser then suppresses that slot's reply rather than lying).
pub fn seed_parser_theme_colors(parser: &mut sonicterm_vt::vt::Parser, theme: &Theme) {
    if let Some((r, g, b)) = theme.colors.foreground.rgb() {
        parser.set_theme_fg(r, g, b);
    }
    if let Some((r, g, b)) = theme.colors.background.rgb() {
        parser.set_theme_bg(r, g, b);
    }
    if let Some((r, g, b)) = theme.colors.cursor.rgb() {
        parser.set_theme_cursor(r, g, b);
    }
    // OSC 4 palette: indices 0..=7 from `ansi.*`, 8..=15 from `bright.*`,
    // in the standard xterm slot order.
    let normal = [
        &theme.colors.ansi.black,
        &theme.colors.ansi.red,
        &theme.colors.ansi.green,
        &theme.colors.ansi.yellow,
        &theme.colors.ansi.blue,
        &theme.colors.ansi.magenta,
        &theme.colors.ansi.cyan,
        &theme.colors.ansi.white,
    ];
    let bright = [
        &theme.colors.bright.black,
        &theme.colors.bright.red,
        &theme.colors.bright.green,
        &theme.colors.bright.yellow,
        &theme.colors.bright.blue,
        &theme.colors.bright.magenta,
        &theme.colors.bright.cyan,
        &theme.colors.bright.white,
    ];
    for (i, hex) in normal.iter().chain(bright.iter()).enumerate() {
        if let Some((r, g, b)) = hex.rgb() {
            parser.set_theme_palette_color(i as u8, r, g, b);
        }
    }
}

/// Resize every pane in `panes` to `(cols, rows)`: both the parser's
/// grid and (if the pane owns one) the PTY child. Used by the window
/// resize handler and by the font live-reload path, where changing
/// cell metrics shifts how many cells fit inside the current window.
///
/// `pub` + `#[doc(hidden)]` so integration tests can exercise the
/// invariant on a synthetic pane map without needing a live wgpu
/// surface or a real shell.
#[doc(hidden)]
pub fn resize_all_panes(panes: &HashMap<u64, PaneState>, cols: u16, rows: u16) {
    for pane in panes.values() {
        pane.parser.lock().grid_mut().resize(cols, rows);
        if let Some(pty) = pane.pty.as_ref() {
            (pty.resize)(cols, rows);
        }
    }
}

/// Resize each pane in `panes` to the cells that fit inside its own
/// `sonicterm_ui::pane::Rect` (window-pixel logical rect produced by
/// `PaneTree::layout`). `cell_w` / `cell_h` are the logical cell metrics
/// from the renderer (`Renderer::cell_size()`).
///
/// This is the per-pane sizing counterpart to [`resize_all_panes`]: the
/// older helper sized every pane to the same whole-window `(cols, rows)`,
/// which is wrong as soon as a tab has more than one pane (an inactive
/// pane's grid then thinks it has more columns than it actually shows,
/// so TUIs like vim/htop draw past their visible border and the wrap
/// column is wrong on resize).
///
/// CLAUDE.md §4: uses `parser.lock()` (NOT `try_lock`) — same as
/// `resize_all_panes`. Call sites are app-thread (WindowEvent::Resized
/// and config-live-reload), not the render hot path, so the lock is
/// safe and a dropped resize would leave the grid wrong-sized for the
/// next burst of pty output.
///
/// `rects` whose `id` is missing from `panes` are silently skipped
/// (covers the brief window during tab close where the layout list
/// includes a pane that was just removed).
///
pub fn resize_panes_to_rects(
    panes: &HashMap<u64, PaneState>,
    rects: &[(u64, sonicterm_ui::pane::Rect)],
    cell_w: f32,
    cell_h: f32,
    content_inset: [f32; 4],
) {
    let [left, right, top, bottom] = content_inset;
    for (id, rect) in rects {
        let Some(pane) = panes.get(id) else {
            // When: `id` names a pane already removed from `panes` — the layout
            // list still carries it mid tab-close — so skip rather than resize it.
            continue;
        };
        let content_w = (rect.w - left - right).max(cell_w);
        let content_h = (rect.h - top - bottom).max(cell_h);
        let (cols, rows) = sonicterm_grid::grid::bounded_grid_size(
            (content_w / cell_w).floor() as u64,
            (content_h / cell_h).floor() as u64,
        );
        pane.parser.lock().grid_mut().resize(cols, rows);
        if let Some(pty) = pane.pty.as_ref() {
            (pty.resize)(cols, rows);
        }
    }
}

/// Return the pane whose half-open rectangle contains `(x, y)`.
fn pane_id_at_point(rects: &[(u64, sonicterm_ui::pane::Rect)], x: f32, y: f32) -> Option<u64> {
    rects.iter().find_map(|(id, rect)| rect.contains(x, y).then_some(*id))
}

/// Mark every pane's grid fully dirty. Used by triggers that change
/// the renderer's *presentation* invariant without mutating any cell
/// content (theme swap, font swap, focus transition, selection change).
/// This is the foundation hook the upcoming RowCache will use to know
/// when its cached row data is stale even though grid revision did not
/// bump.
///
/// `pub` + `#[doc(hidden)]` so integration tests can exercise the
/// invariant on a synthetic pane map.
#[doc(hidden)]
pub fn mark_all_panes_dirty(panes: &HashMap<u64, PaneState>) {
    for pane in panes.values() {
        pane.parser.lock().grid_mut().mark_all_dirty();
    }
}

/// Revalidate the authoritative selection and rebase its active drag anchor.
///
/// Callers already hold the active pane's parser guard during frame assembly;
/// accepting `&Grid` avoids re-locking that parser and the AB-BA deadlock such a
/// re-lock would cause. Search and copy-mode overlays own separate state and are
/// deliberately untouched.
#[doc(hidden)]
pub fn invalidate_selection_for_content(
    selection: &mut Option<Selection>,
    select_anchor: &mut (u64, u16),
    pane_id: u64,
    grid: &Grid,
) -> bool {
    let (anchor_belongs_to_selection, previous_evicted) =
        selection.as_ref().map_or((false, grid.scrollback_evicted()), |selection| {
            (selection.contains(select_anchor.0, select_anchor.1), selection.scrollback_evicted)
        });
    let should_clear = selection.as_mut().is_some_and(|selection| {
        sonicterm_ui::selection::revalidate_selection(selection, pane_id, grid)
    });
    if should_clear {
        *selection = None;
    } else if anchor_belongs_to_selection {
        // When: `anchor_belongs_to_selection` is true, apply the selection endpoints' scrollback rebase to its drag anchor.
        let rebased_rows = selection
            .as_ref()
            .map_or(0, |selection| selection.scrollback_evicted.saturating_sub(previous_evicted));
        select_anchor.0 = select_anchor.0.saturating_sub(rebased_rows);
    }
    should_clear
}

const FOREGROUND_PROCESS_TTL: std::time::Duration = std::time::Duration::from_millis(500);

fn pane_foreground_cache_is_fresh(pane: &PaneState, now: Instant) -> bool {
    pane.fg_proc_cache
        .as_ref()
        .is_some_and(|(sampled, _)| now.duration_since(*sampled) < FOREGROUND_PROCESS_TTL)
}

fn cached_foreground_privileged(pane: &PaneState) -> bool {
    pane.fg_proc_cache
        .as_ref()
        .and_then(|(_, process)| process.as_ref())
        .is_some_and(|process| process.privileged)
}

fn refresh_tab_foreground_privilege(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    pane: &mut PaneState,
    tab_idx: usize,
    allow_proc_probe: bool,
) {
    let now = Instant::now();
    if !pane_foreground_cache_is_fresh(pane, now) && allow_proc_probe {
        let probed = pane
            .pty
            .as_ref()
            .and_then(|pty| pty.pid())
            .and_then(sonicterm_io::proc_info::foreground_process_info);
        pane.fg_proc_cache = Some((now, probed));
    }
    tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
}

fn refresh_window_tab_privileges_at(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    allow_proc_probe: bool,
    force_proc_probe: bool,
    now: Instant,
) -> bool {
    #[cfg(windows)]
    {
        let mut changed = false;
        let mut stale = Vec::new();
        for (tab_idx, tab_state) in tab_states.iter().enumerate() {
            let Some(pane) = panes.get_mut(&tab_state.active_pane) else {
                // When: the tab's active pane no longer exists, clear any warning retained by its tab.
                changed |= tabs.set_foreground_privileged(tab_idx, false);
                continue;
            };
            if allow_proc_probe && (force_proc_probe || !pane_foreground_cache_is_fresh(pane, now))
            {
                // When: this pane's cache is stale or a deadline forces a sample, include it in the shared snapshot.
                if let Some(pid) = pane.pty.as_ref().and_then(|pty| pty.pid()) {
                    stale.push((tab_idx, tab_state.active_pane, pid));
                } else {
                    changed |=
                        pane.fg_proc_cache.as_ref().is_none_or(|(_, process)| process.is_some());
                    pane.fg_proc_cache = Some((now, None));
                }
            }
            changed |= tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
        }

        let pids = stale.iter().map(|(_, _, pid)| *pid).collect::<Vec<_>>();
        let observations = sonicterm_io::proc_info::foreground_processes_info(&pids);
        for ((tab_idx, pane_id, _), observation) in stale.into_iter().zip(observations) {
            let Some(pane) = panes.get_mut(&pane_id) else {
                // When: the pane vanished after collection, its tab must not retain the old warning.
                changed |= tabs.set_foreground_privileged(tab_idx, false);
                continue;
            };
            changed |=
                pane.fg_proc_cache.as_ref().is_none_or(|(_, process)| process != &observation);
            pane.fg_proc_cache = Some((now, observation));
            changed |= tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
        }
        changed
    }

    #[cfg(not(windows))]
    {
        let _ = (tabs, tab_states, panes, allow_proc_probe, force_proc_probe, now);
        false
    }
}

pub(super) fn refresh_window_tab_privileges(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    allow_proc_probe: bool,
) -> bool {
    refresh_window_tab_privileges_at(
        tabs,
        tab_states,
        panes,
        allow_proc_probe,
        false,
        Instant::now(),
    )
}

#[cfg(windows)]
fn force_refresh_window_tab_privileges(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    now: Instant,
) -> bool {
    refresh_window_tab_privileges_at(tabs, tab_states, panes, true, true, now)
}

/// Compute the wezterm-style pretty tab title for the active pane and
/// (if it differs from the current `TabBar` active title) apply it via
/// `set_active_title`. Returns the title actually applied, or `None` if
/// no change was needed.
///
/// Refactored out of `app/window_event.rs` so the equivalent code path
/// in `app/child_window.rs` (Cmd+N / tear-out windows) can share the
/// same logic — otherwise child windows fall back to the literal
/// "shell N" placeholder set at spawn time.
pub fn refresh_active_tab_title(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    pane: &mut PaneState,
    parser: &Parser,
    tab_idx: usize,
    allow_proc_probe: bool,
) -> Option<String> {
    let cwd = parser.cwd().map(str::to_string);
    let raw_title = parser.title().map(str::to_string);
    refresh_tab_foreground_privilege(tabs, pane, tab_idx, allow_proc_probe);
    let proc_name = pane
        .fg_proc_cache
        .as_ref()
        .and_then(|(_, process)| process.as_ref().map(|process| process.name.clone()));
    let auto_title = sonicterm_ui::tab_title::format_tab_title(
        tab_idx,
        cwd.as_deref(),
        proc_name.as_deref(),
        raw_title.as_deref(),
    );
    let effective_title = tabs
        .active()
        .and_then(|tab| tab.custom_title.as_ref())
        .map(|custom| sonicterm_ui::tabs::title_with_replaced_body(&auto_title, custom))
        .unwrap_or_else(|| auto_title.clone());
    let cur = tabs.active().map(|t| t.title.clone());
    if cur.as_deref() == Some(effective_title.as_str()) {
        // When: the shown title already equals `effective_title`, so nothing needs
        // repainting; only the stored auto base may still have drifted underneath.
        if tabs.active().is_some_and(|tab| tab.auto_title != auto_title) {
            tabs.set_active_title(auto_title);
        }
        return None;
    }
    tabs.set_active_title(auto_title);
    Some(effective_title)
}

/// Loader callback type used by the platform shell to reload a theme by name.
pub type ThemeLoader = Box<dyn Fn(&str) -> Result<Theme> + Send + 'static>;
/// Loader callback type used by the platform shell to reload a resolved keymap path.
pub type KeymapLoader = Box<dyn Fn(&Path) -> Result<Keymap> + Send + 'static>;

/// Custom user events delivered through [`EventLoopProxy`].
///
/// Config is read at startup and thereafter only when the user explicitly
/// asks for it via `Action::ReloadConfig`, so there is no watcher thread and
/// no event for "the config file changed on disk".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A pending action arrived from the macOS native menubar. The
    /// payload itself is queued in the static
    /// [`crate::menubar_bridge`] buffer; this variant is just the
    /// wake-up signal so the loop drains it.
    MenuAction,
    /// Script-file open requests were queued by a platform boundary.
    OpenScripts,
    /// A platform OS-drag drop landed and stashed payloads in
    /// [`crate::os_drag_bridge`]. The variant is just the wake-up
    /// signal so the loop drains the queues — separate from
    /// [`Self::MenuAction`] so a noisy drag stream does not flood the
    /// menubar drain path.
    OsDrag,
    /// A platform drag backend reported a cursor move. Windows OLE can produce
    /// these during a native session; the current macOS pasteboard backend does
    /// not. The
    /// actual position is in the [`os_drag::PendingDragOutcome`]
    /// mailbox shared with the backend.
    DragMoved,
    /// A platform drag backend terminated (drop or cancel). The outcome
    /// (drop target, tear-out, or cancel) is in
    /// the [`os_drag::PendingDragOutcome`] mailbox; the dispatcher
    /// inspects it and routes to `App::transfer_tab` or
    /// `App::cancel_drag_session` accordingly.
    DragEnded,
    /// A VT worker coalesced terminal output for this window. The event-loop
    /// thread resolves the live window and requests its redraw; VT workers never
    /// call native window APIs directly.
    RequestRedraw(WindowId),
    /// A previously-deferred font fallback family finished loading in the
    /// `sonicterm_text::async_fallback` background thread. The handler walks
    /// every live window's `GpuRenderer`, calls `clear_shape_cache()` (which
    /// bumps `style_rev` and drops the shape / row / line caches), and issues
    /// `window.request_redraw()` so the next frame re-shapes through the newly
    /// available face and the user's tofu cells get replaced by real glyphs.
    ClearShapeCache,
    /// Background update check finished; show a reusable notification bubble.
    UpdateCheckFinished { level: NotificationLevel, message: String },
    /// A pane's child process ended, and its output channel closed with it.
    ///
    /// Raised once by that pane's VT worker, which classifies the exit before
    /// posting: the child becoming reapable and its pty reaching EOF are
    /// unordered, so the answer needs a bounded wait that must not happen on
    /// the event-loop thread.
    PaneProcessExited {
        /// The pane whose child ended.
        pane_id: u64,
        /// Whether that child exited cleanly, or `None` if it could not be
        /// determined. `None` is not a crash — it holds the pane open, the
        /// same as an unclean exit.
        was_clean: Option<bool>,
    },
    /// A script path could not be represented safely for the active shell.
    ScriptDraftRejected {
        /// User-facing explanation of why no draft was inserted.
        message: String,
    },
    /// A validated OSC 52 clipboard write reached the event-loop thread.
    ///
    /// The VT worker decodes and bounds the payload before constructing this
    /// event; native clipboard access remains confined to the app thread.
    ClipboardWrite {
        /// UTF-8 text requested by the terminal application.
        text: String,
    },
    /// A local-target openability probe completed off the event-loop thread.
    PathProbeFinished(Box<path_target::PathProbeResult>),
    /// A bounded PTY input enqueue failed. Retains the rejected bytes until
    /// the event-loop thread can show a user-actionable notification.
    PtyInputRejected {
        /// Original terminal input that was not sent.
        bytes: Vec<u8>,
        /// Human-readable rejection reason.
        reason: String,
    },
    /// The bounded Linux package-smoke watchdog expired.
    RuntimeSmokeTimeout,
}

fn pty_input_rejected_event(error: sonicterm_io::pty::PtyInputError) -> UserEvent {
    let reason = error.to_string();
    let bytes = error.into_bytes();
    UserEvent::PtyInputRejected { bytes, reason }
}

/// Build an async fallback loader whose notifier fires
/// `UserEvent::ClearShapeCache` on `proxy`. The loader uses
/// `sonicterm_text::async_fallback::default_load_font_family` for actual
/// font resolution (zero-byte handle for OS-resident faces, which is
/// what we want — cosmic-text's `FontSystem` does the real install on
/// first use).
///
/// This is the production wire for the async fallback loader. Every
/// `GpuRenderer::new` site in `sonicterm-app` constructs the loader from its
/// event-loop proxy and hands it to `GpuRenderer::set_async_loader`. From that
/// point on, a background font load completion bumps `style_rev` on every live
/// window and triggers a redraw — the tofu cells flip to real
/// glyphs without the user having to type anything.
/// The legacy `AsyncFallbackLoader` (cosmic-text/swash
/// driven background-load helper) is gone with the rest of the
/// glyphon plumbing. sonicterm-font handles CJK/emoji/Nerd-font
/// fallback synchronously via its built-in vendor chain
/// (`vendor-jetbrains`, `vendor-noto-emoji`, `vendor-nerd-font-symbols`),
/// so the per-window `set_async_loader(...)` plumbing is now a no-op
/// `()`. Keeping the function shape and call site survives so the
/// renderer's `Option<()>` slot stays populated and any future
/// re-introduction of an async hook lands without breaking callers.
pub fn build_async_fallback_loader_for_proxy(_proxy: EventLoopProxy<UserEvent>) {}

mod child_window;
pub use child_window::{
    apply_dpi_to_renderer_if_present, child_window_dpi_changed_handles_no_renderer,
    child_window_resized_handles_no_renderer, resize_renderer_and_panes_if_present,
};
mod config_apply;
mod event_loop;
pub mod hovered_url;
pub mod invariants;
mod key_encoding;
mod keymap_dispatch;
mod media;
pub mod memory_snapshot;
mod misc;
pub mod os_drag;
mod overlays;
mod pane_exit;
mod pane_launch;
mod path_target;
mod quit_hold;
mod redraw_target;
mod runtime_smoke;
pub use runtime_smoke::RuntimeSmokeFailure;
mod render_timing;
pub mod renderer_retention;
pub mod retention;
mod scroll;
pub mod scrollbar_input;
pub mod scrollbar_visibility;
mod search_handle;
mod spawn_pane;
mod tab_state;
pub mod tab_transfer;
mod tear_out;
mod text_edit;
#[doc(hidden)]
pub mod update_check;
mod window_event;
pub use config_apply::{
    config_diff_needs_font_apply, renderer_scrollbar_mode_differs,
    renderer_subpixel_aa_mode_differs,
};
pub use key_encoding::{encode_logical, key_name, key_to_string, key_to_strings, KeyName};

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

/// Public wrapper over the crate's `init_tracing` for the platform shell.
///
/// Installs the subscriber idempotently through `try_init`, so a process that
/// already has one keeps it rather than failing or installing a second.
pub fn init_tracing_public() {
    init_tracing();
}

/// Per-pane runtime state. The parser is shared with a per-pane VT thread
/// that drains the pty out-channel; the pty handle owns the writer side.
///
/// `redraw_target` identifies the window that owns the pane. The main thread
/// swaps the `WindowId` when a pane migrates to a torn-out child; VT workers
/// read the current id after coalescing and send a typed redraw event. Native
/// window APIs remain confined to the winit event-loop thread.
pub struct PaneState {
    /// Governor charges this pane holds, one per resource class.
    ///
    /// Committed reservations rather than repeated reserve/release: a pane's
    /// retention rises and falls continuously, and a release/re-reserve pair
    /// on every sample opens a window where the ledger disagrees with reality.
    /// `try_grow` and `shrink` move a live charge in place, so the figure is
    /// never briefly wrong.
    ///
    /// Released by `Drop` when the pane is dropped, which is the same property
    /// that made the inline-media charge correct: there is no teardown site to
    /// forget.
    pub(crate) charges: HashMap<ResourceClass, sonicterm_resource::CommittedReservation>,
    /// This pane's owner in the governor hierarchy, below its window's.
    ///
    /// Assigned when the pane is inserted into a window rather than at
    /// construction: `PaneState::new` is called from a dozen sites that have
    /// no governor in scope, and threading one through all of them would be a
    /// larger change than the ownership it establishes.
    ///
    /// Held as an [`OwnerGuard`] so the owner closes when the pane drops.
    /// Declared *after* `charges` deliberately: Rust drops fields in
    /// declaration order, and `finish_close` refuses an owner that still holds
    /// charges, so the reservations must release first.
    pub(crate) owner: Option<OwnerGuard>,
    pub parser: Arc<Mutex<Parser>>,
    /// Capture progress seen at the previous retention sample.
    ///
    /// A media capture holds its staging buffer until its terminator arrives,
    /// and the terminator is not guaranteed to — a killed transfer or dropped
    /// link leaves it pinned until the pane dies. The parser cannot tell that
    /// from a slow transfer, having no clock. The sampler has one, so it
    /// remembers what the capture had received last time and cancels only a
    /// capture that has not moved across consecutive samples.
    pub(crate) last_capture_progress: Option<usize>,
    /// How many consecutive samples have seen `last_capture_progress`
    /// unchanged.
    ///
    /// A count rather than a flag because one unchanged reading proves only
    /// one sample interval of silence, and a transfer merely slower than that
    /// interval reads as stalled — cancelling it costs the user a picture they
    /// were waiting for. Requiring the figure to hold still twice buys a
    /// second interval of evidence, so the threshold is the full
    /// `2 × RETENTION_SAMPLE_INTERVAL` the cancellation reports.
    pub(crate) capture_stall_samples: u8,
    pub pty: Option<PtyHandle>,
    pub redraw_target: Arc<Mutex<Option<WindowId>>>,
    /// Absolute row (scrollback-relative) that should appear at the top of
    /// the visible viewport. `None` = "follow the live tail" (default).
    /// Currently set by the OSC 133 prompt-navigation actions. The render
    /// layer treats this as a hint — the grid itself always exposes the
    /// live visible window.
    pub viewport_top_abs: Option<u64>,
    /// Cached foreground-process identity/privilege plus the last probe time.
    ///
    /// The probe walks the whole process table, so it must not run on every
    /// render. The 500 ms title-refresh TTL keeps names and Windows elevation
    /// responsive without reviving the measured idle CPU regression.
    pub fg_proc_cache:
        Option<(std::time::Instant, Option<sonicterm_io::proc_info::ForegroundProcess>)>,
    /// Cross-thread queue populated by the VT loop when OSC 133 command
    /// lifecycle markers are parsed for this pane.
    pub command_events: Arc<Mutex<Vec<PaneCommandEvent>>>,
    /// Per-pane DECTCEM cursor-visibility flag (`CSI ?25h/l`). Written
    /// by the VT loop, read by the render path for the active pane.
    /// **Per-pane (not per-window)** so the Arc travels with the pane
    /// when a tab is torn out into a new window — pre-fix the Arc
    /// lived on `WindowState`, so tear-out's destination got a fresh
    /// Arc and the moved pane's VT thread kept writing to an orphaned
    /// AtomicBool that nobody read. Init `true`.
    pub cursor_visible: Arc<std::sync::atomic::AtomicBool>,
    /// Per-pane kitty-keyboard progressive-enhancement flags (`CSI ?u`),
    /// mirrored out of the parser by the VT loop after each parse batch.
    /// The keypress path reads this lock-free instead of taking
    /// `parser.lock()` before every PTY write — that lock is held by the VT
    /// thread while parsing output, so blocking on it added input latency
    /// whenever output was streaming. Init 0 (legacy encoding).
    pub kitty_flags: Arc<std::sync::atomic::AtomicU8>,
    /// Per-pane DECCKM (?1, application cursor keys) snapshot, mirrored out
    /// of the parser by the VT loop so the keypress path reads it lock-free
    /// (same rationale as `kitty_flags`,). When `true`, arrows /
    /// Home / End encode with the SS3 introducer (`ESC O x`) instead of CSI
    /// so terminfo-driven apps (zsh ZLE, readline, vim, less) recognize them
    /// . Init `false` (normal cursor keys → CSI).
    pub app_cursor_keys: Arc<std::sync::atomic::AtomicBool>,
    /// Decoded inline media images captured from terminal protocols.
    pub inline_images: Arc<Mutex<Vec<sonicterm_render_model::InlineImage>>>,
    /// This pane's share of the process-wide inline-media total.
    ///
    /// Co-owned with the pane's VT worker. The worker ends when its shell
    /// exits, but the pane stays on screen with its images, so a charge held
    /// only by the worker would be released while the pixels are still
    /// retained. Held here so the charge is returned when the pane — and with
    /// it the image store — is actually dropped.
    pub(crate) inline_media_charge: media::SharedInlineMediaCharge,
}

#[derive(Debug, Clone)]
pub struct PaneCommandEvent {
    pub event: CommandEvent,
    pub at: Instant,
    pub duration: Option<Duration>,
}

impl PaneState {
    /// Build a pane around an existing parser and optional PTY.
    ///
    /// The governor owner is left unset here and assigned when the pane is
    /// inserted into a window, so a pane that is built but never inserted
    /// registers no owner to close.
    #[doc(hidden)]
    pub fn new(parser: Arc<Mutex<Parser>>, pty: Option<PtyHandle>) -> Self {
        Self {
            // Assigned when the pane is inserted into a window.
            owner: None,
            charges: HashMap::new(),
            parser,
            last_capture_progress: None,
            capture_stall_samples: 0,
            pty,
            redraw_target: Arc::new(Mutex::new(None)),
            viewport_top_abs: None,
            fg_proc_cache: None,
            command_events: Arc::new(Mutex::new(Vec::new())),
            cursor_visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            kitty_flags: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            app_cursor_keys: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            inline_images: Arc::new(Mutex::new(Vec::new())),
            inline_media_charge: media::new_inline_media_charge(),
        }
    }
}

/// Per-tab state. The `TabBar` keeps title/order; this struct tracks the
/// pane tree and the focused leaf inside the tab.
pub struct TabState {
    pub tree: PaneTree,
    pub active_pane: u64,
    pub search: Option<SearchState>,
    pub command: CommandStatus,
}

impl TabState {
    /// Build a tab around a pane tree, focused on `active_pane`.
    ///
    /// The tab opens with no search session and an idle command status, so a
    /// freshly created tab reports nothing running until its shell says so.
    #[doc(hidden)]
    pub fn new(tree: PaneTree, active_pane: u64) -> Self {
        Self { tree, active_pane, search: None, command: CommandStatus::Idle }
    }
}

/// Deferred in-process tab tear-out request. Drag tear-out records a screen
/// position; command-palette/keymap tear-out leaves it unset so the window
/// manager chooses the destination position.
#[derive(Debug, Clone)]
pub struct PendingTearOut {
    pub source_window: WindowId,
    pub source_tab_idx: usize,
    /// The tab this request names, independent of where it currently sits.
    ///
    /// An index is a position, and positions move: a tab closing at a lower
    /// index leaves the recorded one in range but naming a different tab, so a
    /// bounds check passes and the wrong tab is torn out. That became reachable
    /// once a shell exiting could close a tab on its own, with no user action
    /// to serialise against the drag.
    ///
    /// `None` only for requests built before an id was available, which fall
    /// back to the index.
    pub source_tab_id: Option<sonicterm_ui::tabs::TabId>,
    pub drop_screen_pos: Option<(i32, i32)>,
}

/// Delay allowing a failing Windows clipboard helper to release its open handle.
#[cfg(target_os = "windows")]
const OSC52_CLIPBOARD_REASSERT_DELAY: Duration = Duration::from_millis(150);

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub(super) struct PendingOsc52Reassert {
    /// Clipboard text to restore after the helper releases the clipboard.
    text: String,
    /// Clipboard value observed before the OSC write.
    previous_text: Option<String>,
    /// Event-loop deadline for the one permitted reassertion.
    due: Instant,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
struct PendingForegroundProbe {
    /// Earliest instant at which the foreground process must be sampled again.
    due: Instant,
    /// Whether output activity is forbidden from postponing this deadline.
    fixed: bool,
}

#[doc(hidden)]
pub struct App {
    pub(super) theme: Theme,
    /// Process privilege observed once by the native binary before window creation.
    pub(super) process_privilege: crate::ProcessPrivilege,
    #[cfg(windows)]
    /// Bounded foreground-process sample armed by accepted input or quiet output.
    foreground_probe_wake: Option<PendingForegroundProbe>,
    pub(super) config: Config,
    /// Packaged font directories retained for every renderer and live font rebuild.
    pub(super) font_dirs: Vec<PathBuf>,
    /// Font size the loaded config asked for, in logical px. `ResetFontSize`
    /// returns here rather than to the compile-time default, so Cmd+0 restores
    /// the user's configured size instead of a value they never chose.
    ///
    /// Tracks the config this session has *loaded*, not the file on disk.
    /// Editing `sonicterm.toml` does not move it — the background watcher may
    /// apply other settings from that edit, but the reset target stays where
    /// the session started. Only an explicit `ReloadConfig` moves it.
    pub(super) configured_font_size: f32,
    /// Regular-text `weight_scale` the loaded config asked for. Follows the
    /// same rule as [`Self::configured_font_size`]: `ResetFontWeight` returns
    /// here, and only an explicit reload moves it.
    pub(super) configured_weight_scale: f32,
    pub(super) keymap: Keymap,
    // The main window holds no state of its own here. Its renderer, tabs,
    // tab states, panes, selection, modifiers, copy mode, last render, cursor
    // visibility, and hover link all live in `self.windows[main_window_id]`,
    // the same place a torn-out child's do — so one set of code paths serves
    // both. Reach them through `main_renderer()`, `main_tabs()`,
    // `main_panes()`, `main_selection()`, and their `_mut` counterparts.
    //
    // Callers needing several at once should go through `main_mut()` and
    // split-borrow the fields disjointly; taking two `main_*_mut()` accessors
    // together is a double borrow of the same map entry.
    pub(super) clipboard: Option<Clipboard>,
    #[cfg(target_os = "windows")]
    /// One delayed OSC 52 write that survives a failing clipboard helper's cleanup.
    pub(super) pending_osc52_reassert: Option<PendingOsc52Reassert>,
    /// Test-only in-memory clipboard override for integration tests that need
    /// to observe copy/paste routing without depending on a desktop clipboard
    /// service. `None` means production arboard behavior; `Some(_)` means reads
    /// and writes use this buffer instead.
    #[doc(hidden)]
    pub(super) test_clipboard_text: Option<String>,
    /// Test-only clipboard write rejection injected at the production write
    /// boundary. Disabled in every constructor so ordinary runs retain the real
    /// clipboard behavior and the in-memory success seam remains opt-in.
    #[doc(hidden)]
    pub(super) test_clipboard_write_failure: bool,
    /// Test-only PTY write ledger. `write_to_pane` records every boundary write
    /// here before resolving the pane to a real PTY, so headless tests can assert
    /// which pane an action targeted without constructing a process-backed PTY.
    #[doc(hidden)]
    pub(super) test_pty_writes: Arc<Mutex<Vec<(u64, Vec<u8>)>>>,
    /// Whether the PTY write ledger above is actually recorded. `false` in
    /// production so `dispatch_pty_write_effect` does no lock/clone/push per
    /// write (the ledger would otherwise grow unbounded for the whole
    /// session —). Set `true` when the app is built without an
    /// event-loop proxy (headless/test construction) so existing tests keep
    /// capturing writes with no per-test opt-in.
    #[doc(hidden)]
    pub(super) pty_write_log_enabled: bool,
    // `App`-level DPI and hovered_url fields deleted — both
    // now live exclusively on `WindowState`. Readers go through
    // `self.main()?.dpi_scale` / `self.main()?.hovered_url`
    // (with safe-default fallbacks at call sites). The shadow-sync
    // path was deleted as the last of the per-window migration.
    /// Action::NewWindow sets this
    /// flag, then `drain_pending_window_creates` consumes it by calling
    /// `create_new_terminal_window(el)`. Window creation requires an
    /// `ActiveEventLoop` reference
    /// that isn't reachable from the keymap dispatcher. Works from BOTH
    /// the windows-non-empty case (Cmd+N from a focused window) AND the
    /// windows-empty post-close-last-window dock-alive case on macOS.
    pub(super) pending_new_window: bool,
    /// Deferred in-process tab tear-out request from either drag/drop or the
    /// Move Tab to New Window action. Drained only while an ActiveEventLoop is
    /// available so every path uses the same native-window constructor.
    pub(super) pending_tear_out: Option<PendingTearOut>,
    /// Deferred `cancel_drag_session` request. Set by `handle_os_drag_ended`
    /// on the `DroppedOnEmpty` branch instead of cancelling inline,
    /// so any tear-out-spawn produced by the existing
    /// `pending_new_window` drain runs to completion BEFORE
    /// cross-window drag-residue cleanup mutates `self.windows`.
    /// Drained by `App::drain_pending_os_teardown` AFTER
    /// `App::drain_pending_window_creates` at the natural event-loop
    /// boundary in `event_loop.rs::do_user_event`. The
    /// `cancel_drag_session` all-windows loop runs **unconditionally**
    /// when drained — this flag controls only WHEN it runs, not
    /// WHETHER, so the cleanup stays idempotent.
    pub(super) pending_os_teardown: bool,
    /// Test-only callback fired
    /// inside [`Self::cancel_drag_session`] AFTER the `self.windows.keys()`
    /// snapshot is collected but BEFORE the per-id iteration body runs.
    /// Lets a regression test
    /// mutate `self.windows` in the exact race window that the
    /// `get_mut(&id).else { continue }` arm is designed to tolerate.
    /// Consumed (`take()`-d) at the call site so the closure is invoked
    /// at most once per `cancel_drag_session` run and the mutable
    /// borrow on `self.windows` is not held while it runs. Production
    /// cost is one extra `Option::take()` per `cancel_drag_session`
    /// invocation (always `None` outside tests) — gated by
    /// `#[doc(hidden)]` rather than `#[cfg(test)]` because the test
    /// living in `tests/os_drag_cleanup.rs` is an INTEGRATION test
    /// that compiles the crate without `cfg(test)`.
    #[doc(hidden)]
    pub(super) test_post_snapshot_hook: Option<Box<dyn FnOnce(&mut App) + Send>>,
    /// Deferred app-exit request. Set from `run_action` when the user's
    /// Cmd+W chain has just closed the last tab of the last window AND
    /// `Config::quit_on_last_window_close` is true (or non-macOS).
    /// `do_about_to_wait` drains it by calling `el.exit()`. The flag is
    /// needed because `run_action` does not have an `ActiveEventLoop`
    /// handle.
    pub(super) pending_exit: bool,
    /// When pane retention was last sampled for the memory log.
    ///
    /// `None` until the first sample. Gating on elapsed time rather than
    /// sampling every idle turn keeps a measurement that walks every pane off
    /// the path that governs idle CPU.
    pub(super) last_retention_sample: Option<std::time::Instant>,
    /// The preceding cycle's totals, so a snapshot can report movement.
    ///
    /// `None` until the first sample has been taken, which is what makes the
    /// first snapshot's deltas report `unavailable` rather than `+0` — the
    /// latter claims the process did not move, which is a measurement nobody
    /// made.
    ///
    /// Only the totals are retained rather than the whole snapshot: the
    /// per-renderer breakdown is a string per renderer, and holding it between
    /// samples would keep it alive for the life of the session to serve a
    /// report that never reads it.
    pub(super) last_memory_totals: Option<memory_snapshot::MemoryTotals>,
    /// Nonblocking recorder for bounded postmortem breadcrumbs.
    ///
    /// The platform binary owns the writer thread; the app only holds this cheap
    /// sender and never performs filesystem IO on the event-loop path.
    pub(super) breadcrumb_recorder: Option<sonicterm_logging::breadcrumbs::BreadcrumbRecorder>,
    /// Whether the currently-armed timed wake exists only to sample memory.
    ///
    /// Set when the wake deadline is armed and cleared when it fires. A wake
    /// armed by a diagnostic must not repaint: an idle session would otherwise
    /// draw a frame every thirty seconds forever purely to record that it was
    /// idle, which is a heartbeat redraw under another name.
    pub(super) wake_is_memory_only: bool,
    #[cfg(windows)]
    /// Whether the armed timer is solely a foreground-process sample with no frame due.
    pub(super) wake_is_foreground_probe_only: bool,
    pub(super) command_palette: CommandPalette,
    /// Which window the (single, modal) command palette is attached to.
    /// `None` means it is closed OR attached to the main window; `Some(id)`
    /// means that child window. Both the main and child render paths consult
    /// it so the palette paints only on the window it was opened from —
    /// without it, Cmd+Shift+P typed in a torn-out child opened the palette
    /// on the original main window.
    pub(super) palette_attached_window: Option<WindowId>,
    /// Set the moment a held-tab drag
    /// crosses [`os_drag::OS_DRAG_THRESHOLD_PX`] from its press point,
    /// before the user releases the button. Guards
    /// [`Self::try_os_drag_handoff`] in the `CursorMoved` path so the
    /// OS-level drag session starts mid-gesture (cursor still down)
    /// rather than waiting until mouse-up — which was too late for
    /// `DoDragDrop` to capture the cursor across windows. Cleared on
    /// `cancel_drag_session` and at every fresh mouse-down so a new
    /// gesture re-arms cleanly.
    pub(super) os_drag_handoff_started: bool,
    /// Windows spawned by tearing tabs out of the parent bar. Keyed by
    /// winit WindowId so events route back to the right child.
    /// Process-wide resource governor and its owner hierarchy.
    ///
    /// Holds the `Process` root; every window registers a `Window` owner below
    /// it and every pane an `AppPane` owner below its window. That hierarchy
    /// is what makes a window's total derivable from its panes — the question
    /// per-pane accounting cannot answer and the one a user asks when they
    /// close a window to reclaim memory.
    ///
    /// Registered here rather than accounted here: this change establishes
    /// ownership only. Charging producers through it is the larger job that
    /// changes when allocation happens, not merely where the number lives.
    pub(super) governor: ResourceGovernor,
    pub(super) windows: HashMap<WindowId, WindowState>,
    /// Id of the main window. Set in `do_resumed` once the main `Window` is
    /// created and its [`WindowState`] is inserted into [`Self::windows`].
    ///
    /// `None` before that point, which is why every `main_*()` accessor
    /// returns an `Option` rather than assuming a main window exists.
    pub(super) main_window_id: Option<WindowId>,
    /// Most-recently-OS-frontmost window id, INCLUDING the main window.
    /// Tracks *every* sonic-owned terminal window with a single
    /// non-`Option` discriminant once the first focus arrives:
    ///
    ///   * `Some(main_window_id)`  → main window is OS-frontmost
    ///   * `Some(child_window_id)` → that child window is OS-frontmost
    ///   * `None`                  → no sonic window has been focused yet,
    ///     OR focus has moved out of every sonic window to another app.
    ///
    /// Subsumes a separate "which child has focus" field: main-vs-child is
    /// discriminated by `frontmost_kind()`, so one id answers both questions
    /// and the two cannot disagree.
    ///
    /// Keyboard / menubar / accelerator actions (Cmd+T, Cmd+W, Cmd+\\, …)
    /// route through this id so a chord typed in window B never mutates
    /// window A's tab vec. Set in both the main and child `Focused(true)`
    /// arms; on `Focused(false)` we only clear when the dropped window was
    /// the current frontmost — focus moving to a *different* sonic window
    /// arrives as that other window's `Focused(true)` and overwrites
    /// frontmost in the right order.
    ///
    /// Without it, Cmd+T after a tear-out opened a tab in the wrong window,
    /// and Cmd+W in a new window closed the old window's tab.
    pub(super) frontmost_window: Option<WindowId>,
    /// OS-drag tab payloads received before the main [`WindowState`] exists.
    /// Startup pasteboard / OLE deliveries can arrive before `do_resumed`
    /// inserts `main_window_id`; queue them so the destination tab is created
    /// after main is available instead of silently dropping the payload.
    pub(super) pending_os_drag_payloads: Vec<crate::os_drag::TabPayload>,
    /// Optional theme loader, set by `run_with`. Used to reload a theme
    /// by name live.
    pub(crate) theme_loader: Option<ThemeLoader>,
    /// Optional keymap loader, set by `run_with`.
    pub(crate) keymap_loader: Option<KeymapLoader>,
    /// Proxy used to wake the idle event loop. `None` in tests that
    /// construct `App` directly via [`App::new`] without a real event loop.
    pub(super) event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    /// Bounded workers for openability probes and native direct-open dispatch.
    pub(in crate::app) path_workers: Option<path_target::PathWorkers>,
    /// Current native home captured once for deterministic `~/` target resolution.
    pub(super) home_dir: Option<PathBuf>,
    /// Local hostname used to reject foreign-authority OSC 7 snapshots.
    pub(super) local_hostname: String,
    /// Hidden Linux package-smoke state; absent during ordinary application runs.
    pub(super) runtime_smoke: Option<runtime_smoke::RuntimeSmokeState>,
    /// Minimum interval between two successive frames. Defaults to 1/60s
    /// and is updated in `resumed` from the current monitor's reported
    /// refresh rate. Used by the RedrawRequested handler to skip an
    /// over-render and by `about_to_wait` to schedule the next vsync
    /// boundary via `ControlFlow::WaitUntil`. See perf audit #9.
    pub(super) frame_period: Duration,
    /// The monitor's own reported period, kept separately so the degrade
    /// decision stays reversible.
    ///
    /// `frame_period` is the *resolved* period and is overwritten with the
    /// software cap while degrading. Resolving a later decision from it would
    /// read the cap back as if it were the monitor's rate, so clearing degrade
    /// could not restore the monitor's cadence and the window stayed at 40 fps
    /// until restart. Every resolution reads this field instead; only the
    /// monitor probe writes it.
    pub(super) monitor_frame_period: Duration,
    /// True when the no-GPU degrade path is engaged (software rasterizer
    /// detected or forced via `[appearance].software_render_mode`). When set,
    /// `frame_period` is replaced by the software cap and per-frame scrollbar
    /// fade animation is suppressed so the CPU isn't asked to rasterize at full
    /// refresh. Resolved after the renderer is created and re-resolved on an
    /// explicit config reload.
    pub(super) software_render_degrade: bool,
    /// Set when a RedrawRequested arrives sooner than `frame_period`
    /// after the previous render. `about_to_wait` schedules a
    /// `WaitUntil(last_render + frame_period)` and `new_events`'
    /// `ResumeTimeReached` arm calls `request_redraw()` so we coalesce
    /// the pending request onto the next vsync tick rather than
    /// burning a frame.
    pub(super) pending_redraw: bool,
    /// Per-CHILD-window analogue of [`Self::pending_redraw`]. The main
    /// window's deferred-redraw latch is a single bool keyed off
    /// `main().last_render`; torn-out child windows each carry their own
    /// `WindowState.last_render` and `request_redraw()`, so a child that
    /// defers a PTY-streaming or lock-contended redraw records its
    /// `WindowId` here. `about_to_wait` folds each pending child's
    /// `last_render + frame_period` into the next `WaitUntil` deadline,
    /// and `new_events`' `ResumeTimeReached` arm re-requests a redraw on
    /// exactly those windows. An entry is cleared when that child next
    /// renders past the coalescing gate (or when the window is reaped).
    pub(super) pending_redraw_windows: HashSet<WindowId>,
    pub(super) warm_window_pool: Vec<WarmWindow>,
    /// Set true whenever a user-driven event (keyboard, mouse click,
    /// cursor move while dragging, resize, IME, modifier change) or a
    /// live-reload of theme/font/keymap occurs. The next
    /// `WindowEvent::RedrawRequested` will bypass the vsync coalescing
    /// gate so the first frame after input is immediate (zero added
    /// latency). Subsequent redraws driven purely by streaming PTY
    /// bytes within the same `frame_period` still coalesce onto the
    /// next vsync boundary via `pending_redraw`. Cleared on every
    /// frame we actually render.
    pub(super) input_dirty: bool,
    /// Shared with every VT-thread spawned in `spawn_pty_for_pane` (one
    /// per pane). Incremented by the VT loop whenever a non-empty chunk
    /// of PTY bytes is processed; sampled on each `RedrawRequested` to
    /// decide whether to bypass the vsync coalescing gate.
    pub(super) pty_burst_gen: Arc<AtomicU32>,
    /// Last PTY-burst generation that a completed render observed. If
    /// the VT thread increments [`Self::pty_burst_gen`] during render,
    /// this remains behind the current generation so the next redraw
    /// bypasses the vsync gate instead of losing the burst.
    pub(super) last_seen_burst_gen: u32,
    /// Translation bundle. Rebuilt when the user picks a new locale in
    /// the preferences "Language" dropdown.
    pub(super) i18n: sonicterm_ui::i18n::I18n,
    /// Optional platform hook that takes a serialized tab payload and
    /// hands it off to the OS-level drag-and-drop system
    /// (`NSPasteboard` on macOS, OLE `DoDragDrop` on Windows). When
    /// set, [`Self::tear_out_tab`] checks whether the cursor sits outside every
    /// SonicTerm-owned window and invokes the sink. The local tab is detached
    /// only if the sink returns an explicit `DragAck::Accepted`; current
    /// platform paths preserve it and fall back to in-process tear-out.
    /// Installed by the platform shell via
    /// [`crate::shell::MacShell::with_os_drag_sink`] /
    /// [`crate::shell::WindowsShell::with_os_drag_sink`].
    pub(crate) os_drag_sink: Option<Arc<dyn crate::os_drag::OsDragSink>>,
    /// Platform OS-drag backend. Distinct from `os_drag_sink` (wire-format
    /// publication): Windows drives OLE `DoDragDrop`; macOS currently publishes
    /// the pasteboard payload and posts a cancelled outcome without cursor capture.
    /// Installed by the platform bin (`sonicterm-mac` / `sonicterm-windows`)
    /// at startup. `None` in tests + on platforms without an impl.
    pub(super) os_drag_backend: Option<Box<dyn os_drag::OsTabDragBackend>>,
    /// Shared mailbox the [`os_drag::OsTabDragBackend`] writes pending
    /// drag outcomes into. Drained by `do_user_event` on every
    /// `UserEvent::DragMoved` / `DragEnded` wake.
    pub(super) os_drag_pending: Arc<os_drag::PendingDragOutcome>,
    /// Shared tab-bar snapshot registry. The App publishes the live
    /// per-window tab bar layout into this on every redraw (see
    /// `publish_os_drag_bar_snapshot`); a Phase-C2 OS-drag backend
    /// reads from it inside its drop callback (Windows
    /// IDropTarget::Drop / macOS NSDraggingDestination::performDrop)
    /// to resolve the raw screen-coordinate drop into a real
    /// `(WindowId, slot)` pair before posting a `DroppedOnBar` outcome.
    pub(super) os_drag_bars: Arc<os_drag::TabBarRegistry>,
    /// tracks the source-side bookkeeping while an OS drag
    /// is in flight. `Some((source_window, source_tab_idx))` from
    /// `begin_session` until `UserEvent::DragEnded` is drained; back
    /// to `None` once the dispatcher routes the outcome.
    pub(super) os_drag_source: Option<(WindowId, usize)>,
    /// View → Toggle Tab Bar state. When `false`, the menubar Toggle
    /// Tab Bar action has hidden the tab bar chrome. Defaults to
    /// `true`. Exposed via [`Self::tab_bar_visible`] so the renderer
    /// + hit-test code can read it on each frame.
    pub(super) tab_bar_visible: bool,
    /// Broadcast-input mode. When enabled, bytes typed into `source_pane`
    /// are mirrored into matching receiver panes after the source PTY write.
    pub(super) broadcast: BroadcastState,
    /// Quit confirmation guard for the Cmd+Q chord. A single press arms it and
    /// shows a red "press again" prompt; the app exits on a second press during
    /// the confirmation window. See [`quit_hold`].
    pub(super) quit_hold: quit_hold::QuitHold,
    /// One-shot hook fired the first time the winit `ApplicationHandler::
    /// resumed` callback runs — i.e. when NSApp / the platform event
    /// loop is fully initialized but BEFORE we hand control back to
    /// winit's `run_app`. macOS uses this slot to install the native
    /// NSMenu; calling `setMainMenu` earlier (before winit builds the
    /// AppKit loop) leaves AppKit with only the default
    /// `Apple, sonicterm-mac` menubar.
    pub(crate) on_resumed: Option<Box<dyn FnOnce() + Send>>,

    /// One-shot hook fired the moment the main window has been created
    /// (immediately after `el.create_window` succeeds, before the first
    /// redraw is requested). Receives the `raw-window-handle` of the
    /// window. Windows uses this slot to install the muda menubar,
    /// which requires the HWND at install time. Unused on macOS.
    pub(super) on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
    /// Test-only redraw request counter. Every
    /// production code path that calls `window.request_redraw()` after
    /// a `run_action` dispatch also bumps this counter in lock-step.
    /// Tests assert against this rather than the live winit window
    /// (which has no public introspection API). Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub redraw_request_count: std::sync::atomic::AtomicUsize,
    /// Test-only counter incremented on every call to
    /// [`Self::reap_empty_child`]. Lets tests
    /// distinguish "child window cleanup went through the unified reap
    /// contract" from "a direct `windows.remove` happened" — both would
    /// shrink the `windows` map, but only the former nulls out straggler
    /// `redraw_target`s and fires the reap trace. Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub reap_call_count: std::sync::atomic::AtomicUsize,
    /// Test-only viewport override. When
    /// `Some((outer, cell_w, cell_h))`, [`Self::compute_active_pane_rects`]
    /// uses `outer` instead of fetching the renderer's logical size and
    /// [`Self::resize_visible_panes`] uses `(cell_w, cell_h)` instead of
    /// the renderer's `cell_size()`. Lets tests exercise the production
    /// `close_active_pane` path (Grid + PtyHandle resize wiring) without
    /// a live wgpu surface. Stays `None` in release builds whose tests
    /// don't touch it.
    #[doc(hidden)]
    pub test_viewport_override: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
    /// Winit-agnostic state machine. Routed Intents
    /// (PTY write, scroll, hyperlink open, …) flow through here and
    /// the platform shell's [`Self::dispatch_effects`] translates the
    /// resulting [`AppEffect`] batch into concrete calls against the
    /// existing renderer / clipboard / PTY plumbing. Non-leaf paths
    /// (tab/pane/window lifecycle) continue to take the legacy direct
    /// route rather than passing through the reducer.
    pub(crate) machine: sonicterm_app_core::AppStateMachine,
}

impl sonicterm_ui::broadcast::BroadcastTab for TabState {
    fn pane_tree(&self) -> &PaneTree {
        &self.tree
    }
}

impl App {
    /// Window-pixel rects for every pane in the active tab.
    ///
    /// Derived from the main renderer's logical size, insets, and padding, so
    /// resize and config-reload sites share one geometry source. Empty before a
    /// renderer exists or when no tab is active.
    pub(crate) fn compute_active_pane_rects(&self) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let Some(ws) = self.main() else {
            // When: `main` has no window yet, so no surface exists to derive a
            // layout from and there is nothing to size panes against.
            return Vec::new();
        };
        let tab_idx = ws.tabs.active_index();
        let Some(st) = ws.tab_states.get(tab_idx) else {
            // When: `tab_idx` names no entry in `tab_states`, so no pane tree
            // exists to lay out.
            return Vec::new();
        };
        if let Some((outer, _, _)) = self.test_viewport_override {
            // When: `test_viewport_override` supplies the outer rect directly, so
            // layout runs without a live renderer to read metrics from.
            return st.tree.layout(outer);
        }
        let Some(r) = self.main_renderer() else {
            // When: `main_renderer` is absent, so logical size and insets are
            // unavailable and no rect can be computed.
            return Vec::new();
        };
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        let outer =
            sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0));
        st.tree.layout(outer)
    }

    /// Same as [`Self::compute_active_pane_rects`] but for a torn-out
    /// child window (its own renderer + tab_states).
    pub(crate) fn compute_pane_rects_for(
        child: &WindowState,
    ) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get(tab_idx) else {
            // When: `tab_idx` names no entry in the child's `tab_states`, so it
            // carries no pane tree to lay out.
            return Vec::new();
        };
        if let Some((outer, _, _)) = child.test_pane_viewport {
            // When: `test_pane_viewport` supplies the outer rect, so a headless
            // child with no renderer still resolves its pane geometry.
            return st.tree.layout(outer);
        }
        let Some(r) = child.renderer.as_ref() else {
            // When: the child's `renderer` is absent, so logical size and insets
            // are unavailable and no rect can be computed.
            return Vec::new();
        };
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        let outer =
            sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0));
        st.tree.layout(outer)
    }

    /// Build an app with no event-loop proxy.
    ///
    /// Without a proxy the app cannot post itself user events, so this suits
    /// callers that drive it directly rather than through a running loop.
    #[doc(hidden)]
    pub fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self::new_with_proxy(theme, config, keymap, None)
    }

    /// Build an app that posts user events through `event_loop_proxy`.
    ///
    /// The state machine is built here rather than supplied, so callers that
    /// already own one should hand it in instead.
    #[doc(hidden)]
    pub fn new_with_proxy(
        theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    ) -> Self {
        Self::new_with_proxy_and_machine(
            theme,
            config,
            keymap,
            event_loop_proxy,
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default()),
        )
    }

    /// Build an app around an externally-built
    /// [`sonicterm_app_core::AppStateMachine`].
    ///
    /// The platform shell constructs the machine first and hands it in, so all
    /// state mutation routes through the reducer the shell already owns rather
    /// than a second machine built here.
    pub fn new_with_proxy_and_machine(
        mut theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
        machine: sonicterm_app_core::AppStateMachine,
    ) -> Self {
        theme.apply_accessibility(&config.accessibility);
        // Seed the process-global tab width cap from config before any tab
        // bar is laid out, so a configured value takes effect on the very
        // first frame (hot-reload updates it later via apply_new_config).
        sonicterm_ui::tabbar_view::set_max_tab_width(config.tab_max_width);
        let i18n = sonicterm_ui::i18n::I18n::new(if config.locale.is_empty() {
            None
        } else {
            // When: `config` names a locale, so that tag selects the translation
            // set instead of leaving the OS default to choose it.
            Some(config.locale.as_str())
        });
        let mut command_palette = CommandPalette::new();
        command_palette.set_keymap(&keymap);
        let configured_font_size = config.font.size;
        let configured_weight_scale = config.font.effective_weight_scale();
        let font_dirs = vec![sonicterm_cfg::assets::asset_dir().join("fonts")];
        let path_workers = event_loop_proxy.as_ref().and_then(|proxy| {
            match path_target::PathWorkers::start(proxy.clone()) {
                Ok(workers) => Some(workers),
                Err(error) => {
                    tracing::warn!(%error, "path workers unavailable");
                    None
                }
            }
        });
        let home_dir = path_target::native_home_dir();
        let local_hostname = gethostname::gethostname().to_string_lossy().into_owned();
        Self {
            theme,
            process_privilege: crate::ProcessPrivilege::default(),
            #[cfg(windows)]
            foreground_probe_wake: None,
            config,
            font_dirs,
            configured_font_size,
            configured_weight_scale,
            keymap,
            clipboard: Clipboard::new().ok(),
            #[cfg(target_os = "windows")]
            pending_osc52_reassert: None,
            test_clipboard_text: None,
            test_clipboard_write_failure: false,
            test_pty_writes: Arc::new(Mutex::new(Vec::new())),
            // No event-loop proxy ⇒ headless/test construction ⇒ record PTY
            // writes for assertions. Production always passes `Some(proxy)`,
            // so the ledger stays disabled and adds no per-write cost.
            pty_write_log_enabled: event_loop_proxy.is_none(),
            pending_new_window: false,
            pending_tear_out: None,
            pending_os_teardown: false,
            test_post_snapshot_hook: None,
            pending_exit: false,
            last_retention_sample: None,
            last_memory_totals: None,
            breadcrumb_recorder: None,
            wake_is_memory_only: false,
            #[cfg(windows)]
            wake_is_foreground_probe_only: false,
            command_palette,
            palette_attached_window: None,
            os_drag_handoff_started: false,
            governor: ResourceGovernor::new(
                ProcessKind::Gui,
                GovernorLimits {
                    // Deliberately unlimited. Enforcement stays with the
                    // per-seam caps that are already tested; a second
                    // enforcement point would create two figures that must
                    // agree and will eventually drift — the defect shape of
                    // the charge-lifetime bug, where a reservation outlived
                    // the thing it was taken for.
                    process_bytes: usize::MAX,
                    class_bytes: enum_map::enum_map! { _ => usize::MAX },
                    class_items: enum_map::enum_map! { _ => None },
                },
            )
            .expect("an unlimited governor cannot fail to construct"),
            windows: HashMap::new(),
            main_window_id: None,
            frontmost_window: None,
            pending_os_drag_payloads: Vec::new(),
            theme_loader: None,
            keymap_loader: None,
            event_loop_proxy,
            path_workers,
            home_dir,
            local_hostname,
            runtime_smoke: None,
            // Default to 60 Hz until `resumed` probes the actual
            // monitor refresh rate. ~16.667 ms = 1/60 s.
            frame_period: Duration::from_micros(16_667),
            monitor_frame_period: Duration::from_micros(16_667),
            // Resolved after the renderer is created in `do_resumed`.
            software_render_degrade: false,
            pending_redraw: false,
            pending_redraw_windows: HashSet::new(),
            warm_window_pool: Vec::new(),
            input_dirty: false,
            pty_burst_gen: Arc::new(AtomicU32::new(0)),
            last_seen_burst_gen: 0,
            i18n,
            os_drag_sink: None,
            os_drag_backend: None,
            os_drag_pending: Arc::new(os_drag::PendingDragOutcome::default()),
            os_drag_bars: Arc::new(os_drag::TabBarRegistry::default()),
            os_drag_source: None,
            tab_bar_visible: true,
            broadcast: BroadcastState::Off,
            quit_hold: quit_hold::QuitHold::new(),
            on_resumed: None,
            on_window_ready: None,
            redraw_request_count: std::sync::atomic::AtomicUsize::new(0),
            reap_call_count: std::sync::atomic::AtomicUsize::new(0),
            test_viewport_override: None,
            machine,
        }
    }

    /// Return the privilege snapshot supplied by the native startup boundary.
    #[must_use]
    pub const fn process_privilege(&self) -> crate::ProcessPrivilege {
        self.process_privilege
    }

    /// Install the native process-privilege snapshot before window creation.
    pub(crate) fn set_process_privilege(&mut self, privilege: crate::ProcessPrivilege) {
        self.process_privilege = privilege;
    }

    pub(crate) fn set_breadcrumb_recorder(
        &mut self,
        recorder: sonicterm_logging::breadcrumbs::BreadcrumbRecorder,
    ) {
        self.breadcrumb_recorder = Some(recorder);
    }

    /// Refresh every main-window tab's command status from its panes.
    ///
    /// Every tab is polled, not just the active one, so a background tab's
    /// badge reflects work that finished while it was hidden.
    #[doc(hidden)]
    pub fn poll_command_events_for_all_tabs(&mut self) {
        let n = self.main_tab_states().map(|ts| ts.len()).unwrap_or(0);
        for tab_idx in 0..n {
            self.poll_command_events_for_tab(tab_idx);
        }
    }

    pub(super) fn poll_command_events_for_tab(&mut self, tab_idx: usize) {
        let Some(id) = self.main_window_id else {
            // When: `main_window_id` is unset, so no tab bar exists yet to carry
            // the status this poll would produce.
            return;
        };
        let Some(ws) = self.windows.get_mut(&id) else {
            // When: `id` no longer resolves in `windows`, so the state this poll
            // would write into is already gone.
            return;
        };
        poll_command_events_for_tab_state(
            &ws.panes,
            &mut ws.tab_states,
            &mut ws.tabs,
            &self.config,
            tab_idx,
        );
    }

    /// Test seam: queue a command event on a pane without running a shell.
    ///
    /// Lets a test drive command-status and badge behavior from synthetic
    /// events instead of waiting on real process transitions.
    #[doc(hidden)]
    pub fn __test_push_pane_command_event(
        &mut self,
        pane_id: u64,
        event: CommandEvent,
        at: Instant,
        duration: Option<Duration>,
    ) {
        if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            pane.command_events.lock().push(PaneCommandEvent { event, at, duration });
        }
    }

    /// Test seam: the command status a tab currently reports.
    ///
    /// `None` when no main window or no tab sits at `tab_idx`.
    #[doc(hidden)]
    pub fn __test_command_status_for_tab(&self, tab_idx: usize) -> Option<CommandStatus> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.command.clone())
    }

    /// Test seam: the badge a tab would render at `now`.
    ///
    /// Badge text depends on whether the tab is the active one, so this
    /// resolves activeness the same way the tab bar does.
    #[doc(hidden)]
    pub fn __test_tab_badge(&self, tab_idx: usize, now: Instant) -> Option<&'static str> {
        let tabs = self.main_tabs()?;
        tabs.tabs()
            .get(tab_idx)
            .and_then(|tab| tab.command.clone().badge(now, tab_idx == tabs.active_index()))
    }
}

/// Drain one tab's pane command events into its command status and tab badge.
///
/// Events are collected across every leaf pane in the tab, so a command that
/// finished in a non-focused split still updates the tab. A finished command
/// holds its badge for a few seconds before the status lapses.
#[doc(hidden)]
pub fn poll_command_events_for_tab_state(
    panes: &HashMap<u64, PaneState>,
    tab_states: &mut [TabState],
    tabs: &mut TabBar,
    config: &Config,
    tab_idx: usize,
) {
    let Some(tab_state) = tab_states.get_mut(tab_idx) else {
        // When: `get_mut` cannot resolve `tab_idx`, so nothing exists to receive
        // the drained events and the panes are left holding them.
        return;
    };
    let pane_ids = tab_state.tree.leaves();
    let mut events = Vec::new();
    for pane_id in pane_ids {
        if let Some(pane) = panes.get(&pane_id) {
            let mut q = pane.command_events.lock();
            events.extend(q.drain(..));
        }
    }
    if events.is_empty() {
        // When: no pane produced `events`, so the existing status and badge
        // already describe the tab and republishing would only churn.
        return;
    }
    for ev in events {
        match ev.event {
            CommandEvent::CmdStart => tab_state.command = CommandStatus::Running(ev.at),
            CommandEvent::CmdEnd(exit) => {
                tab_state.command =
                    CommandStatus::Done { exit, until: ev.at + Duration::from_secs(3) };
                maybe_notify_long_command(config, ev.duration, exit);
            }
            CommandEvent::PromptStart => {
                // When: `PromptStart` marks the shell drawing its prompt, not a
                // command boundary, so the tab's running/done status stands.
            }
        }
    }
    if let Some(t) = tab_states.get(tab_idx).map(|st| st.command.clone()) {
        tabs.set_command_status(tab_idx, t);
    }
}

/// Refresh every tab of a torn-out child window from its panes.
///
/// A child runs its own tab bar, so it polls independently of the main window
/// rather than inheriting the main window's sweep.
#[doc(hidden)]
pub fn poll_command_events_for_child_window(child: &mut WindowState, config: &Config) {
    for tab_idx in 0..child.tab_states.len() {
        poll_command_events_for_tab_state(
            &child.panes,
            &mut child.tab_states,
            &mut child.tabs,
            config,
            tab_idx,
        );
    }
}

fn maybe_notify_long_command(config: &Config, duration: Option<Duration>, exit: Option<u8>) {
    let Some(duration) = duration else {
        // When: the event carries no `duration`, so elapsed time cannot be
        // compared against the threshold that makes a command "long".
        return;
    };
    if !config.notifications.long_command {
        // When: `config` disables long_command notifications, so a finished
        // command stays silent however long it ran.
        return;
    }
    if duration.as_secs() <= config.notifications.threshold_secs {
        // When: `duration` sits within threshold_secs, so the user was not
        // waiting long enough for a desktop interruption to be welcome.
        return;
    }
    let result = match exit {
        Some(0) => "completed successfully",
        Some(code) => {
            // When: `code` is nonzero, so the message names the failure rather
            // than the generic completion wording built below.
            return notify_command_done(format!("Command failed with exit code {code}"));
        }
        None => "completed",
    };
    notify_command_done(format!("Command {result} after {}s", duration.as_secs()));
}

#[cfg(target_os = "windows")]
fn notify_command_done(body: String) {
    if let Err(err) = notify_rust::Notification::new().summary("Command done").body(&body).show() {
        tracing::debug!(?err, "desktop notification failed");
    }
}

#[cfg(not(target_os = "windows"))]
fn notify_command_done(_body: String) {}

impl App {
    /// Returns `true` when closing the last window should exit the
    /// process, given a config. On macOS we honor
    /// [`Config::quit_on_last_window_close`] (default `true` →
    /// traditional terminal: closing the last window quits the app;
    /// set to `false` for Chrome/Firefox-style dock-alive). On other platforms there is no dock concept, so we
    /// always exit once the last window is gone — the config is
    /// ignored. Exposed (test-only) so behavior is verifiable without
    /// building a real winit event loop.
    #[doc(hidden)]
    pub fn should_exit_on_last_window_close(config: &Config) -> bool {
        #[cfg(target_os = "macos")]
        {
            config.quit_on_last_window_close
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = config;
            true
        }
    }

    /// Called from the `RedrawRequested` handler when the active pane's
    /// parser lock is contended (held by the VT thread mid-parse).
    /// Marks `pending_redraw` so `about_to_wait` schedules a
    /// `WaitUntil` at the next vsync boundary, and preserves the
    /// `input_dirty` flag captured at the start of the handler so the
    /// rescheduled redraw still bypasses the vsync coalescing gate.
    ///
    /// Without this, a single contended `try_lock` during the
    /// input→output transition of a multi-round prompt (e.g.
    /// `gh auth login`'s device-code flow,) would silently
    /// drop the redraw request — the parsed bytes sat in the grid
    /// unrendered until an unrelated event (Ctrl+C, mouse move) woke
    /// the loop and triggered a fresh `RedrawRequested`.
    #[doc(hidden)]
    pub fn defer_redraw_on_lock_contention(&mut self, was_dirty: bool) {
        self.pending_redraw = true;
        self.input_dirty = was_dirty;
    }

    /// Child-window analogue of [`Self::defer_redraw_on_lock_contention`]
    /// plus the vsync coalescing gate. Records `win_id` in
    /// [`Self::pending_redraw_windows`] so `about_to_wait` schedules a
    /// `WaitUntil` at that child's next frame boundary and
    /// `new_events` re-requests the redraw there — instead of the child
    /// busy-spinning a bare `request_redraw()` that re-contends the very
    /// parser lock the VT thread needs to drain a burst (
    /// `ls -al` was smooth in main but laggy in a torn-out child because
    /// the child render path had neither the gate nor this backoff).
    /// Preserves the `input_dirty` flag captured at the top of the
    /// handler so a deferred input-driven redraw still bypasses the gate
    /// when it re-fires.
    #[doc(hidden)]
    pub fn defer_child_redraw(&mut self, win_id: WindowId, was_dirty: bool) {
        self.pending_redraw_windows.insert(win_id);
        self.input_dirty = was_dirty;
    }

    /// Test-only: `true` if `win_id` has a deferred redraw queued in
    /// [`Self::pending_redraw_windows`] (the child-window coalescing latch).
    #[doc(hidden)]
    pub fn __test_child_redraw_deferred(&self, win_id: WindowId) -> bool {
        self.pending_redraw_windows.contains(&win_id)
    }

    /// Test-only: read the shared input-driven-redraw flag.
    #[doc(hidden)]
    pub fn __test_input_dirty(&self) -> bool {
        self.input_dirty
    }

    /// Install a one-shot callback fired at the top of the first
    /// `ApplicationHandler::resumed` tick. macOS uses this to install
    /// the native NSMenu after winit has built the AppKit event loop —
    /// installing earlier leaves AppKit with only the default
    /// `Apple, sonicterm-mac` menu bar.
    pub fn set_on_resumed<F: FnOnce() + Send + 'static>(&mut self, hook: F) {
        self.on_resumed = Some(Box::new(hook));
    }

    /// Set the one-shot hook fired right after window creation, with
    /// the window's raw handle. See the field docs for the use-case
    /// (Windows muda menubar install).
    pub fn set_on_window_ready<F>(&mut self, hook: F)
    where
        F: FnOnce(raw_window_handle::RawWindowHandle) + Send + 'static,
    {
        self.on_window_ready = Some(Box::new(hook));
    }

    /// Translate a UI message id. See [`sonicterm_ui::i18n::I18n::t`]. Returns
    /// the key itself if no bundle (active or English fallback) has it,
    /// so the UI never renders an empty label.
    pub fn t(&self, key: &str) -> String {
        self.i18n.t(key)
    }

    /// Translate with `{ $name }` arguments. See
    /// [`sonicterm_ui::i18n::I18n::t_args`].
    pub fn t_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.i18n.t_args(key, Some(args))
    }

    /// Currently active locale tag (e.g. `"en"`, `"zh-CN"`).
    pub fn locale(&self) -> String {
        self.i18n.locale()
    }

    /// Live-apply a new locale. Persists the choice to `self.config.locale`.
    /// Pass `""` to mean "auto-detect from OS locale".
    pub fn set_locale(&mut self, requested: &str) {
        self.config.locale = requested.to_string();
        self.i18n = sonicterm_ui::i18n::I18n::new(if requested.is_empty() {
            None
        } else {
            // When: `requested` names a locale tag, so it selects the bundle
            // directly instead of leaving the OS default to decide.
            Some(requested)
        });
    }

    /// Decide whether the event loop should exit. The app should keep
    /// running as long as ANY active terminal window owns at least one tab:
    /// a visible main window with tabs, or any torn-out child window. A
    /// hidden/drained main window is intentionally NOT active for process
    /// lifecycle purposes; once the final child is gone there is no window
    /// the user can interact with, so requires quitting instead of
    /// leaving a dock-alive/headless process around.
    #[doc(hidden)]
    pub fn should_exit(&self) -> bool {
        Self::should_exit_pure(
            self.main_tabs().map(|t| t.len()).unwrap_or(0),
            self.main_is_hidden(),
            self.child_window_count(),
        )
    }

    /// Test-only: pure policy fn mirroring `should_exit` so integration
    /// tests can exercise the rule without constructing a real
    /// `WindowState` (which requires a live winit Window + GpuRenderer).
    #[doc(hidden)]
    pub fn should_exit_pure(main_tabs: usize, main_hidden: bool, child_count: usize) -> bool {
        let main_alive = !main_hidden && main_tabs > 0;
        !main_alive && child_count == 0
    }

    /// Mark a deferred process exit when no active terminal windows remain.
    /// This is the `ActiveEventLoop`-free counterpart to `el.exit()` for
    /// keymap/tab-close paths; `do_about_to_wait` drains the flag. OS window
    /// close handlers with an event-loop handle may still call `el.exit()`
    /// directly after this predicate becomes true.
    pub(super) fn request_exit_if_no_active_windows(&mut self) {
        if self.should_exit() {
            self.pending_exit = true;
        }
    }

    /// is the main window currently hidden / drained?
    /// `true` when the main `WindowState` is gone OR its `hidden` latch
    /// is set. The two shapes mean the same thing operationally — no
    /// visible main — so callers don't need to discriminate.
    #[doc(hidden)]
    pub fn main_is_hidden(&self) -> bool {
        match self.main() {
            Some(ws) => ws.hidden,
            None => true,
        }
    }

    /// Test-only: read the main window's `hidden` latch via the unified
    /// accessor.
    #[doc(hidden)]
    pub fn __test_main_hidden(&self) -> bool {
        self.main_is_hidden()
    }

    /// Test-only: drive the production `hide_main_window` path from
    /// integration tests (the helper itself is `pub(super)`).
    #[doc(hidden)]
    pub fn __test_hide_main_window(&mut self) {
        self.hide_main_window();
    }

    /// Test-only: read the deferred-exit flag set by `run_action`
    /// when the user's Cmd+W chain has drained the last tab of the
    /// last window in `quit_on_last_window_close = true` mode.
    #[doc(hidden)]
    pub fn __test_pending_exit(&self) -> bool {
        self.pending_exit
    }

    /// Unified "did this close just empty the affected window?" check
    /// for the keymap path. Mirrors what the mouse-click close-button
    /// path in `window_event.rs` and the OS `CloseRequested` arm do —
    /// hide the main window (or exit, on the last window) when its
    /// tabs vec is empty, and reap child windows the same way the drag-
    /// merge path does. The flag set here is drained in
    /// `do_about_to_wait`.
    pub(super) fn reap_empty_main_window_after_close(&mut self) {
        if !self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) {
            // When: `main_tabs` still holds a tab, so the window is in use and
            // the drained-window teardown below would close live work.
            return;
        }
        if self.child_window_count() == 0 {
            self.hide_main_window();
            self.request_exit_if_no_active_windows();
        } else {
            // When: `child_window_count` is nonzero, so tabs survive elsewhere;
            // hide main and leave the exit decision to the last child closing.
            self.hide_main_window();
        }
    }

    /// Test-only: force-set the main window's `hidden` latch so
    /// post-merge drain-policy tests can simulate the "main already
    /// retired" state without driving a real winit close event.
    #[doc(hidden)]
    pub fn __test_set_main_hidden(&mut self, v: bool) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.hidden = v;
        }
    }

    fn active_pane_id(&self) -> Option<u64> {
        self.main_active_pane_id()
    }

    fn main_active_pane_id(&self) -> Option<u64> {
        let ws = self.main()?;
        let i = ws.tabs.active_index();
        ws.tab_states.get(i).map(|t| t.active_pane)
    }

    fn active_pane_id_for_kind(&self, kind: FrontmostKind) -> Option<u64> {
        match kind {
            FrontmostKind::Child(id) => {
                let ws = self.windows.get(&id)?;
                let i = ws.tabs.active_index();
                ws.tab_states.get(i).map(|t| t.active_pane)
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                self.main_active_pane_id()
            }
        }
    }

    fn active_pane(&self) -> Option<&PaneState> {
        let id = self.active_pane_id()?;
        self.pane_by_id(id)
    }

    fn pane_by_id(&self, pane_id: u64) -> Option<&PaneState> {
        self.windows.values().find_map(|ws| ws.panes.get(&pane_id))
    }

    fn request_redraw_all_terminal_windows(&self) {
        for (id, ws) in &self.windows {
            if Some(*id) == self.main_window_id {
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            } else {
                // When: `id` is not `main_window_id`, so the redraw is requested
                // on the torn-out child's own surface rather than main's.
                ws.request_redraw();
            }
        }
    }

    fn write_to_pty(&mut self, bytes: Vec<u8>) {
        let Some(active_id) = self.active_pane_id() else {
            // When: `active_pane_id` resolves nothing, so there is no focused
            // target to receive the bytes and no source to broadcast from.
            return;
        };
        self.write_to_pane(active_id, bytes.clone());
        self.broadcast_from(active_id, bytes);
    }

    /// Test-only mirror of the normal KeyboardInput dispatch order: try every
    /// keymap spelling before encoding bytes for PTY forwarding.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        self.__test_dispatch_key_or_encode_pty_with_drain(key, mods, false)
    }

    /// Test-only mirror of the child-window KeyboardInput action path.
    /// The production child handler drains `pending_new_window` immediately
    /// after `run_action`; this helper exposes the same post-dispatch state
    /// without requiring a live `ActiveEventLoop`.
    // Ordering: `kitty_flags` and `app_cursor_keys` both load `Relaxed`; each is a
    // self-contained pane flag whose read is ordered against no other location.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty_with_drain(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
        simulate_drain: bool,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        for key_str in key_to_strings(key, mods) {
            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                // When: `keymap` resolves `key_str` to an action, so binding
                // dispatch is tried before falling back to PTY byte encoding.
                if keymap_dispatch::terminal_input_passthrough_binding(&key_str, &action) {
                    // When: this `action` is a passthrough binding, so the key
                    // belongs to the terminal and the next spelling is tried.
                    continue;
                }
                if self.run_action(&action) {
                    // When: `run_action` consumed the chord, so the caller gets
                    // the action and no encoded bytes reach the PTY.
                    if simulate_drain && self.pending_new_window {
                        self.pending_new_window = false;
                    }
                    return (Some(action), None);
                }
            }
        }
        let kitty_flags = self
            .active_pane()
            .map(|pane| pane.kitty_flags.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        let app_cursor = self
            .active_pane()
            .map(|pane| pane.app_cursor_keys.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false);
        (None, encode_logical(key, mods, kitty_flags, app_cursor))
    }

    fn write_to_pane(&mut self, pane_id: u64, bytes: Vec<u8>) {
        // The keystroke / broadcast / encoded-input path flows through the
        // winit-agnostic `AppStateMachine`. The reducer translates
        // `AppIntent::PtyWrite` into `AppEffect::PtyWrite { pane, data }`, and
        // `dispatch_pty_write_effect` is the boundary method that performs the
        // actual bounded PTY input enqueue.
        let intent = sonicterm_app_core::AppIntent::PtyWrite {
            pane: sonicterm_app_core::PaneId(pane_id),
            bytes: bytes::Bytes::from(bytes),
        };
        // Broadcast fan-out resolves its receiver set before calling this
        // method. A transient machine keeps the pure PtyWrite reduction local
        // while `&mut self` arms the accepted-input foreground probe.
        let mut transient =
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
        for effect in transient.handle(intent) {
            self.dispatch_pty_write_effect(&effect);
        }
    }

    /// Boundary handler for [`sonicterm_app_core::AppEffect::PtyWrite`].
    ///
    /// Resolves the pane id back to a live [`PtyHandle`] in any terminal
    /// window and forwards the bytes.
    pub(crate) fn dispatch_pty_write_effect(&mut self, effect: &sonicterm_app_core::AppEffect) {
        if let sonicterm_app_core::AppEffect::PtyWrite { pane, data } = effect {
            // When: the effect is `PtyWrite`, so `pane` and `data` name a live
            // target to resolve before any bytes are enqueued.
            let pane_id = pane.0;
            let bytes = data.to_vec();
            // Test-only ledger: skipped entirely in production so we don't
            // lock+clone+push on every PTY write (— unbounded
            // growth + per-keystroke overhead over a long session).
            if self.pty_write_log_enabled {
                self.test_pty_writes.lock().push((pane_id, bytes.clone()));
            }
            let Some(p) = self.pane_by_id(pane_id) else {
                // When: `pane_by_id` resolves nothing, so the pane closed before
                // its bytes were enqueued and they have nowhere to land.
                return;
            };
            let queued = p.pty.as_ref().is_some_and(|pty| {
                Self::queue_pty_input(self.event_loop_proxy.as_ref(), pty, bytes)
            });
            #[cfg(windows)]
            if queued {
                // Accepted PTY input can launch a silent command, so sample its process after launch settles.
                self.arm_foreground_probe_after_input(Instant::now());
            }
            #[cfg(not(windows))]
            let _ = queued;
        }
    }

    fn queue_pty_input(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pty: &sonicterm_io::pty::PtyHandle,
        bytes: Vec<u8>,
    ) -> bool {
        if let Err(error) = pty.send_input_nonblocking(bytes) {
            // When: `send_input_nonblocking` refused the bytes, so the writer is
            // gone or saturated and the input is surfaced rather than retried.
            let event = pty_input_rejected_event(error);
            let UserEvent::PtyInputRejected { bytes, reason } = &event else {
                unreachable!("PTY rejection helper must build a rejection event");
            };
            tracing::warn!(
                rejected_bytes = bytes.len(),
                %reason,
                "terminal input was not queued because the PTY writer is unavailable or saturated"
            );
            if let Some(proxy) = proxy {
                // When: a `proxy` exists, so the rejection can reach the event
                // loop; input is never re-sent, only reported.
                let _ = proxy.send_event(event);
            }
            return false;
        }
        true
    }

    /// Generic boundary dispatcher for an Effect batch produced by the
    /// state machine. The leaf classes (PTY,
    /// clipboard set, OpenURL, Quit, Render-reasons that map to a
    /// redraw request) are handled here. Non-leaf classes (WindowOpen,
    /// ChildSpawn, MenubarUpdate, …) fall through to a tracing debug
    /// rather than being dispatched.
    pub(crate) fn dispatch_effects(
        &mut self,
        effects: smallvec::SmallVec<[sonicterm_app_core::AppEffect; 4]>,
    ) {
        use sonicterm_app_core::AppEffect;
        for effect in effects {
            match effect {
                AppEffect::PtyWrite { .. } => {
                    self.dispatch_pty_write_effect(&effect);
                }
                AppEffect::ClipboardSet { text } => {
                    // When: the effect is `ClipboardSet`, so nonempty `text` is
                    // written and empty text stays a no-op contract sentinel.
                    if !text.is_empty() {
                        // When: `text` carries a payload, so it replaces the
                        // clipboard; empty text would clear what the user copied.
                        if let Some(cb) = self.clipboard.as_mut() {
                            // When: a `cb` handle exists, so the write is
                            // attempted and a backend refusal is not fatal here.
                            let _ = cb.set_text(text);
                        }
                    }
                    // Empty text sentinel for CopySelection:
                    // the boundary's existing `copy_selection` already
                    // resolved the selection; the sentinel exists so
                    // the Intent→Effect contract is observable in
                    // tests, and carries no text payload.
                }
                AppEffect::OpenURL { url } => {
                    if let Err(error) = open_url_effect(&url) {
                        tracing::warn!(%error, "failed to open URL effect");
                    }
                }
                AppEffect::Quit => {
                    self.pending_exit = true;
                }
                AppEffect::Render { .. } | AppEffect::RenderDirtyRect { .. } => {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                        self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                // ── PTY class ─────────────────────────────────────────
                //
                // PtyClose: the per-pane `PtyHandle::Drop` impl already
                // SIGKILLs the child (CLAUDE.md §4 land-mine). Removing
                // the pane entry from `WindowState.panes` is what
                // actually triggers the drop. We try the main window
                // first; if not found, scan child windows.
                AppEffect::PtyClose { pane } => {
                    let pane_id = pane.0;
                    let closed = self.close_pty_pane(pane_id);
                    tracing::debug!(target: "state_machine", pane = pane_id, closed, "dispatch_effects: PtyClose");
                }
                // ChildExitPropagate: observability — the renderer's
                // poll loop already noticed the child exit and updated
                // the per-pane status. Surface a structured log so the
                // session-restore layer (post-v1.0) can correlate.
                AppEffect::ChildExitPropagate { pane, status } => {
                    tracing::info!(target: "state_machine", pane = pane.0, status, "child exit propagated");
                }
                // ChildSpawn: record-only at the boundary. Production
                // pane spawning flows through `App::spawn_pane` /
                // `spawn_tab_in_child`, which constructs the PTY
                // directly; the effect here is the observable contract.
                AppEffect::ChildSpawn { pane, argv0 } => {
                    tracing::debug!(target: "state_machine", pane = pane.0, %argv0, "dispatch_effects: ChildSpawn (record-only)");
                }
                // ── OS drag class ────────────────────────────────────
                //
                // The actual platform OS drag is initiated by the
                // tear-out / tab-drag path which talks directly to the
                // platform backend (NSPasteboard / OLE). The reducer
                // emits OsDragStart for observability + future
                // session-restore.
                AppEffect::OsDragStart { src_window, payload_tab } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        tab = payload_tab,
                        "dispatch_effects: OsDragStart (platform path owns the actual drag)"
                    );
                }
                // OsDragEnd: settle the pending-drag table so the
                // tear-out boundary can finalize. The os_drag layer's
                // PendingDragOutcome already tracks the outcome
                // bilaterally; we surface a log here.
                AppEffect::OsDragEnd { src_window, committed } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        committed,
                        "dispatch_effects: OsDragEnd"
                    );
                }
                // ── Clipboard / notification side channels ───────────
                //
                // ClipboardRequest: async paste handshake. The actual
                // read happens through `clipboard.get_text()` at the
                // boundary's paste path; here we surface the request.
                AppEffect::ClipboardRequest { window, bracketed } => {
                    if let Some(cb) = self.clipboard.as_mut() {
                        if let Ok(text) = cb.get_text() {
                            tracing::debug!(
                                target: "state_machine",
                                window = window.0,
                                bracketed,
                                len = text.len(),
                                "dispatch_effects: ClipboardRequest fulfilled"
                            );
                        }
                    }
                }
                // Notification: route through the existing
                // `notify_command_done` path (test capture friendly).
                AppEffect::Notification { title, body } => {
                    // When: the effect is `Notification`, so `title` and `body`
                    // are joined into the one line the notifier accepts.
                    let combined = if title.is_empty() { body } else { format!("{title}: {body}") };
                    notify_command_done(combined);
                }
                // ── Window ops ───────────────────────────────────────
                //
                // WindowOpen: defer to the existing pending-new-window
                // flag drained by event_loop on the next tick. The
                // platform-creation requires `&ActiveEventLoop` which
                // dispatch_effects doesn't carry — flagging keeps the
                // request observable without changing the dispatcher
                // signature.
                AppEffect::WindowOpen { role, initial_size } => {
                    self.pending_new_window = true;
                    tracing::debug!(
                        target: "state_machine",
                        ?role,
                        ?initial_size,
                        "dispatch_effects: WindowOpen queued (drained by event_loop)"
                    );
                }
                // WindowClose: best-effort. Without a WindowKey→WindowId
                // map (lifted in 2d), close the main window or, if it's
                // a child, the matching entry. We at minimum surface a
                // log and set pending_exit when it's the last live
                // window per the reducer's contract.
                AppEffect::WindowClose { window } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        "dispatch_effects: WindowClose (platform path closes via WindowEvent::CloseRequested)"
                    );
                }
                // WindowResize: programmatic resize. winit's
                // `set_inner_size` is the API; since `LogicalSize` here
                // is f64 cells (not pixels) per the reducer's contract,
                // emit a redraw so the boundary re-measures.
                AppEffect::WindowResize { window, size } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        w = size.width,
                        h = size.height,
                        "dispatch_effects: WindowResize (observability)"
                    );
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
                // WindowMove: record-only; OS already moved the window.
                AppEffect::WindowMove { window, pos } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        x = pos.x,
                        y = pos.y,
                        "dispatch_effects: WindowMove (record-only)"
                    );
                }
                // WindowSetTitle updates internal tab chrome only; the native
                // OS window title intentionally stays the static "SonicTerm".
                AppEffect::WindowSetTitle { window, title } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        %title,
                        "dispatch_effects: WindowSetTitle"
                    );
                }
                // TimerSchedule / TimerCancel: the boundary's redraw
                // pacing uses winit's ControlFlow::WaitUntil directly
                // . The reducer emitting these surfaces a
                // contract for future schedulers (e.g. cursor-blink
                // refactor); record-only today.
                AppEffect::TimerSchedule { id, at } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        ?at,
                        "dispatch_effects: TimerSchedule (record-only — winit ControlFlow drives pacing)"
                    );
                }
                AppEffect::TimerCancel { id } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        "dispatch_effects: TimerCancel (record-only)"
                    );
                }
                // ── Menubar ──────────────────────────────────────────
                //
                // MenubarUpdate: macOS rebuilds the NSMenu through the
                // existing `menubar_bridge`; Windows is a log-only no-op because
                // the platform path owns its muda menubar directly. We surface a debug
                // log either way so the request is observable.
                AppEffect::MenubarUpdate(model) => {
                    tracing::debug!(
                        target: "state_machine",
                        items = model.items.len(),
                        "dispatch_effects: MenubarUpdate (platform path owns NSMenu/muda mutation)"
                    );
                }
                // ── Log ──────────────────────────────────────────────
                //
                // LogEvent: forward to tracing at the requested level.
                AppEffect::LogEvent { level, target, msg } => {
                    use sonicterm_app_core::LogLevel;
                    // `target` is &'static str from the reducer but
                    // tracing's `target:` slot needs a literal at the
                    // call site, so capture both as fields instead.
                    match level {
                        LogLevel::Trace => {
                            tracing::trace!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Debug => {
                            tracing::debug!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Info => {
                            tracing::info!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Warn => {
                            tracing::warn!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Error => {
                            tracing::error!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                    }
                }
                // `AppEffect` is #[non_exhaustive]; future variants
                // surface here as an unrouted log until wired.
                _ => {
                    tracing::trace!(target: "state_machine", "dispatch_effects: unrouted effect {:?}", effect);
                }
            }
        }
    }

    pub(super) fn next_main_tab(&mut self) -> bool {
        let Some(tabs) = self.main_tabs_mut() else {
            // When: `main_tabs_mut` resolves nothing, so no tab bar exists to
            // advance and the caller must not be told focus moved.
            return false;
        };
        tabs.next();
        self.resize_visible_panes();
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        true
    }

    pub(super) fn prev_main_tab(&mut self) -> bool {
        let Some(tabs) = self.main_tabs_mut() else {
            // When: `main_tabs_mut` resolves nothing, so no tab bar exists to
            // step backward through.
            return false;
        };
        tabs.prev();
        self.resize_visible_panes();
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        true
    }

    pub(super) fn activate_main_tab(&mut self, idx: usize) -> bool {
        let Some(tabs) = self.main_tabs_mut() else {
            // When: `main_tabs_mut` resolves nothing, so `idx` names no tab that
            // could be brought to the front.
            return false;
        };
        tabs.activate(idx);
        self.resize_visible_panes();
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        true
    }

    pub(super) fn activate_last_main_tab(&mut self) -> bool {
        let Some(last) = self.main_tabs().map(|t| t.len().saturating_sub(1)) else {
            // When: `main_tabs` resolves nothing, so there is no `last` index to
            // activate.
            return false;
        };
        self.activate_main_tab(last)
    }

    fn close_pty_pane(&mut self, pane_id: u64) -> bool {
        let mut closed = false;
        let mut resize_main = false;
        let mut redraw_main = false;

        if let Some(ws) = self.main_mut() {
            // When: `main_mut` resolves a window, so its tabs are searched for
            // the pane before any child window is considered.
            let active_tab = ws.tabs.active_index();
            for (tab_idx, st) in ws.tab_states.iter_mut().enumerate() {
                let leaves = st.tree.leaves();
                if !leaves.contains(&pane_id) {
                    // When: this tab's `leaves` exclude `pane_id`, so its split
                    // tree does not hold the pane being closed.
                    continue;
                }
                if leaves.len() > 1 && st.tree.close(pane_id) {
                    if st.active_pane == pane_id {
                        st.active_pane =
                            leaves.into_iter().find(|id| *id != pane_id).unwrap_or(st.active_pane);
                        // The search was scanning the grid that just went
                        // away. Its matches, their coordinates, and the
                        // revision it recorded all describe that grid.
                        if let Some(search) = st.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                    }
                    if tab_idx == active_tab {
                        resize_main = true;
                        redraw_main = true;
                    }
                }
                break;
            }
            closed = ws.panes.remove(&pane_id).is_some();
        }

        if resize_main {
            self.resize_visible_panes();
        }
        if redraw_main {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        if closed {
            // When: the main sweep already `closed` the pane, so scanning child
            // windows would only rediscover work that is done.
            return true;
        }

        for ws in self.windows.values_mut() {
            let mut resize_child = false;
            let mut redraw_child = false;
            let active_tab = ws.tabs.active_index();
            for (tab_idx, st) in ws.tab_states.iter_mut().enumerate() {
                let leaves = st.tree.leaves();
                if !leaves.contains(&pane_id) {
                    // When: this tab's `leaves` exclude `pane_id`, so this child's
                    // split tree does not hold the pane being closed.
                    continue;
                }
                if leaves.len() > 1 && st.tree.close(pane_id) {
                    if st.active_pane == pane_id {
                        st.active_pane =
                            leaves.into_iter().find(|id| *id != pane_id).unwrap_or(st.active_pane);
                        // The search was scanning the grid that just went
                        // away. Its matches, their coordinates, and the
                        // revision it recorded all describe that grid.
                        if let Some(search) = st.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                    }
                    if tab_idx == active_tab {
                        resize_child = true;
                        redraw_child = true;
                    }
                }
                break;
            }
            if ws.panes.remove(&pane_id).is_some() {
                // When: `panes` actually held `pane_id`, so its removal is what
                // drops the PTY and the surviving splits need re-laying out.
                if resize_child {
                    child_window::resize_visible_panes_in_child(ws);
                }
                if redraw_child {
                    ws.request_redraw();
                }
                return true;
            }
        }

        false
    }

    /// Drive a single [`AppIntent`] through the state machine and
    /// dispatch the resulting Effects through the boundary layer.
    /// Wires the winit-flavoured shell
    /// into the winit-agnostic reducer.
    pub fn dispatch_intent(&mut self, intent: sonicterm_app_core::AppIntent) {
        let effects = self.machine.handle(intent);
        self.dispatch_effects(effects);
    }

    fn broadcast_from(&mut self, active_id: u64, bytes: Vec<u8>) {
        let BroadcastState::On { source_pane, .. } = self.broadcast else {
            // When: `broadcast` is not `On`, so there is no fan-out group and the
            // bytes belong to the focused pane alone.
            return;
        };
        if active_id != source_pane {
            // When: `active_id` is not the `source_pane` that armed the
            // broadcast, so typing here must not fan out to the group.
            return;
        }
        let receivers = self.broadcast_receivers();
        for pane_id in receivers {
            self.write_to_pane(pane_id, bytes.clone());
        }
    }

    pub(crate) fn broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        let BroadcastState::On { scope, source_pane } = self.broadcast else {
            // When: `broadcast` is not `On`, so no `scope` or `source_pane`
            // defines a group and the receiver set is empty.
            return Default::default();
        };
        self.broadcast_receivers_for(scope, source_pane)
    }

    fn broadcast_receivers_for(
        &self,
        scope: BroadcastScope,
        source_pane: u64,
    ) -> std::collections::BTreeSet<u64> {
        let mut receivers = std::collections::BTreeSet::new();
        for ws in self.windows.values() {
            match scope {
                BroadcastScope::Tab => {
                    // When: `scope` is `Tab`, so only panes sharing the source's
                    // own tab receive the fan-out.
                    if let Some((tab_idx, _)) = ws
                        .tab_states
                        .iter()
                        .enumerate()
                        .find(|(_, tab)| tab.tree.leaves().contains(&source_pane))
                    {
                        // When: a tab's `leaves` hold `source_pane`, so that tab's
                        // panes are the receiver set for this window.
                        receivers.extend(sonicterm_ui::broadcast::receiving_panes(
                            &ws.tab_states,
                            scope,
                            source_pane,
                            tab_idx,
                        ));
                        break;
                    }
                }
                BroadcastScope::AllTabs => {
                    receivers.extend(sonicterm_ui::broadcast::receiving_panes(
                        &ws.tab_states,
                        scope,
                        source_pane,
                        ws.tabs.active_index(),
                    ));
                }
            }
        }
        receivers
    }

    /// Test-only: active broadcast source pane, if broadcast is enabled.
    #[doc(hidden)]
    pub fn __test_broadcast_source(&self) -> Option<u64> {
        match self.broadcast {
            BroadcastState::On { source_pane, .. } => Some(source_pane),
            BroadcastState::Off => None,
        }
    }

    /// Test-only: receiver panes under the current broadcast state.
    #[doc(hidden)]
    pub fn __test_broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        self.broadcast_receivers()
    }

    /// Test-only: clear the PTY write ledger before a broadcast assertion.
    #[doc(hidden)]
    pub fn __test_enable_pty_write_log(&mut self) {
        self.pty_write_log_enabled = true;
        self.test_pty_writes.lock().clear();
    }

    /// Test-only: snapshot logged `(pane_id, bytes)` PTY writes.
    #[doc(hidden)]
    pub fn __test_pty_write_log(&self) -> Vec<(u64, Vec<u8>)> {
        self.test_pty_writes.lock().clone()
    }

    /// Test-only: drive the same write + broadcast fan-out as normal input.
    #[doc(hidden)]
    pub fn __test_write_to_pane_with_broadcast(&mut self, pane_id: u64, bytes: Vec<u8>) {
        self.write_to_pane(pane_id, bytes.clone());
        self.broadcast_from(pane_id, bytes);
    }

    /// Test-only: child render pane ids with the broadcast receiver flag that
    /// would be passed into `sonicterm_render_model::PaneRender`.
    #[doc(hidden)]
    pub fn __test_child_broadcast_render_flags(&self, id: WindowId) -> Option<Vec<(u64, bool)>> {
        let child = self.windows.get(&id)?;
        let tab_idx = child.tabs.active_index();
        let panes = child.tab_states.get(tab_idx)?.tree.leaves();
        let receivers = self.broadcast_receivers();
        Some(panes.into_iter().map(|pane| (pane, receivers.contains(&pane))).collect())
    }

    /// Test-only: how many tabs the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_tab_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|c| c.tabs.len())
    }

    /// Test-only: how many panes the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_pane_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|c| c.panes.len())
    }

    /// Test-only: pane ids owned by the named child window.
    #[doc(hidden)]
    pub fn __test_child_pane_ids(&self, id: WindowId) -> Option<Vec<u64>> {
        self.windows.get(&id).map(|c| c.panes.keys().copied().collect())
    }

    /// Test-only: install the headless pane-viewport seam on the main window
    /// so resize wiring runs without a renderer.
    #[doc(hidden)]
    pub fn __test_set_main_pane_viewport(
        &mut self,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        self.__test_synthetic_main();
        self.test_viewport_override = Some((outer, cell_w, cell_h));
        true
    }

    /// Test-only: drive main-window active-tab pane resizing through the same
    /// helper used by production window resize and tab activation.
    #[doc(hidden)]
    pub fn __test_resize_visible_panes(&mut self) {
        self.resize_visible_panes();
    }

    /// Test-only: activate a main tab through the same production helper used
    /// by keyboard/mouse tab activation.
    #[doc(hidden)]
    pub fn __test_invoke_activate_main_tab(&mut self, idx: usize) -> bool {
        self.activate_main_tab(idx)
    }

    /// Test-only: install the headless per-window pane-viewport seam on a child
    /// so the split/close resize wiring runs without a renderer.
    #[doc(hidden)]
    pub fn __test_set_child_pane_viewport(
        &mut self,
        id: WindowId,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        match self.windows.get_mut(&id) {
            Some(c) => {
                c.test_pane_viewport = Some((outer, cell_w, cell_h));
                true
            }
            None => false,
        }
    }

    /// Test-only: split the active pane of the named child window to the right,
    /// driving the same `split_active_pane_in_child` path the keymap uses.
    #[doc(hidden)]
    pub fn __test_child_split_active_right(&mut self, id: WindowId) -> bool {
        self.split_active_pane_in_child(id, sonicterm_cfg::keymap::Direction::Right)
    }

    /// Test-only: grid (cols, rows) of a specific pane in the named child.
    #[doc(hidden)]
    pub fn __test_child_pane_grid_size(&self, id: WindowId, pane_id: u64) -> Option<(u16, u16)> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cols, grid.rows))
    }

    /// Test-only: the active pane id in the named child's active tab.
    #[doc(hidden)]
    pub fn __test_child_active_pane(&self, id: WindowId) -> Option<u64> {
        let child = self.windows.get(&id)?;
        let tab_idx = child.tabs.active_index();
        child.tab_states.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: `true` when the named child pane's scrollbar is currently
    /// inside its idle-visible window (i.e. `mark_active` fired recently).
    /// Used to assert wheel-scroll / view_top jumps light the auto-hide bar
    /// on torn-out windows the same way they do on the main window.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_active(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        let st = self.windows.get(&id)?.scrollbar_vis.get(&pane_id)?;
        let idle_ms = match st.last_active {
            Some(t) => t.elapsed().as_millis() as u64,
            None => u64::MAX,
        };
        Some(idle_ms < scrollbar_visibility::IDLE_HIDE_MS)
    }

    /// Test-only: whether the child pane is currently marked as right-edge hovered.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_near_edge(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        self.windows.get(&id)?.scrollbar_vis.get(&pane_id).map(|st| st.mouse_near_right_edge)
    }

    /// Test-only: clear child scrollbar hover state, mirroring CursorLeft.
    #[doc(hidden)]
    pub fn __test_clear_child_scrollbar_hover(&mut self, id: WindowId) -> bool {
        self.clear_scrollbar_hover_in_child(id)
    }

    /// Test-only: write a child pane's `viewport_top_abs` through the same
    /// production path the scrollbar uses (`set_child_pane_view_top`), so a
    /// test can drive a scroll and observe the visibility side effect.
    #[doc(hidden)]
    pub fn __test_child_set_pane_view_top(
        &mut self,
        id: WindowId,
        pane_id: u64,
        view_top: u64,
        live_top: u64,
    ) {
        self.set_child_pane_view_top(id, pane_id, view_top, live_top);
    }

    /// Test-only: set the last cursor position for a synthetic child window.
    #[doc(hidden)]
    pub fn __test_set_child_cursor_pos(&mut self, id: WindowId, x: f64, y: f64) -> bool {
        match self.windows.get_mut(&id) {
            Some(c) => {
                c.cursor_pos = (x, y);
                true
            }
            None => false,
        }
    }

    /// Test-only: refresh a child window's scrollbar hover state from its last
    /// cursor position, mirroring the production CursorMoved branch.
    #[doc(hidden)]
    pub fn __test_refresh_child_scrollbar_hover_from_cursor(&mut self, id: WindowId) -> bool {
        self.refresh_scrollbar_hover_from_cursor_in_child(id)
    }

    /// Test-only: seed a synthetic child WindowState without constructing a
    /// real winit Window / GpuRenderer. The pane/tab bookkeeping mirrors a
    /// tear-out child, but `window` and `renderer` stay `None` so cargo-test
    /// can exercise App-level multi-window ownership invariants headlessly.
    #[doc(hidden)]
    pub fn __test_seed_child_window(&mut self, titles: &[&str]) -> WindowId {
        self.__test_synthetic_main();
        let id = next_synthetic_child_window_id();
        let mut tabs = TabBar::new();
        let mut tab_states = Vec::new();
        let mut panes = HashMap::new();
        for title in titles {
            let pane_id = next_pane_id();
            let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
            panes.insert(pane_id, PaneState::new(parser, None));
            tabs.push(Tab::new(*title));
            tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        let child = WindowState {
            // Registered when the window is inserted.
            owner: None,
            role: WindowRole::Terminal,
            window: None,
            renderer: None,
            tabs,
            tab_states,
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            pointer_gesture: None,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            path_probe: path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(id, child);
        id
    }

    /// Test-only: inspect drag-gesture residue on a specific
    /// child window so an integration test can assert
    /// [`Self::cancel_drag_session`] clears EVERY window's state, not
    /// just the main one.
    #[doc(hidden)]
    pub fn __test_child_pressed_tab(&self, id: WindowId) -> Option<Option<usize>> {
        self.windows.get(&id).map(|ws| ws.pressed_tab)
    }

    /// Test seam: whether a window is tracking a held mouse button.
    ///
    /// `None` when `id` names no tracked window, which distinguishes an
    /// unknown window from one with no button held.
    #[doc(hidden)]
    pub fn __test_child_mouse_down(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.mouse_down)
    }

    /// Test seam: whether a window has a tab drag in progress.
    ///
    /// `None` when `id` names no tracked window.
    #[doc(hidden)]
    pub fn __test_child_has_drag_session(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.drag_session.is_some())
    }

    /// Test seam: whether a window is a drop target for the current drag.
    ///
    /// `None` when `id` names no tracked window.
    #[doc(hidden)]
    pub fn __test_child_has_drag_target(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.drag_target.is_some())
    }

    /// Test-only: seed the headless drag-chip
    /// marker on a window so a subsequent [`Self::cancel_drag_session`]
    /// can be observed to have cleared it. Returns `false` if the window
    /// id is unknown. The marker is the cross-platform stand-in for
    /// `renderer.set_drag_chip(_)` on `renderer: None` test windows —
    /// production code flips it in the same loop iteration as the real
    /// renderer call, so the assertion fails if the per-window iteration
    /// is ever removed.
    #[doc(hidden)]
    pub fn __test_set_window_drag_chip_marker(&mut self, id: WindowId, present: bool) -> bool {
        if let Some(ws) = self.windows.get_mut(&id) {
            ws.test_drag_chip_marker = Some(present);
            true
        } else {
            // When: `windows` tracks no entry for this id, so no drag-chip marker
            // could be seeded and the caller is told the seam did nothing.
            false
        }
    }

    /// Test-only: read the drag-chip marker for
    /// a window. `None` ⇒ window absent OR marker never seeded;
    /// `Some(true)` ⇒ marker set & not yet cleared by cancel;
    /// `Some(false)` ⇒ marker was set and cancel ran on this window.
    #[doc(hidden)]
    pub fn __test_window_drag_chip_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|ws| ws.test_drag_chip_marker)
    }

    /// Test-only convenience: same as
    /// [`Self::__test_set_window_drag_chip_marker`] but for the
    /// synthetic main window (id from [`synthetic_main_window_id`]).
    #[doc(hidden)]
    pub fn __test_set_main_drag_chip_marker(&mut self, present: bool) -> bool {
        self.__test_set_window_drag_chip_marker(synthetic_main_window_id(), present)
    }

    /// Test-only convenience: read the main window's drag-chip marker.
    #[doc(hidden)]
    pub fn __test_main_drag_chip_marker(&self) -> Option<bool> {
        self.__test_window_drag_chip_marker(synthetic_main_window_id())
    }

    /// Test-only: seed drag-gesture residue on a specific child
    /// window — `pressed_tab`, `mouse_down`, and a synthetic
    /// `drag_session` — without driving a real winit pointer event
    /// sequence. Returns true on success.
    #[doc(hidden)]
    pub fn __test_seed_child_drag_residue(
        &mut self,
        id: WindowId,
        pressed_tab: Option<usize>,
        mouse_down: bool,
        with_drag_session: bool,
    ) -> bool {
        let Some(ws) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so there is no child
            // state to seed drag residue onto.
            return false;
        };
        ws.pressed_tab = pressed_tab;
        ws.mouse_down = mouse_down;
        if with_drag_session {
            ws.drag_session = Some(crate::tab_drag::DragSession::new(0, (0.0, 0.0)));
        }
        true
    }

    /// Test-only: install a frontmost child id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// `frontmost_window` subsumes a separate focused-child field;
    /// this kept the old name so the existing regression tests don't
    /// need touching, but it now drives the unified tracker.
    #[doc(hidden)]
    pub fn __test_set_focused_child(&mut self, id: Option<WindowId>) {
        self.__test_synthetic_main();
        self.frontmost_window = id;
    }

    /// Test-only: read back the current frontmost-child id.
    /// returns `Some(id)` when `frontmost_window` points
    /// at a non-main entry, mirroring the old `focused_child` semantics.
    #[doc(hidden)]
    pub fn __test_focused_child(&self) -> Option<WindowId> {
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        }
    }

    /// Test-only: read back the current `frontmost_window`.
    #[doc(hidden)]
    pub fn __test_frontmost_window(&self) -> Option<WindowId> {
        self.frontmost_window
    }

    /// Test-only: install a `frontmost_window` id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// Used by regression tests to assert that
    /// keymap-dispatched actions route to the right window's tab vec.
    #[doc(hidden)]
    pub fn __test_set_frontmost_window(&mut self, id: Option<WindowId>) {
        self.frontmost_window = id;
    }

    /// Test-only: resolve a chord string through the App's keymap.
    /// Used by `child_window_tab_actions_dispatch.rs` to
    /// pin down that the chords the child-window handler now dispatches
    /// (cmd+1, cmd+2, cmd+Right, cmd+Left) actually resolve to their
    /// expected Action variants.
    #[doc(hidden)]
    pub fn __test_keymap_lookup(&self, keys: &str) -> Option<Action> {
        self.keymap.lookup(keys).cloned()
    }

    /// Test-only: read the window the command palette is currently
    /// attached to. `None` = main window OR closed; `Some(id)` = that
    /// child window. Used by overlay-routing regression tests.
    #[doc(hidden)]
    pub fn __test_palette_attached_window(&self) -> Option<WindowId> {
        self.palette_attached_window
    }

    /// Test-only: whether the command palette is currently open.
    #[doc(hidden)]
    pub fn __test_palette_open(&self) -> bool {
        self.command_palette.is_open()
    }

    /// Test-only: command palette query text.
    #[doc(hidden)]
    pub fn __test_palette_query(&self) -> &str {
        self.command_palette.query()
    }

    /// Test-only: command palette caret byte offset.
    #[doc(hidden)]
    pub fn __test_palette_cursor(&self) -> usize {
        self.command_palette.cursor()
    }

    /// Test-only: replace the command-palette query and refresh its selection.
    #[doc(hidden)]
    pub fn __test_set_palette_query(&mut self, query: &str) {
        self.command_palette.set_query(query);
    }

    /// Test-only: drive command-palette core editing without constructing a
    /// platform-private winit `KeyEvent`.
    #[doc(hidden)]
    pub fn __test_command_palette_text_edit(
        &mut self,
        key: &winit::keyboard::Key,
        modifiers: ModifiersState,
    ) -> bool {
        if !self.command_palette.is_open() || self.palette_ime_is_composing() {
            // When: `command_palette` is shut, or an IME preedit owns its input,
            // so a core text edit would corrupt composition or edit nothing.
            return self.command_palette.is_open();
        }
        if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::TabColor
        {
            // When: `TabColor` mode owns the keystroke, so it counts as handled
            // without editing the query text behind the picker.
            return true;
        }
        let Some(edit) = text_edit::core_text_edit_for_key(key, modifiers) else {
            // When: `core_text_edit_for_key` maps this key to no edit, so the
            // query is untouched and the key is reported unhandled.
            return false;
        };
        self.command_palette.apply_text_edit(edit);
        self.request_redraw_for_overlay(self.palette_attached_window);
        true
    }

    /// Test-only: enter tab-rename mode with a known value.
    #[doc(hidden)]
    pub fn __test_start_rename_tab(&mut self, title: &str) {
        self.command_palette.start_rename_tab(title);
    }

    /// Test-only: drive command-palette key handling by logical key.
    #[doc(hidden)]
    pub fn __test_command_palette_handle_key(&mut self, key: &winit::keyboard::Key) -> bool {
        self.command_palette_handle_logical_key(key)
    }

    /// Test-only: drive command-palette IME handling.
    #[doc(hidden)]
    pub fn __test_command_palette_handle_ime(&mut self, event: &winit::event::Ime) -> bool {
        self.command_palette_handle_ime(event)
    }

    /// Test-only: describe where the main window will anchor the OS IME
    /// candidate area.
    #[doc(hidden)]
    pub fn __test_main_ime_candidate_anchor_kind(&self) -> &'static str {
        if self.command_palette.is_open() && self.palette_attached_window.is_none() {
            "palette"
        } else {
            // When: `command_palette` is shut, or `palette_attached_window` names
            // a child, so the main window's IME anchor is the terminal grid.
            "terminal"
        }
    }

    /// Test-only: read the main window notification bubble message.
    #[doc(hidden)]
    pub fn __test_main_notification_message(&self) -> Option<&str> {
        self.main().and_then(|ws| ws.notification.as_ref()).map(|bubble| bubble.message.as_str())
    }

    /// Test-only: whether the main notification is ongoing.
    #[doc(hidden)]
    pub fn __test_main_notification_ongoing(&self) -> Option<bool> {
        self.main()
            .and_then(|ws| ws.notification.as_ref())
            .map(|bubble| bubble.expires_at.is_none())
    }

    /// Test-only: install a notification with a specific expiration.
    #[doc(hidden)]
    pub fn __test_show_notification_until(
        &mut self,
        kind: FrontmostKind,
        level: NotificationLevel,
        message: &str,
        expires_at: Option<std::time::Instant>,
    ) {
        self.show_notification_for_kind_until(kind, level, message.to_string(), expires_at);
    }

    /// Test-only: run notification expiry and return the next wake time.
    #[doc(hidden)]
    pub fn __test_expire_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.expire_notifications(now)
    }

    /// Test-only: read a child window notification bubble message.
    #[doc(hidden)]
    pub fn __test_child_notification_message(&self, id: WindowId) -> Option<&str> {
        self.windows
            .get(&id)
            .and_then(|ws| ws.notification.as_ref())
            .map(|bubble| bubble.message.as_str())
    }

    /// Test-only invoker for `open_search_in_child`. Mirrors the
    /// pattern used by `__test_invoke_close_active_tab_in_child` so
    /// integration tests can assert the stale-id no-op contract for
    /// overlay routing.
    #[doc(hidden)]
    pub fn __test_invoke_open_search_in_child(&mut self, id: WindowId) -> bool {
        self.open_search_in_child(id)
    }

    /// Test-only: open main search and install a known query.
    #[doc(hidden)]
    pub fn __test_set_main_search_query(&mut self, query: &str) -> bool {
        self.open_search();
        let Some(ws) = self.main_mut() else {
            // When: `main_mut` resolves nothing, so no window holds the search
            // session the query was meant to seed.
            return false;
        };
        let i = ws.tabs.active_index();
        let Some(tab) = ws.tab_states.get_mut(i) else {
            // When: `tab_states` has no entry at the active index `i`, so no tab
            // carries the search state to install into.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the seam refuses rather
            // than fabricating a session the user never opened.
            return false;
        };
        let Some(pane) = ws.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so there is no grid
            // for `set_query` to match the term against.
            return false;
        };
        search.set_query(query, pane.parser.lock().grid());
        true
    }

    /// Test-only: install a known query in an open child search field.
    #[doc(hidden)]
    pub fn __test_set_child_search_query(&mut self, id: WindowId, query: &str) -> bool {
        if !self.open_search_in_child(id) {
            // When: `open_search_in_child` could not open search for this id, so
            // there is no session for the query to land in.
            return false;
        }
        let Some(ws) = self.windows.get_mut(&id) else {
            // When: `windows` no longer tracks this id, so the child vanished
            // between opening search and installing the query.
            return false;
        };
        let i = ws.tabs.active_index();
        let Some(tab) = ws.tab_states.get_mut(i) else {
            // When: `tab_states` has no entry at the active index `i`, so the
            // child carries no tab to install the query into.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the seam refuses rather
            // than fabricating a session.
            return false;
        };
        let Some(pane) = ws.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so there is no grid
            // for `set_query` to match against.
            return false;
        };
        search.set_query(query, pane.parser.lock().grid());
        true
    }

    /// Test-only: apply a core edit to main or child search through the same
    /// shared state operation used by production routing.
    #[doc(hidden)]
    pub fn __test_search_text_edit(
        &mut self,
        id: Option<WindowId>,
        key: &winit::keyboard::Key,
        modifiers: ModifiersState,
    ) -> bool {
        let Some(edit) = text_edit::search_text_edit_for_key(key, modifiers) else {
            // When: `search_text_edit_for_key` maps this key to no edit, so the
            // search term is untouched and the key is reported unhandled.
            return false;
        };
        let target = id.or(self.main_window_id);
        let Some(target) = target else {
            // When: neither the supplied id nor `main_window_id` yields a
            // `target`, so no window owns the search this edit would change.
            return false;
        };
        let Some(ws) = self.windows.get_mut(&target) else {
            // When: `windows` no longer tracks `target`, so the window closed
            // between resolving it and applying the edit.
            return false;
        };
        if ws.ime.is_composing() {
            // When: `ime` is mid-composition, so the key belongs to the preedit
            // and a core edit would cut the composition in half.
            return true;
        }
        let i = ws.tabs.active_index();
        let Some(tab) = ws.tab_states.get_mut(i) else {
            // When: `tab_states` has no entry at the active index `i`, so no tab
            // holds the search this edit would change.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the edit is refused
            // rather than opening a session the user did not ask for.
            return false;
        };
        let Some(pane) = ws.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so re-matching the
            // term has no grid to search.
            return false;
        };
        search.apply_text_edit(edit, pane.parser.lock().grid());
        true
    }

    /// Test-only: read main or child search query and caret.
    #[doc(hidden)]
    pub fn __test_search_query_cursor(&self, id: Option<WindowId>) -> Option<(&str, usize)> {
        let target = id.or(self.main_window_id)?;
        let ws = self.windows.get(&target)?;
        let search = ws.tab_states.get(ws.tabs.active_index())?.search.as_ref()?;
        Some((search.query.as_str(), search.cursor()))
    }

    /// Test-only: seed main IME preedit state.
    #[doc(hidden)]
    pub fn __test_set_main_ime_preedit(&mut self, text: &str) -> bool {
        let Some(ws) = self.main_mut() else {
            // When: `main_mut` resolves nothing, so no window holds the IME state
            // this preedit would seed.
            return false;
        };
        ws.ime.handle_preedit(text, Some((text.len(), text.len())));
        true
    }

    /// Test-only: install an in-memory clipboard buffer. This avoids depending
    /// on the OS clipboard in headless integration tests while exercising the
    /// same `set_clipboard_text` / `paste_clipboard` dispatch paths.
    #[doc(hidden)]
    pub fn __test_set_memory_clipboard(&mut self, text: &str) {
        self.test_clipboard_text = Some(text.to_string());
    }

    /// Test-only: read the in-memory clipboard buffer if installed.
    #[doc(hidden)]
    pub fn __test_memory_clipboard(&self) -> Option<String> {
        self.test_clipboard_text.clone()
    }

    /// Test-only: make clipboard writes fail before either clipboard seam changes.
    #[doc(hidden)]
    pub fn __test_set_clipboard_write_failure(&mut self, enabled: bool) {
        self.test_clipboard_write_failure = enabled;
    }

    /// Test-only: drain the PTY write ledger populated by `write_to_pane`.
    #[doc(hidden)]
    pub fn __test_drain_pty_writes(&self) -> Vec<(u64, Vec<u8>)> {
        std::mem::take(&mut *self.test_pty_writes.lock())
    }

    /// Test-only: exercise file-drop path paste routing without a platform drop event.
    #[doc(hidden)]
    pub fn __test_paste_file_paths_for_kind(
        &mut self,
        kind: FrontmostKind,
        paths: Vec<std::path::PathBuf>,
    ) {
        self.paste_file_paths_for_kind(kind, paths);
    }

    /// Bind an unbound headless selection to a synthetic window's active pane,
    /// matching production mouse selection creation.
    fn bind_test_selection(
        window: &WindowState,
        selection: Option<Selection>,
    ) -> Option<Selection> {
        let mut selection = selection?;
        if selection.pane_id.is_some() {
            // When: `selection` already names a `pane_id`, so rebinding it would
            // move the caller's range onto a different pane.
            return Some(selection);
        }
        let pane_id = window.tab_states.get(window.tabs.active_index())?.active_pane;
        let pane = window.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        selection = selection.with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        Some(selection)
    }

    /// Test-only: set the synthetic main window's selection.
    #[doc(hidden)]
    pub fn __test_set_main_selection(&mut self, selection: Option<Selection>) -> bool {
        let Some(id) = self.main_window_id else {
            // When: `main_window_id` is unset, so no main window exists to carry
            // the selection.
            return false;
        };
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` no longer tracks this id, so the content state the
            // selection binds to cannot be read.
            return false;
        };
        let selection = Self::bind_test_selection(window, selection);
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window disappeared between binding and assignment, so the
            // bound selection has nowhere to be stored.
            return false;
        };
        window.selection = selection;
        true
    }

    /// Test-only: set a synthetic child window's selection.
    #[doc(hidden)]
    pub fn __test_set_child_selection(
        &mut self,
        id: WindowId,
        selection: Option<Selection>,
    ) -> bool {
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` tracks no entry for this id, so the content state
            // the selection binds to cannot be read.
            return false;
        };
        let selection = Self::bind_test_selection(window, selection);
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window disappeared between binding and assignment, so the
            // bound selection has nowhere to be stored.
            return false;
        };
        window.selection = selection;
        true
    }

    /// Test seam: give a tracked window a live winit window and renderer.
    ///
    /// Lets a test promote a synthetic headless entry into one that can render,
    /// without going through real window creation. `false` when `id` is unknown.
    #[doc(hidden)]
    pub fn __test_attach_window_renderer(
        &mut self,
        id: WindowId,
        window: Arc<Window>,
        renderer: GpuRenderer,
    ) -> bool {
        let Some(state) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so there is no state to
            // hold the window handle or its renderer.
            return false;
        };
        state.window = Some(window);
        state.renderer = Some(renderer);
        true
    }

    /// Test seam: the selection a window currently holds.
    ///
    /// The outer `None` means `id` is unknown; the inner `None` means the
    /// window is tracked but has no selection.
    #[doc(hidden)]
    pub fn __test_window_selection(&self, id: WindowId) -> Option<Option<Selection>> {
        self.windows.get(&id).map(|state| state.selection)
    }

    /// Test seam: the pane targeted by a window's real renderer flash state.
    #[doc(hidden)]
    pub fn __test_window_pane_focus_flash_target(&self, id: WindowId) -> Option<u64> {
        self.windows.get(&id)?.renderer.as_ref()?.__test_pane_focus_flash_target()
    }

    /// Test seam: one pixel of a window's software-rendered frame, as BGRA.
    ///
    /// Lets a test assert what the CPU rasterizer actually produced. `None`
    /// when the window is unknown, has no renderer, or the frame is absent.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __test_window_software_frame_pixel_bgra(
        &self,
        id: WindowId,
        x: u32,
        y: u32,
    ) -> Option<[u8; 4]> {
        self.windows.get(&id)?.renderer.as_ref()?.__test_software_frame_pixel_bgra(x, y)
    }

    /// Test seam: force the no-GPU degrade path on or off.
    ///
    /// Bypasses runtime detection so a test can exercise software-render
    /// pacing on a machine that has a working GPU.
    #[doc(hidden)]
    pub fn __test_set_software_render_degrade(&mut self, degrade: bool) {
        self.software_render_degrade = degrade;
    }

    /// Test seam: whether the main window has a redraw waiting on the gate.
    ///
    /// Lets a test assert that a redraw was coalesced rather than drawn.
    #[doc(hidden)]
    pub fn __test_main_redraw_deferred(&self) -> bool {
        self.pending_redraw
    }

    /// Test seam: backdate a window's last-render instant.
    ///
    /// Frame pacing measures elapsed time since the last render, so moving
    /// this lets a test cross a frame boundary without waiting. `false` when
    /// `id` is unknown.
    #[doc(hidden)]
    pub fn __test_set_window_last_render(&mut self, id: WindowId, last_render: Instant) -> bool {
        let Some(state) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no render timestamp
            // exists to backdate.
            return false;
        };
        state.last_render = last_render;
        true
    }

    /// Test seam: a window's cell width, cell height, and top inset.
    ///
    /// These are the metrics pane layout divides by, so a test can check
    /// geometry against the same numbers production uses. `None` when the
    /// window is unknown or has no renderer.
    #[doc(hidden)]
    pub fn __test_window_cell_geometry(&self, id: WindowId) -> Option<(f32, f32, f32)> {
        let renderer = self.windows.get(&id)?.renderer.as_ref()?;
        let (cell_w, cell_h) = renderer.cell_size();
        Some((cell_w, cell_h, renderer.top_inset()))
    }

    /// Test-only: feed bytes into a child pane's parser.
    #[doc(hidden)]
    pub fn __test_advance_child_pane_parser(
        &self,
        id: WindowId,
        pane_id: u64,
        bytes: &[u8],
    ) -> bool {
        let Some(pane) = self.windows.get(&id).and_then(|c| c.panes.get(&pane_id)) else {
            // When: neither `windows` nor its `panes` resolve the request, so the
            // bytes have no parser to advance.
            return false;
        };
        pane.parser.lock().advance(bytes);
        true
    }

    /// Test-only: clear all dirty row flags for a child pane.
    #[doc(hidden)]
    pub fn __test_clear_child_pane_dirty(&self, id: WindowId, pane_id: u64) -> bool {
        let Some(pane) = self.windows.get(&id).and_then(|c| c.panes.get(&pane_id)) else {
            // When: neither `windows` nor its `panes` resolve the request, so no
            // grid exists whose dirty rows could be cleared.
            return false;
        };
        pane.parser.lock().grid_mut().clear_dirty();
        true
    }

    /// Test-only: count dirty rows for a child pane.
    #[doc(hidden)]
    pub fn __test_child_pane_dirty_count(&self, id: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        Some(pane.parser.lock().grid().dirty_count())
    }

    /// Test-only: seed child IME preedit state.
    #[doc(hidden)]
    pub fn __test_set_child_ime_preedit(&mut self, id: WindowId, text: &str) -> bool {
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no child holds the
            // IME state this preedit would seed.
            return false;
        };
        child.ime.handle_preedit(text, Some((text.len(), text.len())));
        true
    }

    /// Test-only: read whether a child IME composition is active.
    #[doc(hidden)]
    pub fn __test_child_ime_composing(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|child| child.ime.is_composing())
    }

    /// Test-only: read whether the main window is in read-only copy mode.
    #[doc(hidden)]
    pub fn __test_main_read_only(&self) -> bool {
        self.main().and_then(|ws| ws.copy_mode.as_ref()).is_some_and(|mode| mode.is_read_only())
    }

    /// Test-only: read whether a child window is in read-only copy mode.
    #[doc(hidden)]
    pub fn __test_child_read_only(&self, id: WindowId) -> Option<bool> {
        self.windows
            .get(&id)
            .map(|child| child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only()))
    }

    /// Test-only: seed the headless renderer-focus marker for a child window.
    #[doc(hidden)]
    pub fn __test_set_child_renderer_focus_marker(&mut self, id: WindowId, focused: bool) -> bool {
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no child carries the
            // renderer-focus marker to update.
            return false;
        };
        child.test_renderer_focus_marker = Some(focused);
        true
    }

    /// Test-only: read the headless renderer-focus marker for a child window.
    #[doc(hidden)]
    pub fn __test_child_renderer_focus_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|child| child.test_renderer_focus_marker)
    }

    /// Test-only: invoke the child focus transition handler without constructing
    /// a winit `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_handle_child_focus_changed(&mut self, id: WindowId, focused: bool) {
        self.handle_child_focus_changed(id, focused);
    }

    /// classify [`Self::frontmost_window`] without
    /// borrowing anything mutably. Returns:
    ///   * `FrontmostKind::None` if no sonic window has been focused yet,
    ///     focus is currently outside every sonic window, or the recorded
    ///     id no longer matches any live window (stale-id race).
    ///   * `FrontmostKind::Main` if the recorded id matches the main
    ///     window we currently own.
    ///   * `FrontmostKind::Child(id)` if the recorded id matches a live
    ///     entry in [`Self::windows`].
    ///   * `FrontmostKind::Other` for any non-terminal window — actions
    ///     should fall through to the safe
    ///     main-window default in that case.
    ///
    /// Pure read; no mutation, no logging. The keymap_dispatch arms call
    /// this first, then route to the matching mutator + redraw target.
    /// Borrow the main window's [`WindowState`] from `self.windows`, keyed by
    /// [`Self::main_window_id`]. Returns `None` before `do_resumed` has run
    /// (no main window yet) or if the entry is missing for any reason.
    ///
    /// Every reader of the main window's renderer, tabs, and panes goes
    /// through this helper or its `_mut` counterpart.
    #[doc(hidden)]
    pub fn main(&self) -> Option<&WindowState> {
        let id = self.main_window_id?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::main`].
    #[doc(hidden)]
    pub fn main_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)
    }

    /// Borrow the main window's `Arc<Window>` from its [`WindowState`].
    /// Sole source of truth for the main window handle. Returns `None`
    /// before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_window(&self) -> Option<&Arc<Window>> {
        self.windows.get(&self.main_window_id?)?.window.as_ref()
    }

    /// borrow the main window's `GpuRenderer`
    /// from its `WindowState`. Sole source of truth for the main
    /// renderer.
    /// Returns `None` before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_renderer(&self) -> Option<&GpuRenderer> {
        self.windows.get(&self.main_window_id?)?.renderer.as_ref()
    }

    /// Mutable counterpart of [`Self::main_renderer`].
    #[doc(hidden)]
    pub fn main_renderer_mut(&mut self) -> Option<&mut GpuRenderer> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)?.renderer.as_mut()
    }

    /// borrow the main window's [`TabBar`] from
    /// its [`WindowState`]. Sole source of truth (legacy `App.tabs` was
    /// Returns `None` before `do_resumed` /
    /// `__test_synthetic_main` has populated the shadow entry.
    #[doc(hidden)]
    pub fn main_tabs(&self) -> Option<&TabBar> {
        Some(&self.windows.get(&self.main_window_id?)?.tabs)
    }

    /// Mutable counterpart of [`Self::main_tabs`].
    #[doc(hidden)]
    pub fn main_tabs_mut(&mut self) -> Option<&mut TabBar> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tabs)
    }

    /// borrow the main window's `Vec<TabState>`
    /// from its [`WindowState`]. Sole source of truth.
    #[doc(hidden)]
    pub fn main_tab_states(&self) -> Option<&[TabState]> {
        Some(self.windows.get(&self.main_window_id?)?.tab_states.as_slice())
    }

    /// Mutable counterpart of [`Self::main_tab_states`].
    #[doc(hidden)]
    pub fn main_tab_states_mut(&mut self) -> Option<&mut Vec<TabState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tab_states)
    }

    /// Insert a window and register its owner as one operation.
    ///
    /// The two steps are inseparable, so they are not offered separately. A
    /// window inserted without an owner is not merely uncharged, it is absent
    /// from hierarchy accounting entirely, and nothing later recovers it:
    /// [`Self::reconcile_pane_owners`] and [`Self::reattribute_pane_owners`]
    /// both skip a window whose owner is `None`, so its panes never get owners
    /// either and the periodic sampler passes over the whole subtree forever.
    ///
    /// Registering here rather than at each call site is the same reasoning
    /// [`Self::reconcile_pane_owners`] applies to panes: a rule every call site
    /// must remember is a rule one call site will forget, and the forgotten one
    /// is silent — the window works, and only the memory report is missing it.
    ///
    /// Panes already in `window` are adopted by this call. A window populated
    /// after insertion instead reconciles when those panes arrive.
    pub(super) fn insert_window_registered(&mut self, id: WindowId, window: WindowState) {
        self.windows.insert(id, window);
        self.register_window_owner(id);
    }

    /// Register a window in the governor hierarchy and record its owner.
    ///
    /// Private, and deliberately: reaching it goes through
    /// [`Self::insert_window_registered`], so registration cannot drift away
    /// from the insertion it belongs to.
    ///
    /// Idempotent by construction: a window that already has an owner keeps
    /// it, so a re-insert during tab transfer cannot create a second owner
    /// that never closes. That is the ratchet shape — an owner registered
    /// twice and released once leaves the hierarchy permanently over-counted.
    fn register_window_owner(&mut self, id: WindowId) {
        self.register_window_owner_inner(id);
        // A window arrives with its panes already populated, so registering
        // the window without them would leave every pane unowned until the
        // next sampling pass.
        //
        // Re-attribution rather than plain reconciliation: a window built by
        // tear-out receives panes that already carry an owner, parented below
        // the window they left. Reconciliation only adopts *ownerless* panes,
        // so it skips exactly those and leaves the source window counting a
        // pane it no longer holds — which refuses that window's close.
        self.reattribute_pane_owners();
    }

    fn register_window_owner_inner(&mut self, id: WindowId) {
        let root = self.governor.root_owner();
        // Cloned before the window borrow: `ResourceGovernor` is a handle over
        // an `Arc<Ledger>`, so this shares the ledger rather than copying it.
        let governor_handle = self.governor.clone();
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` tracks no entry for this id, so an owner created
            // here would have no window to retain or close it.
            return;
        };
        if window.owner.is_some() {
            // When: this `window` already holds an owner, so creating another
            // would leave a duplicate hierarchy node no one closes.
            return;
        }
        let owner =
            self.governor.create_child(root, OwnerKind::Window, tracking_only_owner_limits());
        match owner {
            Ok(owner) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.owner = Some(OwnerGuard::new(governor_handle, owner));
                }
            }
            Err(error) => {
                // A window that cannot register still works; it is invisible
                // to hierarchy accounting until the next insert. Failing the
                // window would trade a diagnostic gap for a lost window.
                tracing::warn!(
                    target: "memory",
                    ?error,
                    "window owner registration failed; hierarchy accounting will omit it"
                );
            }
        }
    }

    /// Test-only: snapshot the governor's process root.
    #[doc(hidden)]
    pub fn __test_governor_snapshot_root(&self) -> sonicterm_types::ResourceSnapshot {
        self.governor
            .snapshot(self.governor.root_owner())
            .expect("the process root always snapshots")
    }

    /// Test-only: move a pane from one window to another.
    ///
    /// Mirrors what tab tear-out does — remove from the source map, insert
    /// into the destination — so the test exercises the real ownership
    /// consequence rather than a simulation of it.
    #[doc(hidden)]
    pub fn __test_move_pane_between_windows(
        &mut self,
        source: WindowId,
        destination: WindowId,
        pane_id: u64,
    ) -> bool {
        let Some(pane) = self.windows.get_mut(&source).and_then(|w| w.panes.remove(&pane_id))
        else {
            // When: `source` yields no `pane_id`, so nothing was detached and both
            // windows keep the panes they had.
            return false;
        };
        let Some(window) = self.windows.get_mut(&destination) else {
            // When: `destination` no longer resolves, so the already-removed pane
            // has nowhere to land and drops with its PTY.
            return false;
        };
        window.panes.insert(pane_id, pane);
        self.reattribute_pane_owners();
        true
    }

    /// Test-only: snapshot any owner.
    #[doc(hidden)]
    pub fn __test_owner_snapshot(
        &self,
        owner: ResourceOwnerId,
    ) -> Option<sonicterm_types::ResourceSnapshot> {
        self.governor.snapshot(owner).ok()
    }

    /// Test-only: measure one pane's retention through the reporting seam.
    #[doc(hidden)]
    pub fn __test_pane_retention(
        &self,
        window: WindowId,
        pane_id: u64,
    ) -> Option<retention::PaneRetention> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        retention::measure_pane(pane)
    }

    /// Test-only: the governor amounts a pane currently holds, by class.
    #[doc(hidden)]
    pub fn __test_pane_charges(
        &self,
        window: WindowId,
        pane_id: u64,
    ) -> Option<HashMap<ResourceClass, sonicterm_types::ResourceAmount>> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        Some(pane.charges.iter().map(|(class, held)| (*class, held.committed_amount())).collect())
    }

    /// Test-only: a pane's total charged bytes across every class.
    #[doc(hidden)]
    pub fn __test_pane_charge_total(&self, window: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        Some(pane.charges.values().map(|held| held.committed_amount().bytes).sum())
    }

    /// Test-only: media captures currently in flight on a pane.
    ///
    /// Distinct from the retained-bytes figure: a cancelled capture and a
    /// completed one both report zero bytes, and the slow-transfer test turns
    /// on which of those happened.
    #[doc(hidden)]
    pub fn __test_pane_capture_count(&self, window: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        pane.parser.try_lock().map(|parser| parser.live_capture_count())
    }

    /// Test-only: the byte ceiling the governor holds a pane owner to.
    ///
    /// Read back from the ledger rather than from the constant, so a limit that
    /// is computed correctly and never installed fails the assertion that uses
    /// this.
    #[doc(hidden)]
    pub fn __test_pane_owner_limit(&self, window: WindowId) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.values().next()?;
        let owner = pane.owner.as_ref()?.id();
        self.governor.snapshot(owner).ok().map(|snapshot| snapshot.owner_bytes_limit)
    }

    /// Test-only: set a child pane's scrollback limit.
    #[doc(hidden)]
    pub fn __test_set_child_pane_scrollback(
        &mut self,
        window: WindowId,
        pane_id: u64,
        limit: usize,
    ) -> bool {
        let Some(pane) = self.windows.get_mut(&window).and_then(|w| w.panes.get_mut(&pane_id))
        else {
            // When: neither `window` nor its `panes` resolve the request, so no
            // grid exists whose scrollback `limit` could be set.
            return false;
        };
        pane.parser.lock().grid_mut().set_scrollback_limit(limit);
        true
    }

    /// Test-only: run a retention sample regardless of the interval.
    ///
    /// The production sampler is interval-gated and level-gated, neither of
    /// which a test should wait on or install a subscriber for. This drives
    /// the same charging pass the sampler runs.
    #[doc(hidden)]
    pub fn __test_force_retention_sample(&mut self) {
        self.reconcile_pane_owners();
        self.__test_charge_pane_owners();
    }

    /// Test-only: whether `owner` is still open in the governor.
    ///
    /// A closed owner's record is dropped, so this reports `false` for both a
    /// closed owner and an owner that never existed. That is the answer the
    /// callers want — "is this still holding resources" — and it stays correct
    /// whichever way the ledger represents a finished owner.
    #[doc(hidden)]
    pub fn __test_owner_is_open(&self, owner: ResourceOwnerId) -> bool {
        self.governor
            .snapshot(owner)
            .map(|snapshot| snapshot.owner_state != sonicterm_types::OwnerState::Closed)
            .unwrap_or(false)
    }

    /// Test-only: a window's owner id, if it registered one.
    #[doc(hidden)]
    pub fn __test_window_owner(&self, id: WindowId) -> Option<ResourceOwnerId> {
        self.windows.get(&id).and_then(|window| window.owner.as_ref()).map(OwnerGuard::id)
    }

    /// Test-only: one pane's owner id, if it has one.
    #[doc(hidden)]
    pub fn __test_pane_owner(&self, window: WindowId, pane_id: u64) -> Option<ResourceOwnerId> {
        self.windows
            .get(&window)?
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.owner.as_ref())
            .map(OwnerGuard::id)
    }

    /// Test-only: how many panes in a window have owners.
    #[doc(hidden)]
    pub fn __test_child_pane_owner_count(&self, id: WindowId) -> Option<usize> {
        self.windows
            .get(&id)
            .map(|window| window.panes.values().filter(|pane| pane.owner.is_some()).count())
    }

    /// Test-only: the pane owner ids in a window, sorted for comparison.
    #[doc(hidden)]
    pub fn __test_child_pane_owners(&self, id: WindowId) -> Vec<u64> {
        let mut owners: Vec<u64> = self
            .windows
            .get(&id)
            .map(|window| {
                window
                    .panes
                    .values()
                    .filter_map(|pane| pane.owner.as_ref())
                    .map(|owner| owner.id().get())
                    .collect()
            })
            .unwrap_or_default();
        owners.sort_unstable();
        owners
    }

    /// Test-only invoker for [`Self::reconcile_pane_owners`].
    #[doc(hidden)]
    pub fn __test_reconcile_pane_owners(&mut self) {
        self.reconcile_pane_owners();
    }

    /// Reconcile pane owners against every window's actual pane set.
    ///
    /// Panes are inserted at a dozen sites, several inside borrows where the
    /// governor is not reachable, and threading registration through all of
    /// them is the "every call site must remember" pattern that produces the
    /// one forgotten site. Reconciling instead means there is no site to
    /// forget: a pane without an owner gets one, and an owner whose pane is
    /// gone is closed.
    ///
    /// Runs from the periodic retention sampler rather than per frame, so its
    /// cost is bounded by that interval regardless of how often panes move.
    pub(super) fn reconcile_pane_owners(&mut self) {
        let window_ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for window_id in window_ids {
            let Some(window) = self.windows.get(&window_id) else {
                // When: `window_id` no longer resolves, so its pane set is gone
                // and there is nothing left to reconcile owners against.
                continue;
            };
            let Some(window_owner) = window.owner.as_ref().map(OwnerGuard::id) else {
                // When: this `window` holds no owner, so `create_child` has no
                // parent to hang pane owners from.
                continue;
            };

            let unowned: Vec<u64> = window
                .panes
                .iter()
                .filter(|(_, pane)| pane.owner.is_none())
                .map(|(id, _)| *id)
                .collect();

            for pane_id in unowned {
                match self.governor.create_child(
                    window_owner,
                    OwnerKind::AppPane,
                    pane_owner_limits(),
                ) {
                    Ok(owner) => {
                        // When: the governor granted `owner`, so it must reach a
                        // pane or be closed; an unheld owner leaks its record.
                        if let Some(pane) =
                            self.windows.get_mut(&window_id).and_then(|w| w.panes.get_mut(&pane_id))
                        {
                            pane.owner = Some(OwnerGuard::new(self.governor.clone(), owner));
                        } else {
                            // When: `panes` no longer resolves `pane_id`, so the
                            // owner would leak unless it is closed here.
                            let _ = self.governor.begin_close(owner);
                            let _ = self.governor.finish_close(owner);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "memory",
                            ?error,
                            "pane owner registration failed; hierarchy accounting omits this pane"
                        );
                    }
                }
            }
        }
    }

    /// Re-parent pane owners whose window has changed, and close their old ones.
    ///
    /// A `PaneState` carries its `owner` field when tab tear-out moves it
    /// between windows, but the owner itself was created *below the source
    /// window's* owner and the governor has no move operation. Left alone, the
    /// source window keeps reporting a pane it no longer has and the
    /// destination reports none for a pane it does — which makes "what does
    /// this window hold" wrong in both directions, and that question is the
    /// entire reason the hierarchy exists.
    ///
    /// Detected by comparing each pane owner's recorded parent against the
    /// window it now lives in, so this needs no hook at the move sites: a pane
    /// that never moved has a matching parent and costs one snapshot read.
    ///
    /// The old owner is closed rather than abandoned. Existing committed
    /// charges move as one class-preserving batch before the guard changes, so
    /// parser contention cannot leave the destination owner empty. The fresh
    /// owner has the same pane limits, so a rejection means an internal ledger
    /// invariant failed; the owned provisional guard closes before that failure
    /// stops the move, while every token remains on the old owner.
    pub(super) fn reattribute_pane_owners(&mut self) {
        let window_ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for window_id in window_ids {
            let Some(window) = self.windows.get(&window_id) else {
                // When: `window_id` no longer resolves, so no pane set remains
                // whose owners could be reattributed.
                continue;
            };
            let Some(window_owner) = window.owner.as_ref().map(OwnerGuard::id) else {
                // When: this `window` holds no owner, so there is no destination
                // parent to move its pane owners onto.
                continue;
            };

            let misattributed: Vec<u64> = window
                .panes
                .iter()
                .filter_map(|(pane_id, pane)| {
                    let owner = pane.owner.as_ref()?.id();
                    let parent = self.governor.snapshot(owner).ok()?.parent?;
                    (parent != window_owner).then_some(*pane_id)
                })
                .collect();

            for pane_id in misattributed {
                let new_owner = match self.governor.create_child(
                    window_owner,
                    OwnerKind::AppPane,
                    pane_owner_limits(),
                ) {
                    Ok(owner) => owner,
                    Err(error) => {
                        // When: `create_child` returns `Err(error)`, no provisional
                        // owner exists and source attribution remains unchanged.
                        tracing::warn!(
                            target: "memory",
                            ?error,
                            pane = pane_id,
                            "pane owner reattribution could not create its destination owner"
                        );
                        continue;
                    }
                };
                let provisional = OwnerGuard::new(self.governor.clone(), new_owner);
                let transferred = {
                    let Some(pane) =
                        self.windows.get_mut(&window_id).and_then(|w| w.panes.get_mut(&pane_id))
                    else {
                        // When: `pane_id` vanished after the owner was created, the
                        // empty provisional guard below must close it immediately.
                        drop(provisional);
                        continue;
                    };
                    install_transferred_pane_owner(pane, provisional)
                };
                match transferred {
                    Ok(stale) => drop(stale),
                    Err(error) => {
                        panic!(
                            "pane {pane_id} owner reattribution violated governor invariants: {error}"
                        );
                    }
                }
            }
        }
        // Ownerless panes may coexist with moved panes when a populated window
        // is first registered; adopt them after reattribution finishes.
        self.reconcile_pane_owners();
    }

    /// Close a window's owner and every pane owner below it.
    ///
    /// Called from window teardown. Owners are closed leaf-first because the
    /// governor refuses to finish closing a parent with open children — which
    /// is the invariant that makes a leaked pane owner visible rather than
    /// silent.
    /// Close the governor owners held by a window already removed from the map.
    ///
    /// Takes the `WindowState` rather than looking it up, because the
    /// production close paths remove the window *before* releasing its
    /// registries — so a lookup-based release returns early and closes
    /// nothing. That is exactly what happened: the release ran, found no
    /// window, and returned, leaving every owner `Open` for the life of the
    /// process.
    pub(super) fn release_owners_of(&mut self, window: &mut WindowState) {
        // Charges first. `finish_close` refuses an owner that still holds
        // them, and the previous order took `pane.owner` while leaving
        // `pane.charges` populated — so every close returned
        // `OwnerHasLiveCharges` and stopped at `Closing`.
        // Charges first, then drop the guards: each closes its owner on drop,
        // and `finish_close` refuses an owner still holding charges.
        for pane in window.panes.values_mut() {
            pane.charges.clear();
            drop(pane.owner.take());
        }
        drop(window.owner.take());
    }

    pub(super) fn release_window_owner(&mut self, id: WindowId) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` no longer resolves, so the map exposes no guards to
            // drain and the owner records are already unreachable.
            return;
        };
        // Charges must be released before the owner closes.
        //
        // `finish_close` refuses an owner that still holds charges, and this
        // took `pane.owner` while leaving `pane.charges` populated — so every
        // close returned `OwnerHasLiveCharges`, the `let _` discarded it, and
        // the owner stopped at `Closing` forever. Measured: 80 of 80 owners
        // still open after 40 create/destroy cycles.
        //
        // `reattribute_pane_owners` already does this in the right order,
        // twelve lines away.
        for pane in window.panes.values_mut() {
            pane.charges.clear();
            drop(pane.owner.take());
        }
        drop(window.owner.take());
    }

    /// The main window's pane map.
    ///
    /// `None` before a main window exists, which distinguishes "no window yet"
    /// from "a window holding no panes".
    pub fn main_panes(&self) -> Option<&HashMap<u64, PaneState>> {
        Some(&self.windows.get(&self.main_window_id?)?.panes)
    }

    /// Mutable counterpart of [`Self::main_panes`]. NOTE: this borrows
    /// the full main [`WindowState`] mutably via `windows.get_mut`, so
    /// callers needing panes + tabs/tab_states/renderer in one expression
    /// must instead `let ws = self.main_mut()?;` and field-disjoint
    /// split-borrow.
    #[doc(hidden)]
    pub fn main_panes_mut(&mut self) -> Option<&mut HashMap<u64, PaneState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.panes)
    }

    /// borrow the main window's selection
    /// `Option<Selection>` from its [`WindowState`]. Sole source of
    /// truth.
    /// Returns `None` (no main window) — `Some(None)` (no selection)
    /// — `Some(Some(_))` (active selection).
    #[doc(hidden)]
    pub fn main_selection(&self) -> Option<&Option<Selection>> {
        Some(&self.windows.get(&self.main_window_id?)?.selection)
    }

    /// Mutable counterpart of [`Self::main_selection`].
    #[doc(hidden)]
    pub fn main_selection_mut(&mut self) -> Option<&mut Option<Selection>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.selection)
    }

    /// borrow the main window's
    /// `ModifiersState` from its [`WindowState`]. Returns
    /// `ModifiersState::empty()` if the main window does not yet
    /// exist (safe default — no modifiers held).
    #[doc(hidden)]
    pub fn main_modifiers(&self) -> ModifiersState {
        self.main_window_id
            .and_then(|id| self.windows.get(&id))
            .map(|ws| ws.modifiers)
            .unwrap_or_else(ModifiersState::empty)
    }

    /// replace the main window's selection.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn selection_set(&mut self, sel: Option<Selection>) {
        if let Some(ws) = self.main_mut() {
            ws.selection = sel;
        }
    }

    /// replace the main window's copy-mode state.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn copy_mode_set(&mut self, st: Option<CopyModeState>) {
        if let Some(ws) = self.main_mut() {
            ws.copy_mode = st;
        }
    }

    /// borrow the [`WindowState`] of whichever terminal
    /// window is OS-frontmost. Falls back to the main window when no
    /// frontmost has been recorded yet (matches the safe default in
    /// [`Self::frontmost_kind`]).
    #[doc(hidden)]
    pub fn frontmost(&self) -> Option<&WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::frontmost`].
    #[doc(hidden)]
    pub fn frontmost_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get_mut(&id)
    }

    /// Which terminal window currently holds OS focus.
    ///
    /// Keymap dispatch routes window-scoped chords by this, so a stale or
    /// unfocused id resolves to `None` rather than defaulting to main.
    #[doc(hidden)]
    pub fn frontmost_kind(&self) -> FrontmostKind {
        let Some(id) = self.frontmost_window else {
            // When: `frontmost_window` recorded nothing, so focus is unknown and
            // callers fall back to main rather than guessing a target.
            return FrontmostKind::None;
        };
        if let Some(w) = self.main_window() {
            // When: `main_window` exists, so its identity is checked before the
            // recorded `id` is treated as a torn-out child.
            if w.id() == id {
                // When: `w` carries the focused `id`, so the chord lands on main
                // and the child lookup below is unnecessary.
                return FrontmostKind::Main;
            }
        }
        if self.windows.contains_key(&id) {
            // When: `windows` still tracks `id` after the main check, so focus
            // sits on a live torn-out child.
            return FrontmostKind::Child(id);
        }
        // Recorded id doesn't match anything live (rare: window closed
        // between the focus event and the action dispatch). Treat as
        // "no frontmost" so callers fall back to the main-window default.
        FrontmostKind::None
    }

    /// if [`Self::frontmost_window`] is `Some(_)`
    /// but classifies as `None` (recorded id no longer matches any
    /// live window), clear it. Called by keymap_dispatch arms BEFORE
    /// falling back to main, so the next dispatch doesn't retry the
    /// dead id. Returns `true` if a stale id was cleared (purely
    /// informational; callers ignore it today).
    #[doc(hidden)]
    pub fn clear_stale_frontmost(&mut self) -> bool {
        if self.frontmost_window.is_some() && self.frontmost_kind() == FrontmostKind::None {
            // When: `frontmost_window` names a window `frontmost_kind` can no
            // longer classify, so the record outlived the window it points at.
            self.frontmost_window = None;
            return true;
        }
        false
    }

    /// Test-only invoker for [`Self::close_active_tab_in_child`]. Exists
    /// because the helper is `pub(super)` and tests live outside the
    /// `app` module tree.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::reap_empty_child`]. Pins
    /// `App::transfer_tab` onto
    /// the unified empty-window cleanup contract: a stale id is a
    /// silent no-op (no panic, no spurious `windows` mutation), which
    /// is the only behaviour we can reliably pin without a live
    /// `WindowState` (needs a wgpu surface + winit `Window`).
    #[doc(hidden)]
    pub fn __test_invoke_reap_empty_child(&mut self, id: WindowId) {
        self.reap_empty_child(id);
    }

    /// Test-only invoker for [`Self::close_tab_at_in_child`] — the
    /// per-index helper the close-button (×) hit-test path uses in a
    /// torn-out child window's tab bar.
    #[doc(hidden)]
    pub fn __test_invoke_close_tab_at_in_child(&mut self, id: WindowId, idx: usize) -> bool {
        self.close_tab_at_in_child(id, idx)
    }

    /// Test-only invoker for [`Self::close_active_pane_or_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane_or_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_pane_or_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::next_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_next_tab_in_child(&mut self, id: WindowId) -> bool {
        self.next_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::prev_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_prev_tab_in_child(&mut self, id: WindowId) -> bool {
        self.prev_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::activate_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_activate_tab_in_child(&mut self, id: WindowId, idx: usize) -> bool {
        self.activate_tab_in_child(id, idx)
    }

    /// Test-only invoker for [`Self::split_active_pane_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_split_active_pane_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.split_active_pane_in_child(id, dir)
    }

    /// Test-only invoker for [`Self::close_active_pane_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_pane_in_child(id)
    }

    /// Test-only invoker for [`Self::close_active_pane`] (the main-window
    /// pane close path). Pairs with [`Self::test_viewport_override`] so
    /// tests can exercise the production close path — including the
    /// post-close `resize_visible_panes` call that re-fits the surviving
    /// sibling's Grid + PtyHandle — without a live wgpu renderer.
    /// See `crates/sonicterm-app/tests/per_pane_resize.rs`.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane(&mut self) {
        self.close_active_pane();
    }

    /// Test-only invoker for [`Self::focus_pane_dir_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_focus_pane_dir_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.focus_pane_dir_in_child(id, dir)
    }

    /// Test-only invoker for [`Self::toggle_active_pane_zoom_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_toggle_active_pane_zoom_in_child(&mut self, id: WindowId) -> bool {
        self.toggle_active_pane_zoom_in_child(id)
    }

    /// Test-only invoker for [`Self::resize_active_split_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_resize_active_split_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.resize_active_split_in_child(id, dir)
    }

    /// Test-only: count of tabs in the main App.
    #[doc(hidden)]
    pub fn __test_main_tab_count(&self) -> usize {
        self.main_tabs().map(|t| t.len()).unwrap_or(0)
    }

    /// Test-only: read the `pending_new_window` flag. Set by the
    /// `Action::NewWindow` dispatcher arm; consumed by
    /// `drain_pending_window_creates` (which needs a live
    /// `ActiveEventLoop` and so can't run in a unit test). The flag
    /// is the testable seam.
    #[doc(hidden)]
    pub fn __test_pending_new_window(&self) -> bool {
        self.pending_new_window
    }

    /// Test seam for deferred in-process tear-out requests.
    #[doc(hidden)]
    pub fn __test_pending_tear_out(&self) -> Option<(WindowId, usize, Option<(i32, i32)>)> {
        self.pending_tear_out
            .as_ref()
            .map(|t| (t.source_window, t.source_tab_idx, t.drop_screen_pos))
    }

    /// test seam: read the `pending_os_teardown` flag set
    /// by `handle_os_drag_ended` on the `DroppedOnEmpty` branch.
    #[doc(hidden)]
    pub fn __test_pending_os_teardown(&self) -> bool {
        self.pending_os_teardown
    }

    /// test seam: directly set `pending_os_teardown` so
    /// the race test can simulate the `DroppedOnEmpty` branch without
    /// forging a full OS-drag pending state.
    #[doc(hidden)]
    pub fn __test_set_pending_os_teardown(&mut self, v: bool) {
        self.pending_os_teardown = v;
    }

    /// test seam: drive `drain_pending_os_teardown` from
    /// integration tests (no `ActiveEventLoop` needed — the teardown
    /// drain doesn't create windows; only the window-create drain
    /// does).
    #[doc(hidden)]
    pub fn __test_drain_pending_os_teardown(&mut self) {
        self.drain_pending_os_teardown();
    }

    /// Test-only: count of entries in `self.windows`. Used by the
    /// `new_window_*` regression tests to assert that a real drain
    /// would change the windows-map cardinality (the post-drain
    /// state itself requires an `ActiveEventLoop`).
    ///
    /// the shadow main entry inserted by
    /// [`Self::do_resumed`] is excluded so existing call sites that
    /// expected this to be "number of torn-out child terminal windows"
    /// keep meaning "number of torn-out child terminal windows".
    #[doc(hidden)]
    pub fn __test_windows_len(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// Test-only: install a synthetic `drag_target` so the
    /// cross-window-merge gate can be exercised without driving a
    /// live winit cursor through `CursorMoved`.
    /// Pure decision used by the CursorMoved tear-out branch: would a
    /// call to `tear_out_tab` right now be a guaranteed no-op (because
    /// we have only one tab AND no cross-window drop target)? Hoisted
    /// out of `tear_out_tab` so the CursorMoved caller can decide
    /// *whether to invoke at all* and, crucially, leave gesture state
    /// (`pressed_tab`, `mouse_down`) intact when the answer is "yes".
    /// Without this gate, the production sequence (lone tab → cursor
    /// crosses tear-out threshold → cursor finally enters another
    /// window's bar) is impossible: the threshold trip would clear the
    /// gesture before the user ever reaches a sibling bar.
    #[doc(hidden)]
    pub fn __test_set_drag_target(
        &mut self,
        target: Option<crate::tab_drag::DropTarget<WindowId>>,
    ) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.drag_target = target;
        }
    }

    /// Test-only: remove a window from `self.windows`
    /// without going through the production teardown paths. Used by
    /// `os_drag_cleanup.rs` to simulate the "window vanished between
    /// snapshot collection and iteration" race that `cancel_drag_session`
    /// tolerates via its `windows.get_mut(...) else { continue }` branch
    /// . Returns `true` if
    /// the window existed and was removed, `false` otherwise.
    #[doc(hidden)]
    pub fn __test_remove_window(&mut self, id: WindowId) -> bool {
        self.release_window_owner(id);
        self.windows.remove(&id).is_some()
    }

    /// Test-only: drop a window without the explicit owner release first.
    ///
    /// [`Self::__test_remove_window`] calls `release_window_owner`, which takes
    /// each pane's owner in the right order before the window is removed. That
    /// hides what the struct does on its own, and the struct has to be right:
    /// a window removed from the map without that call, or held in the map
    /// until the process tears down, closes its owners purely by field drop
    /// order. This models that path.
    #[doc(hidden)]
    pub fn __test_drop_window_without_release(&mut self, id: WindowId) -> bool {
        self.windows.remove(&id).is_some()
    }

    /// Test-only: install a callback
    /// that fires INSIDE [`Self::cancel_drag_session`], AFTER the
    /// `self.windows.keys()` snapshot is collected but BEFORE the
    /// per-id iteration body runs. Lets tests exercise the exact
    /// `get_mut(&id).else { continue }` race-tolerance branch by
    /// removing (or inserting) a window in between.
    #[doc(hidden)]
    pub fn __test_set_post_snapshot_hook<F>(&mut self, f: F)
    where
        F: FnOnce(&mut App) + Send + 'static,
    {
        self.test_post_snapshot_hook = Some(Box::new(f));
    }

    /// How many torn-out child windows are live.
    ///
    /// The shadow main entry is excluded, so this counts only windows a user
    /// tore off rather than every entry in the map.
    #[doc(hidden)]
    pub fn child_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// `1` if the shadow main entry is present in
    /// [`Self::windows`], else `0`. Used by every "count torn-out
    /// child windows" path so they keep the
    /// same number.
    #[inline]
    #[doc(hidden)]
    pub fn shadow_main_count(&self) -> usize {
        match self.main_window_id {
            Some(id) if self.windows.contains_key(&id) => 1,
            _ => 0,
        }
    }

    /// number of windows in the unified
    /// [`Self::windows`] map.
    /// Used by the regression suite to pin the rename + role tagging.
    #[doc(hidden)]
    pub fn unified_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// count entries in [`Self::windows`] whose
    /// role matches the argument. Today every entry is `Terminal`;
    #[doc(hidden)]
    pub fn windows_with_role(&self, role: crate::app::WindowRole) -> usize {
        self.windows
            .iter()
            .filter(|(id, w)| w.role == role && Some(**id) != self.main_window_id)
            .count()
    }

    /// Test-only: seed a synthetic tab with one pane that has no PTY
    /// attached (just a Parser owning a fresh Grid). Lets integration
    /// Read-back of [`Self::main_window_id`] for tests.
    #[doc(hidden)]
    pub fn __test_main_window_id(&self) -> Option<WindowId> {
        self.main_window_id
    }

    // ShadowMainSnapshot helpers deleted — dpi + hovered_url
    // now live exclusively on WindowState.

    /// tests exercise tab/pane bookkeeping without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab(&mut self, title: &str) -> u64 {
        // ensure the synthetic main WindowState
        // entry exists before seeding. Future PRs B2b/c/d delete the
        // App.tabs/tab_states/panes fields outright, so seed writes
        // MUST land in `self.main_mut()` to survive that migration.
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, PaneState::new(parser, None));
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        pane_id
    }

    /// for tests that build an `App` without
    /// `do_resumed` running, insert a synthetic main `WindowState`
    /// entry (window=None, renderer=None) under a stable synthetic
    /// `WindowId` so test seeders can route writes through
    /// [`Self::main_mut`]. No-op if `main_window_id` is already set.
    /// In production [`Self::do_resumed`] detects the synthetic entry
    /// and removes it before inserting the real one.
    #[doc(hidden)]
    pub fn __test_synthetic_main(&mut self) {
        if self.main_window_id.is_some() {
            // When: `main_window_id` is already set, so seeding a second entry
            // would displace the identity live state is keyed by.
            return;
        }
        let id = synthetic_main_window_id();
        let ws = WindowState {
            // Registered when the window is inserted.
            owner: None,
            role: WindowRole::Terminal,
            window: None,
            renderer: None,
            tabs: TabBar::new(),
            tab_states: Vec::new(),
            panes: HashMap::new(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            pointer_gesture: None,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            path_probe: path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(id, ws);
        self.main_window_id = Some(id);
    }

    /// tests exercise tab/pane bookkeeping with a reply-capable parser but
    /// without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab_with_reply(
        &mut self,
        title: &str,
    ) -> (u64, crossbeam_channel::Receiver<Vec<u8>>) {
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(Grid::new(80, 24), tx)));
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, PaneState::new(parser, None));
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        (pane_id, rx)
    }

    /// Test-only: seed an existing synthetic pane parser with the app's
    /// current theme defaults. Mirrors the production spawn path without
    /// requiring a live PTY or reply-forwarder thread.
    #[doc(hidden)]
    pub fn __test_seed_pane_theme_colors(&mut self, pane_id: u64) -> bool {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so there is no parser whose
            // theme reply slots could be seeded.
            return false;
        };
        let mut parser = pane.parser.lock();
        seed_parser_theme_colors(&mut parser, &self.theme);
        true
    }

    /// Test-only: feed bytes into an existing pane parser. Used by integration
    /// tests that need to assert reply bytes from the real pane parser.
    #[doc(hidden)]
    pub fn __test_advance_pane_parser(&self, pane_id: u64, bytes: &[u8]) -> bool {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so the `bytes` have no parser
            // to advance and are dropped rather than misrouted.
            return false;
        };
        pane.parser.lock().advance(bytes);
        true
    }

    /// Test-only: read-only access to the internal panes map so tests
    /// can assert "this pane id is gone after detach".
    #[doc(hidden)]
    pub fn __test_pane_ids(&self) -> Vec<u64> {
        self.main().map(|ws| ws.panes.keys().copied().collect()).unwrap_or_default()
    }

    /// Test-only: read a pane's current `viewport_top_abs`. Used
    /// scrollback-scroll wiring tests to assert wheel + Scroll-keymap
    /// dispatch actually mutates the canonical field.
    #[doc(hidden)]
    pub fn __test_pane_viewport_top_abs(&self, pane_id: u64) -> Option<Option<u64>> {
        self.main()?.panes.get(&pane_id).map(|p| p.viewport_top_abs)
    }

    /// Test-only: synthesize scrollback by feeding `n` numbered lines and
    /// returns the resulting `scrollback_len()`. Each line is 4 chars +
    /// CRLF so callers can predict the row count.
    #[doc(hidden)]
    pub fn __test_grow_pane_scrollback(&self, pane_id: u64, n: u32) -> u64 {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so no scrollback was grown and
            // the reported row count is zero.
            return 0;
        };
        let mut buf = Vec::with_capacity((n as usize) * 8);
        for i in 0..n {
            use std::io::Write;
            let _ = write!(&mut buf, "{:04}\r\n", i % 10_000);
        }
        let mut parser = pane.parser.lock();
        parser.advance(&buf);
        parser.grid().scrollback_len() as u64
    }

    /// Test-only: viewport rows of a pane.
    #[doc(hidden)]
    pub fn __test_pane_viewport_rows(&self, pane_id: u64) -> Option<u16> {
        let pane = self.main()?.panes.get(&pane_id)?;
        Some(pane.parser.lock().grid().rows)
    }

    /// Test-only: current grid size for a pane.
    #[doc(hidden)]
    pub fn __test_pane_grid_size(&self, pane_id: u64) -> Option<(u16, u16)> {
        let pane = self.main()?.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cols, grid.rows))
    }

    /// Test-only: id of the active pane in a given tab. Returns `None`
    /// when `tab_idx` is out of range. Used by `split_focus.rs` to
    /// assert that splitting a pane plus the click-to-focus path
    /// actually flips the focused leaf.
    #[doc(hidden)]
    pub fn __test_active_pane_in_tab(&self, tab_idx: usize) -> Option<u64> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: set the active pane in `tab_idx` to `pane_id`. The
    /// click-to-focus logic in `window_event.rs` is the production
    /// caller; tests exercise the same state transition without
    /// driving a synthetic winit `MouseInput` event.
    #[doc(hidden)]
    pub fn __test_set_active_pane(&mut self, tab_idx: usize, pane_id: u64) -> bool {
        if let Some(st) = self.main_tab_states_mut().and_then(|ts| ts.get_mut(tab_idx)) {
            st.active_pane = pane_id;
            true
        } else {
            // When: `main_tab_states_mut` resolves no entry at `tab_idx`, so no
            // tab exists whose focus could be repointed.
            false
        }
    }

    /// Test-only: drive `split_active(Direction::Right)`. Mirrors the
    /// `Action::SplitRight` dispatch but skips the `Action` round-trip.
    #[doc(hidden)]
    pub fn __test_split_active_right(&mut self) {
        self.split_active(sonicterm_cfg::keymap::Direction::Right);
    }

    /// Test-only: tab count.
    #[doc(hidden)]
    pub fn __test_tab_count(&self) -> usize {
        self.main_tabs().map(|t| t.len()).unwrap_or(0)
    }

    /// Test-only: pending OS-drag payload count.
    #[doc(hidden)]
    pub fn __test_pending_os_drag_payload_count(&self) -> usize {
        self.pending_os_drag_payloads.len()
    }

    /// Test-only: drain queued OS-drag payloads after a synthetic main has
    /// been inserted. Mirrors the production `do_resumed` drain point without
    /// constructing a real winit window.
    #[doc(hidden)]
    pub fn __test_drain_pending_os_drag_payloads(&mut self) {
        self.drain_pending_os_drag_payloads();
    }

    /// Test-only: number of leaf panes in the given tab. Returns
    /// `None` when the tab index is out of range. Used by the
    /// `close_pane_or_tab_semantics` regression suite to assert that
    /// `Action::CloseActivePaneOrTab` shrinks the active tab's pane
    /// tree rather than the tab bar when the tab still has > 1 pane.
    #[doc(hidden)]
    pub fn __test_pane_count_in_tab(&self, tab_idx: usize) -> Option<usize> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.tree.leaves().len())
    }

    /// Test-only: install an `OsDragSink` so [`Self::try_os_drag_handoff`]
    /// can be exercised without going through the platform entry point.
    #[doc(hidden)]
    pub fn __test_set_os_drag_sink(&mut self, sink: Arc<dyn crate::os_drag::OsDragSink>) {
        self.os_drag_sink = Some(sink);
    }

    /// Install the platform OS handoff backend. `sonicterm-mac` supplies its
    /// pasteboard publisher; `sonicterm-windows` supplies OLE `DoDragDrop`.
    /// Tests use it via
    /// [`Self::__test_set_os_drag_backend`] to inject a mock.
    #[doc(hidden)]
    pub fn set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Test-only: install a mock [`os_drag::OsTabDragBackend`].
    #[doc(hidden)]
    pub fn __test_set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Test-only: hand out the shared pending-outcome mailbox
    /// so tests can drive [`Self::handle_os_drag_ended`] without
    /// constructing a real [`winit::event_loop::EventLoopProxy`].
    #[doc(hidden)]
    pub fn __test_os_drag_pending(&self) -> Arc<os_drag::PendingDragOutcome> {
        self.os_drag_pending.clone()
    }

    /// Test-only: seed the in-flight source bookkeeping that
    /// [`Self::begin_os_tab_drag`] normally sets. Used by tests that
    /// drive the dispatcher directly without first calling
    /// `begin_os_tab_drag`.
    #[doc(hidden)]
    pub fn __test_set_os_drag_source(&mut self, source: Option<(WindowId, usize)>) {
        self.os_drag_source = source;
    }

    /// build an [`os_drag::AppHandle`] tied to the App's
    /// event-loop proxy and the shared pending-outcome mailbox. The
    /// returned handle is what gets passed to
    /// [`os_drag::OsTabDragBackend::begin_session`] so the backend can
    /// post `DragMoved` / `DragEnded` events back to the main loop.
    ///
    /// Returns `None` when no event-loop proxy has been wired. In that
    /// case the OS drag is not startable, which the caller treats as
    /// "fall back to the existing within-process tear_out path".
    pub fn os_drag_app_handle(&self) -> Option<os_drag::AppHandle> {
        self.event_loop_proxy.clone().map(|p| {
            os_drag::AppHandle::with_pending_and_bars(
                p,
                self.os_drag_pending.clone(),
                self.os_drag_bars.clone(),
            )
        })
    }

    /// Hand out an `Arc` clone of the shared [`os_drag::TabBarRegistry`].
    /// Platform glue (e.g. `sonicterm-windows::os_drag_win`) calls this to
    /// stash a reference for use inside the OLE IDropTarget::Drop
    /// callback, where the AppHandle isn't always available.
    pub fn os_drag_bar_registry(&self) -> Arc<os_drag::TabBarRegistry> {
        self.os_drag_bars.clone()
    }

    /// Publish the current tab bar layout for `window` into the shared
    /// registry. Called from the App's per-frame render path with
    /// already-resolved screen coordinates (caller is responsible for
    /// converting logical-px / window-local to screen via
    /// winit's `Window::outer_position`).
    pub fn publish_os_drag_bar_snapshot(&self, snapshot: os_drag::TabBarSnapshot) {
        self.os_drag_bars.publish(snapshot);
    }

    /// Convenience: build a [`os_drag::TabBarSnapshot`] from the main
    /// window's current geometry + tab bar and publish it. No-op if the
    /// main window or renderer aren't yet initialized (pre-`resumed`).
    /// Called from the per-frame `RedrawRequested` handler so the
    /// snapshot registry tracks every visible tab-bar state change.
    pub(super) fn publish_main_window_tab_bar(&self) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(w) = self.main_window() else {
            // When: `main_window` does not exist yet, so there is no surface whose
            // tab-bar geometry could be published.
            return;
        };
        let Some(r) = self.main_renderer() else {
            // When: `main_renderer` is absent, so tab-bar height and insets cannot
            // be measured and any published rect would be invented.
            return;
        };
        let inner_origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = w.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let empty_tabs_pub = sonicterm_ui::tabs::TabBar::new();
        let layout = TabBarLayout::compute_with_height(
            self.main_tabs().unwrap_or(&empty_tabs_pub),
            raster_w,
            r.tab_bar_logical_height(),
        )
        .with_top_offset(r.tab_bar_y_offset())
        .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(w.id()), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    /// Remove a window's snapshot from the registry (called on window
    /// close). Safe to call with `None` (matches main-window convention).
    pub fn remove_os_drag_bar_snapshot(&self, window: Option<WindowId>) {
        self.os_drag_bars.remove(window);
    }

    /// Publish the tab bar snapshot for the child window keyed by `id`.
    /// No-op if the child isn't found. Called from the child's redraw
    /// path right after `Renderer::render`.
    pub fn publish_child_window_tab_bar(&self, id: WindowId) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(child) = self.windows.get(&id) else {
            // When: `id` tracks no window, so there is no child tab bar to
            // publish geometry for.
            return;
        };
        let Some(win) = child.window.as_ref() else {
            // When: this `child` has no live window, so screen geometry cannot be
            // measured and the snapshot would carry stale coordinates.
            return;
        };
        let inner_origin = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = win.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let Some(r) = child.renderer.as_ref() else {
            // When: this `child` has no renderer, so tab-bar height is unknown and
            // the layout below has no metrics to compute against.
            return;
        };
        let layout =
            TabBarLayout::compute_with_height(&child.tabs, raster_w, r.tab_bar_logical_height())
                .with_top_offset(r.tab_bar_y_offset())
                .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(id), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    /// begin an OS-level tab drag session via the installed
    /// backend. Returns `true` when the backend was invoked, `false`
    /// when no backend is installed or no event-loop proxy exists (in
    /// which case the caller falls back to the existing tear_out path).
    ///
    /// Records `(source_window, source_tab_idx)` so the
    /// `UserEvent::DragEnded` dispatcher knows where the gesture
    /// originated when routing the outcome.
    pub fn begin_os_tab_drag(
        &mut self,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    ) -> bool {
        let Some(handle) = self.os_drag_app_handle() else {
            // When: no `os_drag_app_handle` can be built, so the platform has no
            // drag context and recording a source would strand it.
            return false;
        };
        let Some(backend) = self.os_drag_backend.as_mut() else {
            // When: no `os_drag_backend` is installed, so nothing can carry the
            // session and the source must not be recorded.
            return false;
        };
        backend.begin_session(handle, source_window, source_tab_idx, payload_json, drag_image_png);
        self.os_drag_source = Some((source_window, source_tab_idx));
        true
    }

    /// does the installed backend own the gesture end-to-end?
    /// `try_os_drag_handoff` consults this to decide whether to skip
    /// the legacy `OsDragSink` after `begin_os_tab_drag` returns —
    /// running both on Windows would invoke `DoDragDrop` twice.
    pub fn os_drag_backend_handles_full_gesture(&self) -> bool {
        self.os_drag_backend.as_ref().map(|b| b.handles_full_gesture()).unwrap_or(false)
    }

    /// register a winit window with the installed OS-drag
    /// backend so OS-level drops landing on that window's HWND /
    /// NSWindow are routed back into the App. Called once per window
    /// at creation time — main window from `App::resumed`, torn-out
    /// child windows from `tear_out_tab` / `tear_out_from_child`.
    ///
    /// No-op if no backend is installed (mac, tests) — the trait's
    /// default `register_window` impl is itself a no-op, so a backend
    /// that does not need per-window registration (mac) can opt out
    /// cleanly while still implementing the unified entry point.
    ///
    /// Without this call, drops on torn-out child windows on Windows
    /// silently never reach `IDropTarget::Drop` (blocker).
    pub fn register_window_with_os_drag_backend(
        &mut self,
        window_id: WindowId,
        window: &std::sync::Arc<winit::window::Window>,
    ) {
        let Some(handle) = self.os_drag_app_handle() else {
            // When: no `os_drag_app_handle` can be built, so the window cannot be
            // registered as a drop target on this platform.
            return;
        };
        let Some(backend) = self.os_drag_backend.as_mut() else {
            // When: no `os_drag_backend` is installed, so per-window registration
            // has nothing to register against.
            return;
        };
        backend.register_window(handle, window_id, window);
    }

    pub(super) fn release_child_window_registries(&mut self, window_id: WindowId) {
        self.pending_redraw_windows.remove(&window_id);
        self.os_drag_bars.remove(Some(window_id));
        if let Some(backend) = self.os_drag_backend.as_mut() {
            backend.unregister_window(window_id);
        }
    }

    /// dispatcher entry point for `UserEvent::DragMoved`.
    /// Drains the mailbox; currently a no-op beyond logging — the
    /// drag-chip overlay is rendered from `tab_drag` state, not from
    /// the OS cursor stream. Reserved for future "highlight drop
    /// target in destination bar" feedback.
    pub fn handle_os_drag_moved(&mut self) -> Option<(i32, i32)> {
        let pos = self.os_drag_pending.take_moved();
        if let Some(p) = pos {
            tracing::trace!(?p, "os_drag_session: cursor moved");
        }
        pos
    }

    /// dispatcher entry point for `UserEvent::DragEnded`.
    /// Drains the mailbox outcome and routes it: `DroppedOnBar` →
    /// [`Self::transfer_tab`]; `Cancelled` → [`Self::cancel_drag_session`];
    /// `DroppedOnEmpty` is left for the existing tear_out path (this
    /// dispatcher just clears the in-flight bookkeeping). Returns the
    /// outcome that was processed for tests to assert on.
    pub fn handle_os_drag_ended(&mut self) -> Option<os_drag::DragOutcome> {
        let outcome = self.os_drag_pending.take_ended()?;
        let source = self.os_drag_source.take();
        match outcome {
            os_drag::DragOutcome::DroppedOnBar { target_window, target_slot } => {
                // When: the drop landed on a bar, so `target_window` and
                // `target_slot` name where the dragged tab should be inserted.
                let Some((src_win, src_idx)) = source else {
                    // When: no `source` was recorded, so there is no tab to move
                    // and the stale drag state is cancelled instead.
                    tracing::warn!(
                        "os_drag_session: DroppedOnBar arrived with no recorded source — cancelling"
                    );
                    self.cancel_drag_session();
                    return Some(outcome);
                };
                // `source` / `target` are `Option<WindowId>`, where
                // `None` means "the App's main window". The
                // backend always reports a concrete WindowId on the
                // source side, but the *target* may legitimately be the
                // main window. Detect that by comparing against the
                // App's `window` field.
                let src_opt = self
                    .main_window()
                    .map(|w| w.id())
                    .filter(|&id| id == src_win)
                    .map_or(Some(src_win), |_| None);
                let tgt_opt = match target_window {
                    Some(id) if self.main_window().map(|w| w.id() == id).unwrap_or(false) => None,
                    other => other,
                };
                if let Err(e) = self.transfer_tab(src_opt, src_idx, tgt_opt, target_slot) {
                    tracing::warn!(?e, "os_drag_session: transfer_tab refused — cancelling");
                    self.cancel_drag_session();
                }
            }
            os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos } => {
                tracing::debug!(
                    ?drop_screen_pos,
                    "os_drag_session: DroppedOnEmpty — in-process tear-out"
                );
                // replace the legacy
                // out-of-process tear-out (child-window via
                // `spawn_tearout_child` → `Command::new`) with an
                // in-process create. Enqueue a typed `PendingTearOut`
                // request carrying the recorded source tab handle and
                // the Win32 cursor screen position; the next
                // event-loop tick drains it via the existing
                // `drain_pending_window_creates` slot, which now
                // builds the child window directly from the reusable
                // helper extracted from `tear_out.rs`.
                if let Some((src_win, src_idx)) = source {
                    let source_tab_id = self.tab_id_at(src_win, src_idx);
                    self.pending_tear_out = Some(PendingTearOut {
                        source_window: src_win,
                        source_tab_idx: src_idx,
                        source_tab_id,
                        drop_screen_pos: Some(drop_screen_pos),
                    });
                } else {
                    // When: no source was recorded, so no tab can be torn out and
                    // the drop is logged rather than acted on.
                    tracing::warn!(
                        "os_drag_session: DroppedOnEmpty without recorded source — no tear-out"
                    );
                }
                // Do NOT call `cancel_drag_session` inline
                // here. The `DroppedOnEmpty` path triggers a
                // tear-out-spawn that creates a brand new top-level
                // window via the `pending_new_window` /
                // `pending_tear_out` drain. If we cancel inline,
                // cross-window drag-residue cleanup runs BEFORE the
                // new window exists, racing the spawn and potentially
                // freezing Explorer's drag thread on Windows when the
                // OLE drop-target tear-down sequence overlaps with new
                // HWND creation. Defer cancellation to
                // `drain_pending_os_teardown`, which runs AFTER
                // `drain_pending_window_creates` at the event-loop
                // boundary. Order matters; this flag controls only
                // WHEN cancel runs, not WHETHER — the all-windows
                // loop still runs unconditionally on drain (preserves
                // the `os_drag_cleanup.rs:172-201` idempotence
                // guarantee).
                self.pending_os_teardown = true;
            }
            os_drag::DragOutcome::Cancelled => {
                self.cancel_drag_session();
            }
        }
        Some(outcome)
    }

    /// Test-only: drive the OS-drag handoff path with a forced "cursor
    /// is outside any window" precondition (trivially true in tests
    /// since no winit window is created). Returns the same bool as the
    /// internal implementation: `true` = source-tab was detached,
    /// `false` = source tab preserved.
    #[doc(hidden)]
    pub fn __test_try_os_drag_handoff(&mut self, index: usize) -> bool {
        self.try_os_drag_handoff(index)
    }

    /// Test-only: inspect and mutate the drag-gesture state
    /// (`pressed_tab`, `mouse_down`) so an integration test can
    /// reproduce the production sequence "tab pressed → cursor
    /// crosses tear-out threshold → eventually drops on sibling
    /// window" without needing a live winit `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_pressed_tab(&self) -> Option<usize> {
        self.main().and_then(|ws| ws.pressed_tab)
    }

    /// Test seam: whether the main window is tracking a held mouse button.
    ///
    /// Reports `false` when no main window exists, so a caller sees the same
    /// "nothing held" answer either way.
    #[doc(hidden)]
    pub fn __test_mouse_down(&self) -> bool {
        self.main().map(|ws| ws.mouse_down).unwrap_or(false)
    }

    /// Test seam: set which tab the main window treats as pressed.
    ///
    /// Seeds a synthetic main window first, so a test can drive tab-press
    /// behavior without a live winit window.
    #[doc(hidden)]
    pub fn __test_set_pressed_tab(&mut self, v: Option<usize>) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.pressed_tab = v;
        }
    }

    /// Test seam: set whether the main window holds a mouse button.
    ///
    /// Seeds a synthetic main window first, so drag gestures can be driven
    /// without real pointer events.
    #[doc(hidden)]
    pub fn __test_set_mouse_down(&mut self, v: bool) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.mouse_down = v;
        }
    }

    /// Test-only: borrow the redraw target Arc for a given pane id,
    /// so a test can assert the per-pane redraw indirection survives
    /// state transfers.
    #[doc(hidden)]
    pub fn __test_pane_redraw_target(&self, id: u64) -> Option<Arc<Mutex<Option<WindowId>>>> {
        self.main()?.panes.get(&id).map(|p| p.redraw_target.clone())
    }

    /// Test-only: install or clear a pane's PTY handle so tear-out tests
    /// can verify ownership moves without spawning a real shell.
    #[doc(hidden)]
    pub fn __test_set_pane_pty(&mut self, id: u64, pty: Option<PtyHandle>) -> bool {
        let Some(pane) = self.main_mut().and_then(|ws| ws.panes.get_mut(&id)) else {
            // When: `id` resolves to no pane, so the supplied `pty` has no owner
            // and is dropped instead of installed.
            return false;
        };
        pane.pty = pty;
        true
    }

    /// Test-only: report whether a pane still has a PTY handle.
    #[doc(hidden)]
    pub fn __test_pane_pty_present(&self, id: u64) -> Option<bool> {
        self.main()?.panes.get(&id).map(|pane| pane.pty.is_some())
    }

    /// Read-only accessor used by tests and (eventually) the
    /// renderer to honor the View → Toggle Tab Bar menu item.
    #[doc(hidden)]
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }

    /// cancel an in-flight drag session. Wired
    /// to the ESC key handler in `window_event.rs` (any window's
    /// `WindowEvent::KeyboardInput` with `NamedKey::Escape` clears
    /// the App's drag_session AND every per-window drag_session) so
    /// the gesture is abandoned with the source tab left in place.
    /// Returns `true` if a drag session was actively cleared, `false`
    /// when no drag was in progress.
    #[doc(hidden)]
    pub fn cancel_drag_session(&mut self) -> bool {
        let mut had = false;
        // (defensive): snapshot window-id keys BEFORE the
        // mutation loop. The loop body calls `clear_drag_chip` /
        // `request_redraw`, neither of which mutate `self.windows`
        // today, but a future per-window handler (or a winit reentrant
        // callback on Windows under heavy load) could
        // insert/remove a window mid-iteration. Iterating a snapshot of
        // `Vec<WindowId>` is panic-free and matches intent: cancel
        // residue on the set of windows that exist RIGHT NOW. The
        // all-windows loop runs UNCONDITIONALLY — never short-circuit;
        // `os_drag_cleanup.rs:172-201` asserts this on a re-armed
        // second invocation.
        let ids: Vec<_> = self.windows.keys().copied().collect();
        // Invoke the test-only
        // post-snapshot hook AFTER `ids` is collected but BEFORE the
        // iteration body starts. The `take()` releases the hook so it
        // never re-fires, and (more importantly) leaves no live borrow
        // on `self` — the closure can freely mutate `self.windows`,
        // which is the exact race we need to exercise to prove the
        // `get_mut(&id).else { continue }` arm below fires. Always
        // `None` in production (the setter is `__test_*`-gated).
        if let Some(hook) = self.test_post_snapshot_hook.take() {
            hook(self);
        }
        // clear ALL per-window drag residue, not just
        // drag_session / drag_target. Previously `pressed_tab` and
        // `mouse_down` were only cleared on the main window, and the
        // renderer's `drag_chip` overlay was never cleared by this path
        // at all — so an OS-drag end (which bypasses the normal
        // MouseInput::Released handlers in window_event.rs / child_window.rs
        // that DO clear drag_chip) left a stale grey chip rectangle
        // floating in empty pane space until the next render forced a
        // refresh. Iterate every WindowState (main + children) and wipe
        // the lot.
        for id in ids {
            let Some(ws) = self.windows.get_mut(&id) else {
                // When: `windows` dropped `id` between the snapshot and this
                // iteration, so its residue is already gone with the window.
                continue;
            };
            if ws.drag_session.take().is_some() {
                had = true;
            }
            ws.drag_target = None;
            ws.pressed_tab = None;
            ws.mouse_down = false;
            window_event::cancel_pointer_gesture(&mut ws.pointer_gesture);
            // also abandon any scrollbar/splitter drag residue —
            // a global drag-cancel should leave no gesture half-held on any
            // window, mirroring the focus-loss cleanup.
            ws.scrollbar_drag = None;
            ws.splitter_drag = None;
            // Clear the renderer's persistent
            // drag-chip overlay AND the headless-test marker via a single
            // helper so production and test paths can never diverge. The
            // per-frame emitter keeps drawing
            // whatever Some(_) value sits in the renderer, so leaving it
            // behind ships a stale chip until something else triggers a
            // set_drag_chip(None). For headless test windows the renderer
            // is None — the `test_drag_chip_marker` mirror is what
            // the cleanup tests assert against.
            ws.clear_drag_chip();
            // Force a repaint so the cleared chip actually leaves the
            // screen instead of waiting for the next external event.
            if let Some(w) = ws.window.as_ref() {
                w.request_redraw();
            }
        }
        self.os_drag_handoff_started = false;
        had
    }

    /// pure cross-window transfer API. Operates
    /// on the App's MAIN window only (`source` / `target` are both
    /// `None` ⇒ main↔main reorder). Tests exercise the pure-container
    /// form in `crate::app::tab_transfer` directly; the App wrapper
    /// here delegates to the existing detach/attach pairs so the four
    /// real-window flavors (main↔main, main↔child, child↔main,
    /// child↔child) all funnel through one entry point.
    ///
    /// Returns `Ok(())` when the transfer happened, or a
    /// [`TransferError`] describing the validation failure. The
    /// pre-validation step is deliberate: a `bool` API that detaches
    /// first silently drops the detached tab —
    /// killing its child shell via `PtyHandle::Drop` — when the target
    /// window vanishes between gesture-start and drop. Source state is
    /// left untouched until *both* endpoints have been proven
    /// reachable.
    #[doc(hidden)]
    pub fn transfer_tab(
        &mut self,
        source: Option<WindowId>,
        source_idx: usize,
        target: Option<WindowId>,
        target_idx: usize,
    ) -> Result<(), TransferError> {
        // 0) pre-validate BOTH endpoints before mutating any window.
        //    Detaching and then
        //    failing to attach drops the `PaneState`, which kills the
        //    child shell via `PtyHandle::Drop`.
        match source {
            None => {
                // When: `source` is `None`, so the main window is the origin and
                // its bounds are proven before anything detaches.
                let main = self.main().ok_or(TransferError::SourceMissing)?;
                if source_idx >= main.tab_states.len() || source_idx >= main.tabs.len() {
                    // When: `source_idx` exceeds either `main` collection, so
                    // refusing now avoids detaching a tab that does not exist.
                    return Err(TransferError::SourceIndexOutOfBounds);
                }
            }
            Some(id) => {
                // When: `source` names a child, so that window is the origin and
                // its bounds are proven before anything detaches.
                let src = self.windows.get(&id).ok_or(TransferError::SourceMissing)?;
                if source_idx >= src.tab_states.len() || source_idx >= src.tabs.len() {
                    // When: `source_idx` exceeds either `src` collection, so
                    // refusing now avoids detaching a tab that does not exist.
                    return Err(TransferError::SourceIndexOutOfBounds);
                }
            }
        }
        if let Some(id) = target {
            // When: `target` names a child, so its existence is proven before the
            // source tab is detached and could be dropped.
            if !self.windows.contains_key(&id) {
                // When: `windows` lacks `id`, so refusing now leaves the source
                // tab attached rather than destroying it mid-move.
                return Err(TransferError::TargetMissing);
            }
        }

        // 1) detach from source — guaranteed to succeed after step 0.
        let detached = match source {
            None => self.detach_tab_state(source_idx),
            Some(id) => self.detach_from_child(id, source_idx),
        };
        let Some((tab, state, panes)) = detached else {
            // When: `detached` yielded no tab despite validation, so refusing is
            // the only move that cannot drop a live shell.

            // Shouldn't happen — step 0 validated. Defensive bail.
            return Err(TransferError::SourceIndexOutOfBounds);
        };

        // 2) attach to target — also guaranteed reachable after step 0.
        match target {
            None => self.attach_tab_state(target_idx, tab, state, panes),
            Some(id) => {
                // When: `target` names a child, so the detached tab is handed to
                // that window's attach path.
                if !self.attach_to_child(id, target_idx, tab, state, panes) {
                    // When: `attach_to_child` refused after preflight and already
                    // owns the moved values, so the tab cannot return to source.
                    return Err(TransferError::TargetMissing);
                }
            }
        }

        // 3) focus target window + bookkeeping
        match target {
            None => {
                if let Some(w) = self.main_window().cloned() {
                    self.frontmost_window = Some(w.id());
                    w.request_redraw();
                }
            }
            Some(id) => {
                self.frontmost_window = Some(id);
                if let Some(ws) = self.windows.get(&id) {
                    if let Some(w) = ws.window.as_ref() {
                        w.focus_window();
                        w.request_redraw();
                    }
                }
            }
        }

        // 4) source-empty → close source window
        let source_empty = match source {
            None => self.main_tabs().map(|t| t.is_empty()).unwrap_or(true),
            Some(id) => self.windows.get(&id).map(|w| w.tabs.is_empty()).unwrap_or(true),
        };
        if source_empty {
            if let Some(id) = source {
                // child window — route through the unified empty-window
                // cleanup contract so straggler redraw targets get nulled
                // and the "child reaped" trace fires; a raw
                // `windows.remove` skips both bits of bookkeeping.
                self.reap_empty_child(id);
            } else {
                // When: the emptied `source` is the main window, so its own
                // last-tab-closed handling hides it rather than reaping it.
            }
        }
        Ok(())
    }
}

/// Why a transfer rejected the gesture without losing the tab. Returned
/// by [`App::transfer_tab`]. A missing-target attach would otherwise
/// silently drop the detached `PaneState`, killing its child shell via
/// `PtyHandle::Drop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum TransferError {
    /// `source` was `Some(id)` but the id is not in `App::windows`.
    SourceMissing,
    /// `target` was `Some(id)` but the id is not in `App::windows`.
    TargetMissing,
    /// `source_idx` is beyond the source window's tab vector.
    SourceIndexOutOfBounds,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.do_resumed(el);
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        self.do_user_event(el, event);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        self.do_window_event(el, win_id, event);
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.do_new_events(_el, cause);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.do_about_to_wait(el);
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Forward to sonicterm-logging so every Cmd+Q / WM_CLOSE /
        // last-window exit lands in sonicterm.log. See
        // `crates/sonicterm-logging/src/exit_trace.rs`.
        sonicterm_logging::record_loop_exiting();
    }
}

#[cfg(test)]
#[path = "effect_cleanup_tests.rs"]
mod effect_cleanup_tests;

#[cfg(test)]
#[path = "click_count_tests.rs"]
mod click_count_tests;

#[cfg(test)]
#[path = "focus_feedback_tests.rs"]
mod focus_feedback_tests;

#[cfg(test)]
#[path = "native_window_title_tests.rs"]
mod native_window_title_tests;

#[cfg(test)]
#[path = "redraw_coalescing_tests.rs"]
mod redraw_coalescing_tests;

#[cfg(test)]
#[path = "warm_window_pool_tests.rs"]
mod warm_window_pool_tests;

#[cfg(test)]
#[path = "command_event_tests.rs"]
mod command_event_tests;

#[cfg(test)]
#[path = "tear_out_timing_tests.rs"]
mod tear_out_timing_tests;

#[cfg(test)]
#[path = "software_render_tests.rs"]
mod software_render_tests;

#[cfg(test)]
#[path = "selection_invalidation_tests.rs"]
mod selection_invalidation_tests;

#[cfg(test)]
#[path = "pty_input_tests.rs"]
mod pty_input_tests;

#[cfg(test)]
#[path = "privilege_tests.rs"]
mod privilege_tests;
