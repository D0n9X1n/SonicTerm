//! The Windows 11 snap layouts flyout on SonicTerm's self-drawn maximise button, as pure decisions.
//! Windows offers the flyout only when `WM_NCHITTEST` answers `HTMAXBUTTON` over the button; the
//! mouse messages Windows then sends for that spot are non-client ones, which winit does not turn
//! into pointer events. This module decides, per message, how the Windows subclass hands them to
//! winit as client messages, so hover, press, release-outside cancel and click keep their in-app
//! behaviour. It reads no operating system state, so every host tests it.

use sonicterm_ui::window_band::{caption_button_at, BandButtons, CaptionButton, WindowBand};

/// `WM_NCHITTEST`.
pub(crate) const WM_NCHITTEST: u32 = 0x0084;
/// `WM_NCMOUSEMOVE`.
pub(crate) const WM_NCMOUSEMOVE: u32 = 0x00A0;
/// `WM_NCLBUTTONDOWN`.
pub(crate) const WM_NCLBUTTONDOWN: u32 = 0x00A1;
/// `WM_NCLBUTTONUP`.
pub(crate) const WM_NCLBUTTONUP: u32 = 0x00A2;
/// `WM_NCLBUTTONDBLCLK`.
pub(crate) const WM_NCLBUTTONDBLCLK: u32 = 0x00A3;
/// `WM_NCMOUSELEAVE`.
pub(crate) const WM_NCMOUSELEAVE: u32 = 0x02A2;
/// `WM_MOUSELEAVE`.
pub(crate) const WM_MOUSELEAVE: u32 = 0x02A3;
/// `WM_MOUSEMOVE`.
pub(crate) const WM_MOUSEMOVE: u32 = 0x0200;
/// `WM_CAPTURECHANGED`.
pub(crate) const WM_CAPTURECHANGED: u32 = 0x0215;
/// `HTCLIENT`.
pub(crate) const HTCLIENT: isize = 1;
/// `HTMAXBUTTON`.
pub(crate) const HTMAXBUTTON: isize = 9;

/// What the subclass needs to find the maximise button: the band SonicTerm last drew, its height
/// in raster px and the display scale. Only a band with caption buttons has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SnapTarget {
    /// The band, in logical px.
    pub band: WindowBand,
    /// The band's height in raster (physical) px.
    pub band_height: f32,
    /// The window's display scale.
    pub scale: f32,
}

impl SnapTarget {
    /// The target for `band`, or `None` when the band draws no caption buttons (native titlebar,
    /// full screen, macOS), so the subclass passes every message through.
    pub(crate) fn of(band: WindowBand, band_height: f32, scale: f32) -> Option<Self> {
        (band.integrated && band.buttons == BandButtons::Caption).then_some(Self {
            band,
            band_height,
            scale,
        })
    }

    /// Whether client point `(x_px, y_px)` (physical px) of a client area `client_width` wide is on the
    /// maximise button, with the geometry the App hit-tests presses with.
    pub(crate) fn on_maximize(&self, client_width: f32, x_px: f32, y_px: f32) -> bool {
        caption_button_at(&self.band, client_width, self.band_height, self.scale, x_px, y_px)
            == Some(CaptionButton::Maximize)
    }
}

/// Where the cursor is, for the subclass: on the maximise button, elsewhere in the client area, or
/// outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// On the maximise button.
    Maximize,
    /// In the client area, off the maximise button.
    Client,
    /// Outside the client area.
    Outside,
}

/// The subclass's mouse-tracking memory for one window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SnapTracking {
    /// A client `WM_MOUSELEAVE` was swallowed while the cursor sat on the maximise button, so winit
    /// still counts the cursor inside and its leave tracking is spent: the next real client move
    /// re-arms it, or the next non-client leave hands winit the leave.
    pub lost_leave: bool,
}

