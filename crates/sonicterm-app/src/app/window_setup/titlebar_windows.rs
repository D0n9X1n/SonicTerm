//! The Windows side of the snap layouts flyout: a comctl32 subclass on each terminal window with
//! caption buttons. It answers `HTMAXBUTTON` over SonicTerm's maximise button, so Windows 11 shows
//! the flyout there, and hands the non-client mouse messages that follow to winit as client ones,
//! by the decisions in `snap_route`. Windows 10 gets the same routing and shows no flyout.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_ui::window_band::WindowBand;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TRACKMOUSEEVENT_FLAGS,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, IsWindow, WM_LBUTTONDOWN, WM_NCDESTROY,
};

use super::snap_route::{
    self, lparam_of, point_of, SnapAction, SnapTarget, SnapTracking, Spot, HTCLIENT, HTMAXBUTTON,
    WM_CAPTURECHANGED, WM_MOUSELEAVE, WM_MOUSEMOVE, WM_NCMOUSELEAVE,
};

/// The id SonicTerm's snap subclass is installed under.
const SUBCLASS_ID: usize = 0x534E_4150;

/// `MK_LBUTTON`: the key state of a client left press.
const MK_LBUTTON: usize = 0x0001;

/// `HOVER_DEFAULT`: the system hover time, unused by leave tracking but required in the request.
const HOVER_DEFAULT: u32 = 0xFFFF_FFFF;

/// One hooked window: its current target, its tracking, and the generation its subclass carries.
#[derive(Debug, Clone, Copy)]
struct Hook {
    target: Option<SnapTarget>,
    tracking: SnapTracking,
    generation: usize,
}

thread_local! {
    /// The event-loop thread's hooked windows by HWND value. Every access copies out and drops the
    /// borrow before any window procedure runs, since those can re-enter the subclass.
    static HOOKS: RefCell<HashMap<isize, Hook>> = RefCell::new(HashMap::new());
    /// The next subclass generation: a subclass whose entry was removed or replaced acts as none.
    static NEXT_GENERATION: Cell<usize> = const { Cell::new(1) };
}

/// Keep `window`'s snap hook in step with its band: `band_height` in raster px at `scale`.
pub(super) fn sync(window: &winit::window::Window, band: WindowBand, band_height: f32, scale: f32) {
    let Ok(handle) = window.window_handle() else {
        // When: window_handle fails, the window is gone and needs no hook.
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        // When: the handle is not Win32, there is no HWND to subclass.
        return;
    };
    sync_hwnd(HWND(handle.hwnd.get() as *mut _), SnapTarget::of(band, band_height, scale));
}

/// Hook `hwnd` the first pass it has a target; later passes only update its target, so full
/// screen (no target) passes every message through until the buttons return.
pub(super) fn sync_hwnd(hwnd: HWND, target: Option<SnapTarget>) {
    let key = hwnd.0 as isize;
    let known = HOOKS
        .try_with(|hooks| {
            let Ok(mut hooks) = hooks.try_borrow_mut() else {
                // When: try_borrow_mut fails, the map is borrowed: skip this pass, never install twice.
                return true;
            };
            hooks.get_mut(&key).map(|hook| hook.target = target).is_some()
        })
        .unwrap_or(true);
    if known || target.is_none() {
        // When: known, the target is updated; when target is none, nothing needs the hook yet.
        return;
    }
    let generation = NEXT_GENERATION.with(|next| next.replace(next.get().wrapping_add(1)));
    let installed =
        // SAFETY: hwnd is a live window of this thread; snap_proc stays valid for the process and
        // removes itself on WM_NCDESTROY.
        unsafe { SetWindowSubclass(hwnd, Some(snap_proc), SUBCLASS_ID, generation) };
    if installed.as_bool() {
        // When: installed is true, SetWindowSubclass took: record the hook under its generation.
        let hook = Hook { target, tracking: SnapTracking::default(), generation };
        let _ = HOOKS.try_with(|hooks| {
            if let Ok(mut hooks) = hooks.try_borrow_mut() {
                hooks.insert(key, hook);
            }
        });
    }
}

/// Whether `hwnd` has a snap hook recorded.
#[cfg(test)]
pub(super) fn hooked(hwnd: HWND) -> bool {
    HOOKS.with(|hooks| hooks.borrow().contains_key(&(hwnd.0 as isize)))
}

/// `hwnd`'s hook if it still carries `generation`.
fn hook_of(hwnd: HWND, generation: usize) -> Option<Hook> {
    HOOKS
        .try_with(|hooks| {
            let hooks = hooks.try_borrow().ok()?;
            hooks.get(&(hwnd.0 as isize)).copied().filter(|hook| hook.generation == generation)
        })
        .ok()
        .flatten()
}

