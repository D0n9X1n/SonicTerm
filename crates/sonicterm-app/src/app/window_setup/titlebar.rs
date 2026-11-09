//! The integrated titlebar's native side: the window attributes it is created with, the band
//! metrics read back from the platform, and the titlebar gestures SonicTerm performs itself.

use sonicterm_cfg::config::TitlebarMode;
use sonicterm_ui::window_band::WindowBand;
use winit::window::{Window, WindowAttributes};

#[cfg(target_os = "macos")]
#[path = "titlebar_macos.rs"]
mod macos;

/// Whether a terminal window created with `mode` gets the integrated band on this platform.
pub(crate) fn integrated(mode: TitlebarMode) -> bool {
    mode == TitlebarMode::Integrated && cfg!(any(target_os = "macos", target_os = "windows"))
}

/// `attrs` for a terminal window with titlebar `mode`. Integrated on macOS: a transparent titlebar
/// over a full-size content view with its title text hidden (the title is still set). Integrated
/// on Windows: an undecorated window that keeps its drop shadow. Native, Linux and report windows
/// keep the system titlebar.
pub(crate) fn titlebar_attributes(attrs: WindowAttributes, mode: TitlebarMode) -> WindowAttributes {
    if !integrated(mode) {
        // When: integrated(mode) is false, the system titlebar stays and attrs are unchanged.
        return attrs;
    }
    #[cfg(target_os = "macos")]
    let attrs = {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attrs
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
    };
    #[cfg(target_os = "windows")]
    let attrs = {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs.with_decorations(false).with_undecorated_shadow(true)
    };
    attrs
}

/// The band window `window`, created with titlebar `mode`, has now, logical px: the traffic
/// lights' centre and right edge on macOS, the caption buttons on Windows, and no buttons in full
/// screen; a native window has none.
pub(crate) fn read_window_band(window: &Window, mode: TitlebarMode) -> WindowBand {
    if !integrated(mode) {
        // When: integrated(mode) is false, the system titlebar means no band.
        return WindowBand::default();
    }
    if window.fullscreen().is_some() {
        // When: full screen hides the window buttons, the band holds only the name.
        return WindowBand::buttonless();
    }
    #[cfg(target_os = "macos")]
    {
        macos::traffic_light_band(window).unwrap_or_else(WindowBand::buttonless)
    }
    #[cfg(not(target_os = "macos"))]
    {
        WindowBand::caption()
    }
}

/// Logical px the titlebar band adds above the strip at creation for titlebar `mode`, before the
/// window exists to measure: the system titlebar height on macOS, the caption height on Windows.
pub(crate) fn creation_band_estimate(mode: TitlebarMode) -> f32 {
    if !integrated(mode) {
        // When: integrated(mode) is false, the system titlebar adds nothing to the inner size.
        return 0.0;
    }
    #[cfg(target_os = "macos")]
    {
        macos::titlebar_height_estimate()
    }
    #[cfg(not(target_os = "macos"))]
    {
        sonicterm_ui::window_band::CAPTION_BUTTON_HEIGHT
    }
}

/// Keep an integrated Windows window's drop shadow only outside full screen: winit draws the
/// undecorated shadow by moving the client area one pixel, which full screen must not have.
pub(crate) fn sync_fullscreen_shadow(window: &Window, mode: TitlebarMode) {
    #[cfg(target_os = "windows")]
    if integrated(mode) {
        // The shadow follows full screen; setting the same value again is a no-op inside winit.
        use winit::platform::windows::WindowExtWindows;
        window.set_undecorated_shadow(window.fullscreen().is_none());
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (window, mode);
}

/// What a double-click on empty titlebar space does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoubleClickAction {
    /// Zoom (macOS) or maximise/restore (Windows).
    Maximize,
    /// Minimise the window.
    Minimize,
    /// Nothing.
    None,
}

/// The macOS `AppleActionOnDoubleClick` value `setting` as an action; an absent or unknown value
/// is the system default, zoom.
pub(crate) fn double_click_action(setting: Option<&str>) -> DoubleClickAction {
    match setting {
        Some("Minimize") => DoubleClickAction::Minimize,
        Some("None") => DoubleClickAction::None,
        _ => DoubleClickAction::Maximize,
    }
}

/// The system's titlebar double-click action: the user's macOS preference; maximise on Windows.
pub(crate) fn system_double_click_action() -> DoubleClickAction {
    #[cfg(target_os = "macos")]
    {
        double_click_action(macos::double_click_setting().as_deref())
    }
    #[cfg(not(target_os = "macos"))]
    {
        DoubleClickAction::Maximize
    }
}

/// The system's double-click time and the half-size of its double-click rectangle in raster px:
/// `GetDoubleClickTime` and `SM_CXDOUBLECLK`/`SM_CYDOUBLECLK` on Windows; the Windows defaults
/// (500 ms, a 4-pixel rectangle) where no setting is read.
pub(crate) fn system_double_click_limits() -> (std::time::Duration, f32) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK,
        };
        let (millis, width, height) =
            // SAFETY: GetDoubleClickTime and GetSystemMetrics take no pointers and only read settings.
            unsafe {
                (
                    GetDoubleClickTime(),
                    GetSystemMetrics(SM_CXDOUBLECLK),
                    GetSystemMetrics(SM_CYDOUBLECLK),
                )
            };
        let half = (width.max(height).max(1) as f32) / 2.0;
        (std::time::Duration::from_millis(u64::from(millis.max(1))), half)
    }
    #[cfg(not(target_os = "windows"))]
    {
        (std::time::Duration::from_millis(500), 4.0)
    }
}

/// Run `action` on `window`.
pub(crate) fn run_double_click(window: &Window, action: DoubleClickAction) {
    match action {
        DoubleClickAction::Maximize => window.set_maximized(!window.is_maximized()),
        DoubleClickAction::Minimize => window.set_minimized(true),
        DoubleClickAction::None => {
            // When: DoubleClickAction::None, the user turned the titlebar double-click off.
        }
    }
}

/// The click count of the native event being handled, on macOS; `None` elsewhere, where the App
/// counts clicks itself with the system's double-click time.
pub(crate) fn native_click_count() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        macos::current_click_count()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
#[path = "titlebar_tests.rs"]
mod titlebar_tests;
