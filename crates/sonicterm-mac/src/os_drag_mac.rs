//! macOS implementation of [`sonicterm_app::os_drag::OsDragSink`].
//!
//! ## What this does on real hardware
//!
//! When `sonicterm-shared` detects a tab tear-out whose cursor has left
//! every SonicTerm window, it calls [`MacOsDragSink::begin_drag`] with a
//! serialized [`TabPayload`]. We write the JSON to the **general
//! NSPasteboard** under the custom type
//! [`sonicterm_app::os_drag::PASTEBOARD_TYPE`] (`com.sonic-terminal.tab.v1`).
//!
//! A second running `SonicTerm.app` instance polls the pasteboard on
//! window focus (`NSApplicationDidBecomeActive`) and consumes any
//! pending payload by spawning a fresh tab with the supplied
//! cwd/cmd/env. Source kills its local tab BEFORE the destination
//! reads — so there is at most one live shell per torn tab even if
//! the user is hyper-active with the cursor.
//!
//! ## Why not a full `NSDragging` session
//!
//! `NSDraggingSession` requires the source `NSWindow` / `NSView` to
//! emit `beginDraggingSessionWithItems:event:source:` from a
//! mouse-event handler the AppKit run loop directly invoked. winit
//! intercepts mouse events at the `NSWindow` level and re-emits them
//! through its own delegate, which means by the time `sonicterm-shared`
//! decides to start a drag, AppKit no longer considers the current
//! event a drag-eligible mouse-down. Workarounds (re-injecting an
//! `NSEvent`, swizzling winit's delegate) are large and fragile.
//!
//! v1 ships pasteboard-based handoff: same observable user behavior
//! ("drag tab out → it appears in the other SonicTerm"), no drag preview
//! image follows the cursor across the screen. We file the preview
//! image work as a v2 follow-up.
//
// FUTURE: implement true NSDraggingSession once we either (a) write
// our own NSView atop CAMetalLayer and bypass winit's view, or (b)
// upstream a winit hook that exposes the live NSEvent to user code.

#![cfg(target_os = "macos")]

use std::sync::Arc;

use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSString};
use sonicterm_app::os_drag::{DragAck, OsDragSink, TabPayload, PASTEBOARD_TYPE};

/// Sink that posts dragged-tab payloads to the macOS general
/// pasteboard under [`PASTEBOARD_TYPE`].
pub struct MacOsDragSink;

impl MacOsDragSink {
    /// Build the sink as an `Arc<dyn OsDragSink>` ready to pass to
    /// `MacShell::with_os_drag_sink`.
    pub fn arc() -> Arc<dyn OsDragSink> {
        Arc::new(MacOsDragSink)
    }
}

impl OsDragSink for MacOsDragSink {
    fn begin_drag(&self, payload: &TabPayload) -> DragAck {
        let json = match payload.to_json() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(?e, "os_drag_mac: payload serialization failed");
                return DragAck::NotAcknowledged;
            }
        };
        // FFI to AppKit. The general pasteboard is documented
        // main-thread-safe; we never store the returned pointers
        // across run loop iterations.
        let pasteboard: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        let type_str: Retained<NSString> = NSString::from_str(PASTEBOARD_TYPE);
        let types: Retained<NSArray<NSString>> =
            NSArray::from_retained_slice(std::slice::from_ref(&type_str));
        let _ = unsafe { pasteboard.declareTypes_owner(&types, None) };
        let value: Retained<NSString> = NSString::from_str(&json);
        let ok = pasteboard.setString_forType(&value, &type_str);
        if ok {
            tracing::info!(
                tab = %payload.tab_title,
                bytes = json.len(),
                "os_drag_mac: payload written to NSPasteboard"
            );
        } else {
            tracing::warn!("os_drag_mac: NSPasteboard.setString_forType returned NO");
        }
        // DATA-LOSS FIX (PR #59 review): even a successful
        // pasteboard write is NOT a consumption ack — no receiver
        // may ever pick it up. Until v2 adds a reply-key heartbeat
        // we always tell the caller to keep the source tab alive.
        DragAck::NotAcknowledged
    }
}

/// Read any pending payload off the general pasteboard, returning it
/// and clearing the slot only after a valid SonicTerm payload is
/// observed. Returns `None` when no SonicTerm payload is present (the
/// common case — most pasteboard writes are unrelated text). Called
/// by the destination process on application activation.
///
/// DATA-LOSS FIX (PR #59 review): we previously called
/// `clearContents()` *before* validating the JSON, which would wipe
/// arbitrary unrelated clipboard contents from other apps whenever
/// any string happened to be tagged with our type. Now we validate
/// first and only clear on a successful round-trip.
pub fn take_pending_payload() -> Option<TabPayload> {
    let pasteboard: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
    let type_str: Retained<NSString> = NSString::from_str(PASTEBOARD_TYPE);
    let value = pasteboard.stringForType(&type_str)?;
    let s = value.to_string();
    match TabPayload::from_json(&s) {
        Ok(p) => {
            let _ = pasteboard.clearContents();
            Some(p)
        }
        Err(e) => {
            tracing::warn!(
                ?e,
                "os_drag_mac: pasteboard JSON malformed; ignoring (and NOT clearing)"
            );
            None
        }
    }
}
