//! macOS `OsTabDragBackend` implementation.
//!
//! Wires `begin_session` to the real AppKit pasteboard write that the
//! cross-process path already proved out, and posts a
//! synchronous terminal [`DragOutcome`] back through the
//! [`AppHandle`] so the App's `UserEvent::DragEnded` dispatcher
//! releases its in-flight bookkeeping cleanly.
//!
//! ## Real work this performs
//!
//! 1. Builds a lightweight source identifier when the caller cannot serialize
//!    the full [`TabPayload`].
//! 2. Writes the full payload (or fallback identifier) to the general
//!    `NSPasteboard` under [`sonicterm_app::os_drag::PASTEBOARD_TYPE`].
//!    This makes it available to a later process activation; it does not prove
//!    that any receiver adopted the payload.
//! 3. Stashes the [`AppHandle`] for a future native AppKit callback path.
//! 4. Posts [`DragOutcome::Cancelled`] synchronously so the app clears the
//!    in-flight gesture and preserves the source tab. Same-process tab merging
//!    remains handled by SonicTerm's in-process geometry/transfer path.
//!
//! ## AppKit drag initiation constraint
//!
//! `beginDraggingSessionWithItems:event:source:` requires the source
//! `NSView` to emit the call from a mouse-event handler the AppKit
//! run loop *directly* invoked. winit intercepts mouse events at the
//! `NSWindow` level and re-emits them through its own delegate, so by
//! the time `sonicterm-app` decides to start a drag, AppKit no longer
//! considers the current event a drag-eligible mouse-down. This is
//! the same constraint already documented in
//! [`crate::os_drag_mac`]; lifting it requires either (a) a custom
//! `NSView` atop `CAMetalLayer` that owns mouse-down handling
//! directly, or (b) a winit hook that exposes the live `NSEvent` to
//! user code. Neither boundary is available through the current winit
//! integration, so native cursor capture cannot start from this backend.
//!
//! Until that lifts, this backend is a pasteboard publisher plus a synchronous
//! cancellation callback. It does not implement native cursor capture or a
//! same-process AppKit drop target.

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSString};
use sonicterm_app::app::os_drag::{
    AppHandle, BackendWindowId as WindowId, DragOutcome, OsTabDragBackend,
};
use sonicterm_app::os_drag::PASTEBOARD_TYPE;

/// Production `OsTabDragBackend` impl for macOS. Holds the most
/// recently stashed [`AppHandle`] so AppKit callback hooks (future
/// NSDraggingSource subclass) can post back to the winit main loop.
#[allow(dead_code)] // wired in production via `sonicterm-mac::main` (MacShell::with_os_drag_backend slot)
pub struct MacOsTabDragBackend {
    handle_slot: std::sync::Mutex<Option<AppHandle>>,
}

#[allow(dead_code)]
impl MacOsTabDragBackend {
    /// Construct a backend with an empty handle slot.
    pub fn new() -> Self {
        Self { handle_slot: std::sync::Mutex::new(None) }
    }

    /// Box-wrapped constructor for `App::set_os_drag_backend`.
    pub fn boxed() -> Box<dyn OsTabDragBackend> {
        Box::new(Self::new())
    }
}

impl Default for MacOsTabDragBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize the in-process drag identifier carried on the
/// `com.sonic-terminal.tab.v1` pasteboard type. Peer
/// NSDraggingDestination instances on other SonicTerm windows read this
/// to confirm the payload is one of ours.
#[allow(dead_code)]
fn build_payload_json(source_window: WindowId, source_tab_idx: usize) -> String {
    // WindowId Debug is stable per winit version and unique per
    // window — adequate as an opaque tag for peer recognition.
    format!(r#"{{"src_window_id":"{:?}","src_tab_idx":{}}}"#, source_window, source_tab_idx)
}

/// Write `json` to the general pasteboard under [`PASTEBOARD_TYPE`].
/// Returns `true` on success. Real AppKit FFI; same code path as
/// [`crate::os_drag_mac::MacOsDragSink::begin_drag`].
#[allow(dead_code)]
fn write_payload_to_pasteboard(json: &str) -> bool {
    let pasteboard: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
    let type_str: Retained<NSString> = NSString::from_str(PASTEBOARD_TYPE);
    let types: Retained<NSArray<NSString>> =
        NSArray::from_retained_slice(std::slice::from_ref(&type_str));
    let _ =
        // SAFETY: retained pasteboard/type objects outlive this synchronous declaration; owner is intentionally nil.
        unsafe { pasteboard.declareTypes_owner(&types, None) };
    let value: Retained<NSString> = NSString::from_str(json);
    pasteboard.setString_forType(&value, &type_str)
}

impl OsTabDragBackend for MacOsTabDragBackend {
    fn begin_session(
        &mut self,
        handle: AppHandle,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    ) {
        // Stash the handle for any AppKit callback path.
        let mut slot = match self.handle_slot.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *slot = Some(handle.clone());
        drop(slot);

        // Prefer the full TabPayload schema from the caller. If for
        // any reason the caller passed an empty string (e.g. payload
        // serialization failed upstream — try_os_drag_handoff
        // tolerates that), fall back to the lightweight identifier
        // tuple so the pasteboard still carries a non-empty
        // recognisable blob.
        let json = if payload_json.is_empty() {
            build_payload_json(source_window, source_tab_idx)
        } else {
            // When: `payload_json` is nonempty, preserve the full serialized tab payload for later adoption.
            payload_json
        };

        // Publish the payload for a later SonicTerm process activation. This
        // does not acknowledge adoption and does not move a same-process tab.
        let wrote = write_payload_to_pasteboard(&json);

        tracing::info!(
            ?source_window,
            source_tab_idx,
            image_bytes = drag_image_png.len(),
            pasteboard_wrote = wrote,
            "MacOsTabDragBackend::begin_session — pasteboard payload published"
        );

        // Release the app's in-flight bookkeeping without transferring or
        // deleting the source tab. A later process may independently consume
        // the pasteboard payload and spawn a fresh shell from it.
        handle.post_drag_ended(DragOutcome::Cancelled);
    }

    fn register_window(
        &mut self,
        _handle: AppHandle,
        _window_id: sonicterm_app::app::os_drag::BackendWindowId,
        _window: &std::sync::Arc<sonicterm_app::app::os_drag::BackendWindow>,
    ) {
        // macOS uses NSPasteboard publish/subscribe — there is no
        // per-window IDropTarget equivalent to register. Implemented
        // for trait consistency only; is Windows-only
        // in practice.
    }
}