/// Store `tracking` for `hwnd`'s hook of `generation`; a replaced hook is left alone.
fn store_tracking(hwnd: HWND, generation: usize, tracking: SnapTracking) {
    let _ = HOOKS.try_with(|hooks| {
        if let Ok(mut hooks) = hooks.try_borrow_mut() {
            if let Some(hook) = hooks.get_mut(&(hwnd.0 as isize)) {
                if hook.generation == generation {
                    hook.tracking = tracking;
                }
            }
        }
    });
}

/// Forget `hwnd`'s hook of `generation`.
fn forget(hwnd: HWND, generation: usize) {
    let _ = HOOKS.try_with(|hooks| {
        if let Ok(mut hooks) = hooks.try_borrow_mut() {
            let key = hwnd.0 as isize;
            if hooks.get(&key).is_some_and(|hook| hook.generation == generation) {
                hooks.remove(&key);
            }
        }
    });
}

/// Whether `hwnd` still exists with its hook of `generation`: a window procedure the subclass
/// called may have closed the window, and nothing may be sent to it afterwards.
fn alive(hwnd: HWND, generation: usize) -> bool {
    let exists =
        // SAFETY: IsWindow only reads whether the handle names a window.
        unsafe { IsWindow(Some(hwnd)) }.as_bool();
    exists && hook_of(hwnd, generation).is_some()
}

/// Where client point `(x_px, y_px)` of `hwnd` is, against `target`'s maximise button.
fn spot_at(hwnd: HWND, target: Option<SnapTarget>, (x_px, y_px): (i32, i32)) -> Spot {
    let mut rect = RECT::default();
    if
    // SAFETY: hwnd is the subclassed window and rect a local the call writes.
    unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        // When: GetClientRect fails, treat the cursor as outside.
        return Spot::Outside;
    }
    if x_px < rect.left || x_px >= rect.right || y_px < rect.top || y_px >= rect.bottom {
        // When: (x, y) is outside the client rect, the cursor is outside.
        return Spot::Outside;
    }
    let width = (rect.right - rect.left) as f32;
    let on_button =
        target.is_some_and(|target| target.on_maximize(width, x_px as f32, y_px as f32));
    if on_button {
        Spot::Maximize
    } else {
        // When: on_button is false, the cursor is elsewhere in the client area.
        Spot::Client
    }
}

/// Screen point `(x_px, y_px)` in `hwnd`'s client coordinates.
fn to_client(hwnd: HWND, (x_px, y_px): (i32, i32)) -> (i32, i32) {
    let mut point = POINT { x: x_px, y: y_px };
    let _ =
        // SAFETY: hwnd is the subclassed window and point a local the call rewrites in place.
        unsafe { ScreenToClient(hwnd, &mut point) };
    (point.x, point.y)
}

/// Where the cursor is now, for `hwnd` and `target`.
fn cursor_spot(hwnd: HWND, target: Option<SnapTarget>) -> Spot {
    let mut point = POINT::default();
    if
    // SAFETY: point is a local the call writes.
    unsafe { GetCursorPos(&mut point) }.is_err() {
        // When: GetCursorPos fails (a secure desktop), treat the cursor as outside.
        return Spot::Outside;
    }
    spot_at(hwnd, target, to_client(hwnd, (point.x, point.y)))
}

/// Ask Windows for a leave message for `hwnd`: client leave with `TME_LEAVE`, non-client with
/// `TME_NONCLIENT` too. A failed request leaves the tracking as it was.
fn track(hwnd: HWND, flags: TRACKMOUSEEVENT_FLAGS) {
    let mut request = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: flags,
        hwndTrack: hwnd,
        dwHoverTime: HOVER_DEFAULT,
    };
    let _ =
        // SAFETY: request is a fully initialised local naming this thread's window.
        unsafe { TrackMouseEvent(&mut request) };
}

