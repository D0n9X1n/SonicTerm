//! Epic #289 Phase C2 — OS-level drag *session* hookup.
//!
//! This module is distinct from the *cross-process* drag wire format
//! at [`crate::os_drag`]:
//!
//! * [`crate::os_drag`] (top-level) defines the **wire payload**
//!   ([`crate::os_drag::TabPayload`], [`crate::os_drag::PASTEBOARD_TYPE`])
//!   carried between two Sonic *processes* via NSPasteboard / OLE
//!   clipboard. That's the Phase C1 work that already shipped.
//!
//! * **This module** ([`crate::app::os_drag`]) defines the
//!   [`OsTabDragBackend`] trait that lets the App start an OS-level
//!   *drag session* (NSDraggingSession / OLE DoDragDrop) so the cursor
//!   stays captured across window boundaries even while the user is
//!   physically dragging a tab between two Sonic windows of the *same*
//!   process.
//!
//! Phase C ([`crate::app::tab_transfer`]) added the pure
//! [`crate::app::App::transfer_tab`] primitive — given a `(src_window,
//! src_tab_idx, dst_window, dst_tab_idx)` 4-tuple, move a tab. Phase
//! C1 added the cross-process wire format. **Phase C2 (this file)**
//! wires up the actual NSDraggingSession / OLE-DoDragDrop calls so
//! that a real user mouse drag ends up calling
//! [`crate::app::App::transfer_tab`].
//!
//! ## Why a trait
//!
//! NSDraggingSession lives in `sonic-mac`; OLE DoDragDrop lives in
//! `sonic-windows`. The `sonic-app` crate is platform-agnostic and
//! cannot link AppKit / Win32 directly without breaking the
//! cross-platform build. The trait is the seam:
//!
//! ```text
//!  sonic-app (this crate)
//!    ├─ defines OsTabDragBackend trait
//!    └─ App owns Option<Box<dyn OsTabDragBackend>>
//!
//!  sonic-mac
//!    └─ MacOsTabDragBackend: OsTabDragBackend  ← begins NSDragSession
//!
//!  sonic-windows
//!    └─ WinOsTabDragBackend: OsTabDragBackend  ← begins OLE DoDragDrop
//! ```
//!
//! ## Callback flow
//!
//! NSDraggingSource / IDropSource callbacks fire on a thread that is
//! not winit's main loop (AppKit posts to the main RunLoop; OLE
//! pumps a private message loop). The backend therefore cannot poke
//! `App` directly — it must hop through the winit
//! [`winit::event_loop::EventLoopProxy`] to wake the main loop and
//! deliver a `UserEvent::DragMoved` / `UserEvent::DragEnded`. The
//! [`AppHandle`] shim wraps that proxy + the bookkeeping the backend
//! needs to identify *which* session is ending (source window, source
//! tab index, payload).
//!
//! ## What this does NOT do
//!
//! * It does NOT replace [`crate::tab_drag`]'s pure within-bar drag
//!   geometry — that still handles "drag tab to slot 3 of the same
//!   bar" reorders. This file only kicks in when the cursor leaves
//!   the source window's tab bar, at which point we need OS cursor
//!   capture to keep receiving events.
//! * It does NOT touch the cross-process wire format in
//!   [`crate::os_drag`]. Same-process drag uses the in-memory
//!   `(src_window, src_idx, dst_window, dst_idx)` tuple; cross-process
//!   drag still flows through `TabPayload` + `OsDragSink::begin_drag`.

use std::sync::{Arc, Mutex};

use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

/// Re-export of [`winit::window::WindowId`] so platform backend crates
/// (`sonic-mac`, `sonic-windows`) that already depend on `sonic-app`
/// don't have to add a direct `winit` dep just to spell the trait
/// signature. Keeps the dependency surface minimal.
pub use winit::window::WindowId as BackendWindowId;

use super::UserEvent;

/// Mouse-down → drag-start hysteresis, in logical pixels. Identical
/// to [`crate::tab_drag::DRAG_START_THRESHOLD_PX`] — duplicated here
/// only because the OS-drag trigger path doesn't want a cyclic dep
/// on the pure tab_drag module just for one constant.
///
/// Below this floor a mouse-down + mouse-up is a click, not a drag.
/// The threshold matches Cocoa's `kDragViewMovementThreshold` and
/// GTK's default — anything smaller flickers the OS drag chrome on
/// every accidental jitter.
pub const OS_DRAG_THRESHOLD_PX: f32 = 5.0;

