//! Windows stub for [`sonic_shared::os_drag::OsDragSink`].
//!
//! Real OLE drag-and-drop (`IDataObject` / `IDropSource` /
//! `DoDragDrop`) requires:
//!
//!   * registering the custom clipboard format with
//!     `RegisterClipboardFormatW("com.sonic-terminal.tab.v1")`,
//!   * implementing an `IDataObject` COM object that exposes the
//!     registered format as a `CF_HGLOBAL` blob,
//!   * implementing an `IDropSource` COM object whose
//!     `QueryContinueDrag` polls modifier keys, and
//!   * subclassing the winit-owned HWND so its `WM_LBUTTONDOWN` path
//!     can invoke `DoDragDrop` with the right thread affinity.
//!
//! That's a substantial chunk of unsafe COM work that needs a real
//! Windows machine to validate. v1 ships a stub that logs at warn
//! level — the macOS path is fully wired and the cross-platform
//! `TabPayload` plumbing is ready for the Windows impl to slot in.
//
// FUTURE: Windows implementation. Skeleton sketch:
//   1. lazy_static! { static ref CF_SONIC_TAB: u16 =
//          unsafe { RegisterClipboardFormatW(w!("com.sonic-terminal.tab.v1")) }; }
//   2. struct SonicDataObject(TabPayload); impl IDataObject for ... { ... }
//   3. struct SonicDropSource; impl IDropSource for ... { ... }
//   4. unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) }
//   5. Register HWND as IDropTarget for receiving side.

#![cfg(target_os = "windows")]

use std::sync::Arc;

use sonic_shared::os_drag::{DragAck, OsDragSink, TabPayload};

pub struct WinOsDragSink;

impl WinOsDragSink {
    pub fn arc() -> Arc<dyn OsDragSink> {
        Arc::new(WinOsDragSink)
    }
}

impl OsDragSink for WinOsDragSink {
    fn begin_drag(&self, payload: &TabPayload) -> DragAck {
        // DATA-LOSS FIX (PR #59 review): a stub that "drops" the
        // payload but lets the caller kill the source tab destroys
        // the user's live shell. Return NotAcknowledged so
        // `try_os_drag_handoff` falls back to the in-process
        // tear-out path (child window) instead.
        tracing::warn!(
            tab = %payload.tab_title,
            "OS drag not yet implemented on Windows — falling back to in-process tear-out (source tab preserved)"
        );
        DragAck::NotAcknowledged
    }
}

/// No-op receiver matching the macOS [`take_pending_payload`] shape.
pub fn take_pending_payload() -> Option<TabPayload> {
    None
}
