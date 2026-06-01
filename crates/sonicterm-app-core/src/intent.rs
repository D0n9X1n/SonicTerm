//! Winit-agnostic intent enum: what the app loop wants the platform
//! shell to do next. The platform layer (sonicterm-app) maps these into
//! winit/wgpu/arboard calls.

use sonicterm_types::Action;

/// A unit of work the app-core asks the platform layer to perform.
#[derive(Debug, Clone)]
pub enum AppIntent {
    /// Request a redraw of the active window.
    Redraw(RedrawReason),
    /// Resize the active surface.
    Resize {
        /// New surface width in physical pixels.
        width_px: u32,
        /// New surface height in physical pixels.
        height_px: u32,
    },
    /// Dispatch a user `Action` (keymap-resolved).
    DispatchAction(Action),
    /// Set the active window title.
    SetTitle(String),
    /// Quit the app.
    Quit,
}

/// Why a redraw was requested. The platform layer may use this to
/// coalesce (see LM-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawReason {
    /// New bytes arrived from a PTY.
    PtyBytes,
    /// User keystroke (immediate paint).
    UserInput,
    /// Window resize or DPI change.
    SurfaceChange,
    /// Cursor blink tick.
    CursorBlink,
    /// Theme or font reload.
    ConfigReload,
}
