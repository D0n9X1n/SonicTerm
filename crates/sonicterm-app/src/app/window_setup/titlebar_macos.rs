//! macOS titlebar metrics and preferences, read from AppKit on the main thread.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSView, NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSUserDefaults};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_ui::window_band::WindowBand;
use winit::window::Window;

/// The AppKit window behind `window`, on the main thread.
fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    MainThreadMarker::new()?;
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        // When: the handle is not AppKit's, there is no NSWindow to read.
        return None;
    };
    let view =
        // SAFETY: appkit.ns_view is winit's live NSView for this window; it outlives this call, on the main thread.
        unsafe { appkit.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

/// The traffic lights' band: the close button's vertical centre and the zoom button's right edge,
/// in the content view's top-left coordinates, logical px. `None` when the buttons are missing.
pub(super) fn traffic_light_band(window: &Window) -> Option<WindowBand> {
    let ns_window = ns_window(window)?;
    let content = ns_window.contentView()?;
    let close = ns_window.standardWindowButton(NSWindowButton::CloseButton)?;
    let zoom = ns_window.standardWindowButton(NSWindowButton::ZoomButton)?;
    let height = content.frame().size.height;
    // Each button's frame converted into the content view; AppKit's y axis points up.
    let to_content = |button: &NSView| -> NSRect {
        let frame = button.frame();
        let parent =
            // SAFETY: superview only reads the button's view hierarchy, on the main thread.
            unsafe { button.superview() };
        parent.map_or(frame, |parent| parent.convertRect_toView(frame, Some(&content)))
    };
    let close_rect = to_content(&close);
    let zoom_rect = to_content(&zoom);
    let flipped = content.isFlipped();
    let close_mid = close_rect.origin.y + close_rect.size.height / 2.0;
    // AppKit's y axis points up unless the view is flipped; the band measures from the top.
    let centre_from_top = height - close_mid;
    let centre_from_top = if flipped { close_mid } else { centre_from_top };
    let zoom_right = zoom_rect.origin.x + zoom_rect.size.width;
    Some(WindowBand::traffic_lights(centre_from_top as f32, zoom_right as f32))
}

/// The system titlebar height for a titled window, logical px: the frame a titled window needs
/// around a content rect, less that rect. AppKit answers this before any window exists.
pub(super) fn titlebar_height_estimate() -> f32 {
    /// Fallback when AppKit cannot be asked: the standard titlebar height.
    const STANDARD_TITLEBAR: f32 = 28.0;
    let Some(mtm) = MainThreadMarker::new() else {
        // When: MainThreadMarker::new is None, AppKit cannot be asked off the main thread.
        return STANDARD_TITLEBAR;
    };
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
    let frame = NSWindow::frameRectForContentRect_styleMask(
        content,
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
        mtm,
    );
    (frame.size.height - content.size.height) as f32
}

/// The user's `AppleActionOnDoubleClick` preference, if set.
pub(super) fn double_click_setting() -> Option<String> {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("AppleActionOnDoubleClick");
    defaults.stringForKey(&key).map(|value| value.to_string())
}

/// The click count of the event AppKit is delivering now.
pub(super) fn current_click_count() -> Option<u32> {
    let mtm = MainThreadMarker::new()?;
    let event = NSApplication::sharedApplication(mtm).currentEvent()?;
    u32::try_from(event.clickCount()).ok()
}

/// Switch the live `window` to the integrated titlebar (`integrated`) or the system one: the same
/// full-size content view, transparent titlebar and hidden title that creation sets, applied to
/// the running `NSWindow` on the main thread. As winit does after a style-mask change, the view
/// takes first responder again so keys and IME keep reaching it. Returns whether AppKit was reached.
pub(super) fn apply_titlebar(window: &Window, integrated: bool) -> bool {
    let Some(ns_window) = ns_window(window) else {
        // When: ns_window is None, there is no AppKit window to switch on this thread.
        return false;
    };
    let mut mask = ns_window.styleMask();
    mask.set(NSWindowStyleMask::FullSizeContentView, integrated);
    ns_window.setStyleMask(mask);
    if let Some(view) = ns_window.contentView() {
        // When: contentView is Some, it takes first responder again; a style-mask change can
        // leave the window without a key view, as winit notes.
        let _ = ns_window.makeFirstResponder(Some(&view));
    }
    ns_window.setTitlebarAppearsTransparent(integrated);
    // The integrated titlebar hides the title; the system titlebar shows it again.
    let visibility =
        if integrated { NSWindowTitleVisibility::Hidden } else { NSWindowTitleVisibility::Visible };
    ns_window.setTitleVisibility(visibility);
    true
}

/// Whether the live `window` now has the integrated titlebar's full-size content view, as AppKit
/// reports it; `None` off the main thread or without an AppKit window.
pub(super) fn has_full_size_content(window: &Window) -> Option<bool> {
    let ns_window = ns_window(window)?;
    Some(ns_window.styleMask().contains(NSWindowStyleMask::FullSizeContentView))
}

/// Whether AppKit has `window` in full screen, or still animating out of it: its full-screen
/// style-mask bit stays set until the exit transition ends, unlike winit's own state, which clears
/// before the animation. `None` off the main thread or without an AppKit window.
pub(super) fn in_full_screen(window: &Window) -> Option<bool> {
    let ns_window = ns_window(window)?;
    Some(ns_window.styleMask().contains(NSWindowStyleMask::FullScreen))
}
