//! macOS titlebar metrics and preferences, read from AppKit on the main thread.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSView, NSWindow, NSWindowButton, NSWindowStyleMask};
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