/// What a real OS-level drag did when the user released the button.
///
/// Returned from the backend to the app via [`UserEvent::DragEnded`]
/// so the dispatcher can decide between [`crate::app::App::transfer_tab`]
/// and [`crate::app::App::cancel_drag_session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragOutcome {
    /// User let go over a Sonic window's tab bar — perform a transfer.
    /// `target_window == None` means the App's main window; `Some(id)`
    /// means a torn-out child window. `target_slot` is the insertion
    /// index in the destination bar (`[0, len]`). This is the "real"
    /// drop-on-bar outcome the C2 spec asks for — the backend MUST hit
    /// test the destination bar and post the resolved slot rather than
    /// a placeholder zero.
    DroppedOnBar { target_window: Option<WindowId>, target_slot: usize },
    /// User let go over empty space (no Sonic tab bar under the
    /// cursor) — Phase C semantics: tear out to a new floating child
    /// window. The backend includes the screen-global drop position so
    /// the App can place the new window's origin sensibly.
    DroppedOnEmpty { drop_screen_pos: (i32, i32) },
    /// User cancelled (Esc pressed, drag rejected, source window
    /// closed mid-drag, etc.). No state change — the source tab stays
    /// where it was.
    Cancelled,
}

/// The trait every platform OS-drag backend implements.
///
/// Single method — the rest of the dance (cursor capture, hit-testing
/// the pasteboard format, callback dispatch) lives inside the
/// backend's platform-specific impl. The backend takes ownership of
/// the gesture once `begin_session` returns: from that moment until
/// it posts [`UserEvent::DragEnded`] via the [`AppHandle`], the App
/// should treat the source tab as "live but in flight" — render the
/// drag-chip overlay, suppress other tab interactions, etc.
///
/// **Threading:** `begin_session` is called from the winit main
/// thread. Platform backends may spin up worker threads internally
/// (OLE does), but every interaction with [`AppHandle`] uses the
/// thread-safe [`EventLoopProxy`] it wraps.
pub trait OsTabDragBackend: Send {
    /// Start an OS-level drag session. The backend is now responsible
    /// for cursor capture and for posting `UserEvent::DragMoved` /
    /// `UserEvent::DragEnded` back through the handle.
    ///
    /// `payload_json` is the full [`crate::os_drag::TabPayload`]
    /// serialized to JSON, ready to be written to the platform
    /// pasteboard / OLE clipboard under
    /// [`crate::os_drag::PASTEBOARD_TYPE`] /
    /// `CF_SONIC_TAB`. Backends MUST write the full schema so peer
    /// Sonic windows / processes can parse it via
    /// [`crate::os_drag::TabPayload::from_json`].
    ///
    /// `drag_image_png` is an optional rasterized preview of the
    /// dragged tab. Backends that can render their own preview (e.g.
    /// via NSDraggingItem's `setImageComponentsProvider:`) may ignore
    /// it; backends without that capability use it directly.
    fn begin_session(
        &mut self,
        handle: AppHandle,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    );

    /// Returns `true` if this backend OWNS the gesture end-to-end —
    /// the caller MUST skip the legacy cross-process
    /// [`crate::os_drag::OsDragSink::begin_drag`] path because invoking
    /// it would double-fire (e.g. on Windows where both call
    /// `DoDragDrop`).
    ///
    /// Default `false` keeps the legacy sink as a fallback. The
    /// Windows backend overrides to `true` because its `begin_session`
    /// invokes `DoDragDrop` synchronously. The macOS backend keeps
    /// `false` — its `begin_session` only writes the pasteboard
    /// (NSDraggingSession proper is constrained by winit's mouse
    /// interception, see `sonic-mac/src/tab_drag_os.rs`), so the
    /// legacy sink path remains a valid mirror.
    fn handles_full_gesture(&self) -> bool {
        false
    }
}

/// Thin shim that lets a backend running off the winit thread post
/// events back into the App's event loop.
///
/// Wraps the winit [`EventLoopProxy`] plus a one-slot mailbox for the
/// pending [`DragOutcome`] — the proxy itself only carries a unit-y
/// `UserEvent` wake signal; richer data has to ride a side channel.
/// Pattern matches `crate::os_drag::PendingPayloadSlot`.
#[derive(Clone)]
pub struct AppHandle {
    proxy: EventLoopProxy<UserEvent>,
    pending: Arc<PendingDragOutcome>,
}