/// What the subclass does with one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapAction {
    /// Hand the message to the next procedure unchanged.
    Pass,
    /// `WM_NCHITTEST`: run the default, then answer `HTMAXBUTTON` if it said `HTCLIENT` over the
    /// maximise button.
    HitTest,
    /// A non-client move over the button: send winit a client move there, arm non-client leave
    /// tracking, then pass the original on so Windows times the flyout.
    Hover,
    /// A non-client press or double-click on the button: send winit a client move and left press
    /// there (winit captures the mouse, so the release arrives as a client message); return 0.
    Press,
    /// Return 0 without running the default: a non-client release on the button must not run the
    /// system's own maximise, and a client leave over the button must not reach winit.
    Swallow,
    /// A client leave while the cursor sits on the button: swallow it, remember it, and arm
    /// non-client leave tracking, which reports when the cursor really goes.
    SwallowLeave,
    /// A real client move after a swallowed leave: re-arm client leave tracking, then pass it on.
    RearmClient,
    /// The cursor left after a swallowed leave: send winit the client leave, then pass it on.
    ForwardLeave,
    /// Capture ended with the cursor still on the button: arm non-client leave tracking, pass on.
    ArmNonClient,
}

/// The action for message `message`. `nc_maximize` is whether a non-client message's hit code
/// (its `wparam`) is `HTMAXBUTTON`; `spot` is where the cursor is now, and `captured` whether this
/// window holds the mouse capture. Returns the action and the tracking to store before acting.
pub(crate) fn route(
    message: u32,
    nc_maximize: bool,
    spot: Spot,
    captured: bool,
    tracking: SnapTracking,
) -> (SnapAction, SnapTracking) {
    let lost = tracking.lost_leave;
    match message {
        WM_NCHITTEST => (SnapAction::HitTest, tracking),
        WM_NCMOUSEMOVE if nc_maximize => (SnapAction::Hover, tracking),
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if nc_maximize => (SnapAction::Press, tracking),
        WM_NCLBUTTONUP if nc_maximize => (SnapAction::Swallow, tracking),
        WM_MOUSELEAVE if spot == Spot::Maximize && !captured => {
            // The cursor sits on the button, so winit's leave is spurious: keep it inside.
            (SnapAction::SwallowLeave, SnapTracking { lost_leave: true })
        }
        WM_MOUSEMOVE if lost && spot == Spot::Client => {
            // Lost, and the cursor is back in the client area: winit's tracking resumes.
            (SnapAction::RearmClient, SnapTracking { lost_leave: false })
        }
        WM_NCMOUSELEAVE if lost && !captured => {
            // Lost and not captured: the cursor left the button for another window or for
            // our client area; either way winit gets its leave, and a return re-enters normally.
            (SnapAction::ForwardLeave, SnapTracking { lost_leave: false })
        }
        WM_CAPTURECHANGED if lost => capture_ended(spot),
        _ => (SnapAction::Pass, tracking),
    }
}

/// The action when this window loses the mouse capture after a swallowed leave, by where the
/// cursor is: re-arm client tracking, keep watching the button, or hand winit the leave.
fn capture_ended(spot: Spot) -> (SnapAction, SnapTracking) {
    match spot {
        Spot::Client => (SnapAction::RearmClient, SnapTracking { lost_leave: false }),
        Spot::Maximize => (SnapAction::ArmNonClient, SnapTracking { lost_leave: true }),
        Spot::Outside => (SnapAction::ForwardLeave, SnapTracking { lost_leave: false }),
    }
}

/// The signed screen or client point packed in `lparam`: x in the low word, y in the high word,
/// each a signed 16-bit value, so monitors left of or above the primary give negative values.
pub(crate) fn point_of(lparam: isize) -> (i32, i32) {
    let x_px = i32::from(lparam as u16 as i16);
    let y_px = i32::from((lparam >> 16) as u16 as i16);
    (x_px, y_px)
}

/// `(x_px, y_px)` packed as a mouse message's `lparam`, each coordinate as a signed 16-bit word.
pub(crate) fn lparam_of(x_px: i32, y_px: i32) -> isize {
    let low = (x_px as i16 as u16) as isize;
    let high = ((y_px as i16 as u16) as isize) << 16;
    low | high
}

#[cfg(test)]
#[path = "snap_route_tests.rs"]
mod snap_route_tests;
