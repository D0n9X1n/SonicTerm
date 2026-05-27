//! Custom non-client area handling for the integrated Win11-style titlebar.
//!
//! We subclass the HWND so we can:
//! - zero out the OS-drawn caption / borders via `WM_NCCALCSIZE`
//! - reapply DWM frame extension after composition changes
//! - serve `WM_NCHITTEST` ourselves, returning `HTCAPTION` for the drag
//!   strip and `HTMINBUTTON` / `HTMAXBUTTON` / `HTCLOSE` for the three
//!   caption buttons painted by `sonic-gpu::quad::paint_caption_buttons`.
//! - dispatch `WM_NCLBUTTONUP` over a caption button to the matching
//!   window action (minimize / maximize-or-restore / close). Without
//!   this, the buttons were hit-tested but no click ever did anything.
//!
//! Caption-button rects come from
//! [`sonic_ui::tabbar_view::caption_button_rects`] so the hit-test
//! geometry stays in sync with what's drawn.

#![cfg(target_os = "windows")]

use sonic_app::app::integrated_titlebar_inset_px;
use sonic_shared::tabbar_view::{caption_button_rects, CaptionAction, CaptionButton};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    IsZoomed, PostMessageW, ShowWindow, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON,
    SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE, WM_DWMCOMPOSITIONCHANGED, WM_NCCALCSIZE,
    WM_NCHITTEST, WM_NCLBUTTONUP,
};

const SUBCLASS_ID: usize = 0x5071_C001;

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

/// Pure-fn translation from a `WM_NCLBUTTONUP` `wparam` (the HT* code)
/// to the action the chrome should execute. Returns `None` when the
/// hit-test code is not a caption-button code so the default proc
/// handles it (drag, etc.).
///
/// Extracted as a free function so it can be unit-tested without a
/// real HWND or Windows event loop.
#[must_use]
pub fn caption_action_for(ht: u32) -> Option<CaptionAction> {
    CaptionButton::from_hit_test(ht).map(CaptionButton::action)
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
            let hit = |r: &sonic_shared::tabbar_view::Rect| {
                local_x >= r.x && local_x < r.x + r.w && local_y >= r.y && local_y < r.y + r.h
            };
            if hit(&close) {
                LRESULT(HTCLOSE as isize)
            } else if hit(&max) {
                LRESULT(HTMAXBUTTON as isize)
            } else if hit(&min) {
                LRESULT(HTMINBUTTON as isize)
            } else {
                // Drag strip; anything not over a button is the caption.
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_NCLBUTTONUP => {
            // wparam carries the HT* code our WM_NCHITTEST returned.
            if let Some(action) = caption_action_for(wparam.0 as u32) {
                unsafe { perform_caption_action(hwnd, action) };
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_DWMCOMPOSITIONCHANGED => {
            unsafe { extend_frame(hwnd) };
            LRESULT(0)
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn perform_caption_action(hwnd: HWND, action: CaptionAction) {
    match action {
        CaptionAction::Minimize => {
            let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        }
        CaptionAction::ToggleMaximize => {
            let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
            let cmd = if zoomed { SW_RESTORE } else { SW_MAXIMIZE };
            let _ = unsafe { ShowWindow(hwnd, cmd) };
        }
        CaptionAction::Close => {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
    }
}

// Suppress dead-code warning for HTCLIENT — kept imported so future
// edits to the hit-test that want to fall through to the client area
// don't have to rediscover the symbol.
const _: u32 = HTCLIENT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_action_for_known_hits() {
        assert_eq!(caption_action_for(HTMINBUTTON), Some(CaptionAction::Minimize));
        assert_eq!(caption_action_for(HTMAXBUTTON), Some(CaptionAction::ToggleMaximize));
        assert_eq!(caption_action_for(HTCLOSE), Some(CaptionAction::Close));
    }

    #[test]
    fn caption_action_for_other_codes_is_none() {
        // HTCAPTION (2) and HTCLIENT (1) are not button hits.
        assert_eq!(caption_action_for(HTCAPTION), None);
        assert_eq!(caption_action_for(HTCLIENT), None);
        assert_eq!(caption_action_for(0), None);
    }
}
