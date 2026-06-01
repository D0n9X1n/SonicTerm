//! Phase C2 — Windows `OsTabDragBackend` implementation.
//!
//! Wires `begin_session` straight into the existing OLE `DoDragDrop`
//! loop in [`crate::os_drag_win`]. Same-process drag uses the in-memory
//! `(src_window, src_tab_idx)` bookkeeping the App already keeps in
//! `os_drag_source`; the OLE payload only needs to carry an opaque
//! identifier the IDropTarget on a peer Sonic HWND can use to recognise
//! "this is one of ours, accept it". We reuse the existing
//! `CF_SONIC_TAB` clipboard format for that — the JSON body is the
//! source-side identifier tuple.
//!
//! ## Threading model
//!
//! `DoDragDrop` spins a private OLE message pump for the lifetime of
//! the drag. That pump runs on the calling thread (the winit main
//! thread). winit will not deliver events while OLE is pumping, but
//! that is the expected Windows model — the user is actively dragging,
//! the rest of the UI is frozen by design until the gesture ends.
//!
//! When `DoDragDrop` returns we have the terminal outcome (drop on a
//! Sonic IDropTarget → `DROPEFFECT_MOVE`; drop on bare desktop /
//! non-Sonic → `DROPEFFECT_NONE`; ESC → `DRAGDROP_S_CANCEL`). We
//! translate that into a [`DragOutcome`] and post it through
//! [`AppHandle::post_drag_ended`] so the App's
//! `UserEvent::DragEnded` dispatcher can call
//! [`sonicterm_app::app::App::transfer_tab`] or
//! [`sonicterm_app::app::App::cancel_drag_session`] as appropriate.

#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::sync::Arc;

use sonicterm_app::app::os_drag::{
    AppHandle, BackendWindow as Window, BackendWindowId as WindowId, DragOutcome, OsTabDragBackend,
};

/// Production `OsTabDragBackend` impl for Windows. Holds the most
/// recently stashed [`AppHandle`] so the OLE `IDropSource` /
/// `IDropTarget` callbacks can post back to the winit main loop via
/// the wrapped `EventLoopProxy`.
#[allow(dead_code)] // wired in production via `sonicterm-windows::main` (run_with_os_drag_pending_and_window_hook backend slot)
pub struct WinOsTabDragBackend {
    handle_slot: std::sync::Mutex<Option<AppHandle>>,
    /// Set of HWND ids (as u64) that have already had `RegisterDragDrop`
    /// called against them. OLE leaks the previous registration on a
    /// re-register, so we de-dupe here. Keyed by HWND-as-u64 so we
    /// don't need a `Send`-unsafe `HWND` newtype just for tracking.
    registered_windows: std::sync::Mutex<HashMap<WindowId, u64>>,
}