impl AppHandle {
    /// Wrap an existing [`EventLoopProxy`] + freshly-allocated mailbox.
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self { proxy, pending: Arc::new(PendingDragOutcome::default()) }
    }

    /// Reuse an existing mailbox — used by the App-side dispatcher so
    /// the same `Arc<PendingDragOutcome>` is shared between the
    /// backend's [`AppHandle`] clone and the App's own drain path.
    pub fn with_pending(
        proxy: EventLoopProxy<UserEvent>,
        pending: Arc<PendingDragOutcome>,
    ) -> Self {
        Self { proxy, pending }
    }

    /// Backend-side: cursor moved during a live drag. Posts a
    /// [`UserEvent::DragMoved`] wake; the App's `do_user_event` reads
    /// the latest position from the mailbox. Old positions are
    /// overwritten — only the most-recent matters.
    pub fn post_drag_moved(&self, screen_pos: (i32, i32)) {
        self.pending.set_moved(screen_pos);
        // send_event returns Err only when the event loop is gone; in
        // that case a wake is meaningless anyway, so swallow silently.
        let _ = self.proxy.send_event(UserEvent::DragMoved);
    }

    /// Backend-side: drag finished. Posts a [`UserEvent::DragEnded`]
    /// and parks the outcome in the mailbox.
    pub fn post_drag_ended(&self, outcome: DragOutcome) {
        self.pending.set_ended(outcome);
        let _ = self.proxy.send_event(UserEvent::DragEnded);
    }

    /// App-side: clone of the shared mailbox so the dispatcher in
    /// `event_loop.rs` can drain pending outcomes on each
    /// `UserEvent::DragMoved` / `DragEnded` wake.
    pub fn pending_handle(&self) -> Arc<PendingDragOutcome> {
        self.pending.clone()
    }
}

/// One-slot mailbox shared between an [`AppHandle`] (backend writer)
/// and the App's user-event dispatcher (reader).
///
/// Two slots: the latest cursor position (overwritten each
/// `post_drag_moved`) and the terminal outcome (set once on
/// `post_drag_ended`). The dispatcher drains both; the App's main
/// loop is responsible for actioning whatever it drains.
#[derive(Debug, Default)]
pub struct PendingDragOutcome {
    moved: Mutex<Option<(i32, i32)>>,
    ended: Mutex<Option<DragOutcome>>,
}

impl PendingDragOutcome {
    /// Public so tests can populate the mailbox without needing to
    /// construct a real [`EventLoopProxy`] (which requires a live
    /// display on most platforms). In production this is only called
    /// through [`AppHandle::post_drag_moved`] / [`AppHandle::post_drag_ended`].
    pub fn set_moved(&self, pos: (i32, i32)) {
        let mut g = self.moved.lock().unwrap_or_else(|p| p.into_inner());
        *g = Some(pos);
    }
    /// Public for the same reason as [`Self::set_moved`].
    pub fn set_ended(&self, outcome: DragOutcome) {
        let mut g = self.ended.lock().unwrap_or_else(|p| p.into_inner());
        *g = Some(outcome);
    }
    /// Drain the latest cursor position (if any).
    pub fn take_moved(&self) -> Option<(i32, i32)> {
        self.moved.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
    /// Drain the terminal outcome (if any).
    pub fn take_ended(&self) -> Option<DragOutcome> {
        self.ended.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
    /// Non-destructive peek: returns whether the ended slot is
    /// currently populated, without draining it. Used by the Windows
    /// backend to detect whether the IDropTarget::Drop callback
    /// already posted a richer outcome (target_window + target_slot
    /// from cursor hit-test) so it doesn't overwrite that with a
    /// less-specific DROPEFFECT-derived outcome.
    pub fn peek_ended(&self) -> Option<DragOutcome> {
        *self.ended.lock().unwrap_or_else(|p| p.into_inner())
    }
}

// Unit tests live alongside the integration tests in
// `crates/sonic-app/tests/os_drag_dispatch_flow.rs` — see that file
// for the mock-backend driven flow assertions covering
// `begin_session` invocation, threshold gating, and the
// DragOutcome → transfer_tab / cancel_drag_session dispatch.