/// Send `message` to the procedures below this subclass (winit's), not through it.
fn send_below(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> LRESULT {
    // SAFETY: called only from snap_proc for its own hwnd, which is what DefSubclassProc requires.
    unsafe { DefSubclassProc(hwnd, message, WPARAM(wparam), LPARAM(lparam)) }
}

/// The snap subclass procedure. It copies the hook out, decides with `snap_route::route`, stores
/// the new tracking before acting, and after every call into winit checks the window still lives.
// SAFETY: Windows calls this only as the subclass procedure of hwnd on its own thread, with that
// message's wparam and lparam; it reads no pointer from them.
unsafe extern "system" fn snap_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    generation: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        // When: message is WM_NCDESTROY, the hook goes before the HWND can be reused.
        forget(hwnd, generation);
        let _ =
            // SAFETY: hwnd is this subclass's window; removing our own subclass here is allowed.
            unsafe { RemoveWindowSubclass(hwnd, Some(snap_proc), SUBCLASS_ID) };
        return send_below(hwnd, message, wparam.0, lparam.0);
    }
    let Some(hook) = hook_of(hwnd, generation) else {
        // When: no hook of this generation, the subclass is stale and passes everything on.
        return send_below(hwnd, message, wparam.0, lparam.0);
    };
    let nc_maximize = hook.target.is_some() && wparam.0 == HTMAXBUTTON as usize;
    let lost = hook.tracking.lost_leave;
    let needs_spot = matches!(message, WM_MOUSELEAVE | WM_NCMOUSELEAVE)
        || (lost && matches!(message, WM_MOUSEMOVE | WM_CAPTURECHANGED));
    let spot = if needs_spot {
        cursor_spot(hwnd, hook.target)
    } else {
        // When: needs_spot is false, the route ignores the spot, so no cursor read is spent.
        Spot::Outside
    };
    let captured =
        // SAFETY: GetCapture only reads which window of this thread holds the capture.
        unsafe { GetCapture() } == hwnd;
    let (action, tracking) = snap_route::route(message, nc_maximize, spot, captured, hook.tracking);
    if tracking != hook.tracking {
        // The tracking changed: store it before acting, since acting can re-enter.
        store_tracking(hwnd, generation, tracking);
    }
    act(hwnd, (message, wparam.0, lparam.0), generation, action, hook.target)
}

/// Carry out `action` for `message` (with its wparam and lparam) on `hwnd`.
fn act(
    hwnd: HWND,
    (message, wparam, lparam): (u32, usize, isize),
    generation: usize,
    action: SnapAction,
    target: Option<SnapTarget>,
) -> LRESULT {
    match action {
        SnapAction::Pass => send_below(hwnd, message, wparam, lparam),
        SnapAction::HitTest => {
            let hit = send_below(hwnd, message, wparam, lparam);
            let client = to_client(hwnd, point_of(lparam));
            // The default said client area and the point is the maximise button: name it.
            let on_button = hit.0 == HTCLIENT && spot_at(hwnd, target, client) == Spot::Maximize;
            if on_button {
                LRESULT(HTMAXBUTTON)
            } else {
                // When: on_button is false, the default answer stands.
                hit
            }
        }
        SnapAction::Hover => {
            // When: Hover, winit gets the client move first, then the system its own message.
            let (x_px, y_px) = to_client(hwnd, point_of(lparam));
            send_below(hwnd, WM_MOUSEMOVE, 0, lparam_of(x_px, y_px));
            if !alive(hwnd, generation) {
                // When: alive(hwnd, generation) is false, winit closed the window; nothing more.
                return LRESULT(0);
            }
            track(hwnd, TME_LEAVE | TME_NONCLIENT);
            send_below(hwnd, message, wparam, lparam)
        }
        SnapAction::Press => {
            let (x_px, y_px) = to_client(hwnd, point_of(lparam));
            let client = lparam_of(x_px, y_px);
            send_below(hwnd, WM_MOUSEMOVE, 0, client);
            if alive(hwnd, generation) {
                // The window survived the move: winit gets the press and captures the mouse.
                send_below(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, client);
            }
            LRESULT(0)
        }
        SnapAction::Swallow => LRESULT(0),
        SnapAction::SwallowLeave => {
            track(hwnd, TME_LEAVE | TME_NONCLIENT);
            LRESULT(0)
        }
        SnapAction::RearmClient => {
            track(hwnd, TME_LEAVE);
            send_below(hwnd, message, wparam, lparam)
        }
        SnapAction::ForwardLeave => {
            // When: ForwardLeave, winit gets the client leave it missed, then the original.
            send_below(hwnd, WM_MOUSELEAVE, 0, 0);
            if !alive(hwnd, generation) {
                // When: alive(hwnd, generation) is false, winit closed the window on the leave.
                return LRESULT(0);
            }
            send_below(hwnd, message, wparam, lparam)
        }
        SnapAction::ArmNonClient => {
            track(hwnd, TME_LEAVE | TME_NONCLIENT);
            send_below(hwnd, message, wparam, lparam)
        }
    }
}

#[cfg(test)]
#[path = "titlebar_windows_tests.rs"]
mod titlebar_windows_tests;