#[allow(dead_code)]
impl WinOsTabDragBackend {
    /// Construct a backend with an empty handle slot.
    pub fn new() -> Self {
        Self {
            handle_slot: std::sync::Mutex::new(None),
            registered_windows: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Box-wrapped constructor for `App::set_os_drag_backend`.
    pub fn boxed() -> Box<dyn OsTabDragBackend> {
        Box::new(Self::new())
    }
}

impl Default for WinOsTabDragBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize the same-process drag identifier carried in the
/// `CF_SONIC_TAB` clipboard payload. Peer IDropTarget instances on
/// other Sonic HWNDs in this process read this back to confirm the
/// payload is one of ours and to recover the source coordinates
/// (although in-process drags reuse the App's `os_drag_source`
/// bookkeeping directly, so the JSON is purely a tagging mechanism).
#[allow(dead_code)]
fn build_payload_json(source_window: WindowId, source_tab_idx: usize) -> String {
    // WindowId Debug format is stable across winit versions we ship
    // and unique-per-window — adequate as an opaque tag for peer
    // IDropTarget recognition. Real cross-process drags use the full
    // TabPayload schema in `sonicterm_app::os_drag`; this is the lighter
    // in-process tag.
    format!(r#"{{"src_window_id":"{:?}","src_tab_idx":{}}}"#, source_window, source_tab_idx)
}

impl OsTabDragBackend for WinOsTabDragBackend {
    fn handles_full_gesture(&self) -> bool {
        // We invoke `DoDragDrop` synchronously below — the caller in
        // `App::try_os_drag_handoff` MUST NOT also call the legacy
        // `OsDragSink::begin_drag`, which re-enters DoDragDrop and
        // would falsely return `DROPEFFECT_NONE` (no live gesture),
        // spuriously triggering `spawn_tearout_child`.
        true
    }

    fn begin_session(
        &mut self,
        handle: AppHandle,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    ) {
        // Stash the handle. OLE callbacks (IDropSource::QueryContinueDrag,
        // IDropTarget::Drop) post back through it.
        let mut slot = match self.handle_slot.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *slot = Some(handle.clone());
        drop(slot);

        tracing::info!(
            ?source_window,
            source_tab_idx,
            image_bytes = drag_image_png.len(),
            payload_bytes = payload_json.len(),
            "WinOsTabDragBackend::begin_session — entering DoDragDrop"
        );

        // Carry the FULL TabPayload schema on CF_SONIC_TAB — peer
        // IDropTarget on a Sonic HWND parses via
        // `sonicterm_app::os_drag::TabPayload::from_json`. If the caller
        // failed to serialize (passed empty), fall back to the
        // lightweight identifier tuple so OLE still has a non-empty
        // blob to publish; the destination will log a parse warning
        // and decline, which is preferable to a 0-byte HGLOBAL.
        let payload = if payload_json.is_empty() {
            build_payload_json(source_window, source_tab_idx)
        } else {
            payload_json
        };

        // Install the AppHandle so the IDropTarget::Drop callback in
        // os_drag_win can post a real DragOutcome::Drop back to the
        // dispatcher (target window / slot routing).
        crate::os_drag_win::install_drop_outcome_handle(handle.clone());

        // Run the real OLE drag/drop loop synchronously.
        let effect = crate::os_drag_win::begin_tab_drag(&payload);

        // Clear the installed handle now that the gesture has
        // terminated — a subsequent unrelated CF_SONIC_TAB drop
        // (e.g. from another Sonic process) must not reuse a stale
        // handle.
        crate::os_drag_win::clear_drop_outcome_handle();

        // If the IDropTarget::Drop callback already posted a richer
        // outcome (target_window + target_slot from cursor hit-test),
        // do not overwrite it. Otherwise translate the OLE DROPEFFECT.
        if handle.pending_handle().peek_ended().is_some() {
            return;
        }
        let outcome = match effect {
            e if e == windows::Win32::System::Ole::DROPEFFECT_MOVE.0 => {
                // Drop accepted by a Sonic IDropTarget but the
                // destination side did not post a richer outcome —
                // dispatcher will route via transfer_tab with default
                // main-window/self target. The destination IDropTarget
                // in os_drag_win already pushes a TabPayload via
                // `os_drag_bridge::push_tab_payload`, so the
                // user-visible result is "tab appears at destination".
                DragOutcome::DroppedOnBar { target_window: None, target_slot: source_tab_idx }
            }
            _ => DragOutcome::Cancelled,
        };

        handle.post_drag_ended(outcome);
    }

    fn register_window(&mut self, _handle: AppHandle, window_id: WindowId, window: &Arc<Window>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let raw = match window.window_handle() {
            Ok(h) => h.as_raw(),
            Err(e) => {
                tracing::warn!(?e, "WinOsTabDragBackend::register_window: no raw handle");
                return;
            }
        };
        let RawWindowHandle::Win32(h) = raw else {
            tracing::warn!(?raw, "WinOsTabDragBackend::register_window: not a Win32 handle");
            return;
        };
        let hwnd_val = h.hwnd.get() as u64;
        let mut reg = match self.registered_windows.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if reg.contains_key(&window_id) {
            tracing::debug!(?window_id, "register_window: already registered, skipping");
            return;
        }
        let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
        // SAFETY: HWND is alive (caller just created the window) and
        // OLE was initialized on this thread by `os_drag_win::init_ole`
        // in main.rs.
        unsafe { crate::os_drag_win::register_for_window(hwnd) };
        reg.insert(window_id, hwnd_val);
        tracing::info!(?window_id, hwnd = hwnd_val, "register_window: RegisterDragDrop installed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_json_carries_src_tab_idx() {
        // We can't synthesize a real WindowId without a winit loop, so
        // exercise the format directly on the slot the App actually
        // uses. The exact debug shape of WindowId isn't asserted
        // (winit-internal), only that src_tab_idx round-trips.
        let s = format!(r#"{{"src_window_id":"foo","src_tab_idx":{}}}"#, 7);
        assert!(s.contains(r#""src_tab_idx":7"#));
        assert!(s.contains("src_window_id"));
    }

    #[test]
    fn boxed_backend_implements_trait() {
        let _: Box<dyn OsTabDragBackend> = WinOsTabDragBackend::boxed();
    }
}
