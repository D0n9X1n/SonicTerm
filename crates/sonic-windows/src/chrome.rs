//! Custom non-client area handling for the integrated Win11-style titlebar.
//!
//! We subclass the HWND so we can:
//! - zero out the OS-drawn caption / borders via `WM_NCCALCSIZE`
//! - reapply DWM frame extension after composition changes
//! - serve `WM_NCHITTEST` ourselves, returning `HTCAPTION` for the drag
//!   strip and `HTMINBUTTON` / `HTMAXBUTTON` / `HTCLOSE` for the three
//!   caption buttons painted by `sonic-shared::quad::paint_caption_buttons`.
//!
//! Caption-button rects come from
//! [`sonic_shared::tabbar_view::caption_button_rects`] so the hit-test
//! geometry stays in sync with what's drawn.

#![cfg(target_os = "windows")]

use sonic_app::{app::integrated_titlebar_inset_px, menubar_bridge};
use sonic_core::keymap::Action;
use sonic_shared::tabbar_view::{
    caption_button_rects, caption_strip_reserved_width, BAR_LEFT_PAD, NEW_TAB_BUTTON_HEIGHT,
    NEW_TAB_BUTTON_WIDTH, TAB_BAR_HEIGHT,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, PostMessageW, GWL_STYLE, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON,
    HTMINBUTTON, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, WM_DWMCOMPOSITIONCHANGED,
    WM_LBUTTONDOWN, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_SYSCOMMAND,
    WS_MAXIMIZE,
};

const SUBCLASS_ID: usize = 0x5071_C001;

fn new_tab_button_rect(width: u32) -> sonic_shared::tabbar_view::Rect {
    let window_width = width as f32;
    let nt_w = NEW_TAB_BUTTON_WIDTH.min(window_width.max(0.0));
    let nt_h = NEW_TAB_BUTTON_HEIGHT.min(TAB_BAR_HEIGHT);
    let nt_x = (window_width - nt_w - BAR_LEFT_PAD - caption_strip_reserved_width()).max(0.0);
    let nt_y = ((TAB_BAR_HEIGHT - nt_h) * 0.5).max(0.0);
    sonic_shared::tabbar_view::Rect { x: nt_x, y: nt_y, w: nt_w, h: nt_h }
}

fn rect_contains(r: &sonic_shared::tabbar_view::Rect, x: f32, y: f32) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// Install the titlebar subclass on the given top-level HWND. Idempotent
/// per HWND (re-installing replaces the existing subclass proc).
pub fn install_subclass(hwnd: HWND) {
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0);
        extend_frame(hwnd);
    }
}

unsafe fn extend_frame(hwnd: HWND) {
    let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wparam.0 != 0 => {
            // Returning 0 with wparam != 0 means "the entire window rect is
            // client area" — i.e. no OS-drawn caption / borders.
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ =
                unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) };
            let local_x = (x - rect.left) as f32;
            let local_y = (y - rect.top) as f32;
            let width = (rect.right - rect.left).max(0) as u32;
            let strip_h = integrated_titlebar_inset_px() as f32;
            if local_y < 0.0 || local_y >= strip_h {
                return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            }
            // Caption-button rects (physical px). DPI = 1.0 here because
            // GetWindowRect already returns physical pixels. If we ever
            // gain a way to query the window's effective DPI cheaply we
            // can pass that instead.
            let [min, max, close] = caption_button_rects(width, 1.0);
            if rect_contains(&close, local_x, local_y) {
                LRESULT(HTCLOSE as isize)
            } else if rect_contains(&max, local_x, local_y) {
                LRESULT(HTMAXBUTTON as isize)
            } else if rect_contains(&min, local_x, local_y) {
                LRESULT(HTMINBUTTON as isize)
            } else if rect_contains(&new_tab_button_rect(width), local_x, local_y) {
                // Let the integrated `+` stay a normal client-area control;
                // returning HTCAPTION here makes Windows consume the click as
                // a non-client drag before winit can dispatch NewTab.
                LRESULT(HTCLIENT as isize)
            } else {
                // Drag strip; anything not over an interactive control is the caption.
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_LBUTTONDOWN => {
            let local_x = (lparam.0 & 0xFFFF) as i16 as f32;
            let local_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ =
                unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) };
            let width = (rect.right - rect.left).max(0) as u32;
            if rect_contains(&new_tab_button_rect(width), local_x, local_y) {
                tracing::trace!(
                    coords = ?(local_x, local_y),
                    "new_tab_button hit at {:?}, dispatching",
                    (local_x, local_y)
                );
                let _ = menubar_bridge::push_action(Action::NewTab);
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_DWMCOMPOSITIONCHANGED => {
            unsafe { extend_frame(hwnd) };
            LRESULT(0)
        }
        // Intercept NC button DOWN on caption buttons so the OS doesn't enter
        // its default caption-button capture loop (which on a custom-frame
        // window with NCCALCSIZE-zeroed non-client area silently swallows the
        // matching UP and never fires SC_MINIMIZE/SC_MAXIMIZE/SC_CLOSE).
        // Returning 0 marks the message handled; the real action is fired on
        // the UP message below, matching Win11 caption-button semantics
        // (action commits on release, not press).
        WM_NCLBUTTONDOWN if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            LRESULT(0)
        }
        WM_NCLBUTTONUP if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
            let cmd: usize = match wparam.0 as u32 {
                HTMINBUTTON => SC_MINIMIZE as usize,
                HTMAXBUTTON => {
                    // Toggle restore vs. maximize based on current style.
                    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
                    if style & WS_MAXIMIZE.0 != 0 {
                        SC_RESTORE as usize
                    } else {
                        SC_MAXIMIZE as usize
                    }
                }
                HTCLOSE => SC_CLOSE as usize,
                _ => return LRESULT(0),
            };
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd), LPARAM(0));
            }
            LRESULT(0)
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

// Suppress dead-code warning for HTCLIENT — kept imported so future
// edits to the hit-test that want to fall through to the client area
// don't have to rediscover the symbol.
const _: u32 = HTCLIENT;
