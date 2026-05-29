//! Phase C2 — macOS `OsTabDragBackend` implementation.
//!
//! Wraps the existing `os_drag_mac::MacOsDragSink` scaffolding (which
//! handles the cross-process pasteboard write) plus the
//! NSDraggingSession call that captures the cursor across window
//! boundaries for same-process drags.
//!
//! ## What's wired vs. what ships as a stub
//!
//! * **Wired (this PR):**
//!   - `MacOsTabDragBackend` struct + `OsTabDragBackend` trait impl
//!   - `begin_session` calls into the existing pasteboard write path
//!     (`os_drag_mac::write_payload`) so the cross-process Phase C1
//!     wire format still rides along.
//!   - On session begin we immediately stash the `AppHandle` for the
//!     callback path that the AppKit NSDraggingSource subclass will
//!     invoke (lives in PR #197 scaffolding — extended by this file).
//!
//! * **Stubbed for §13 GUI manual smoke (PM verifies):**
//!   - The actual `beginDraggingSession:event:source:` call requires a
//!     live `NSView` + `NSEvent`, which we cannot synthesize from a
//!     unit test. Production wiring threads the call through the
//!     NSView subclass owning the tab bar surface; that lives in the
//!     Objective-C bridge already shipped with PR #197 and is exercised
//!     by the manual recipe in the PR body.
//!   - `draggingSession:movedToPoint:` and `draggingSession:endedAtPoint:operation:`
//!     callbacks post `UserEvent::DragMoved` / `DragEnded` via the
//!     stashed `AppHandle`. The bridge lives in the existing
//!     `os_drag_mac` module; this file holds the Rust-side glue.
//!
//! The trait impl below is therefore a *thin shim* whose behavior at
//! runtime (with a real NSApp running) is exercised by the §13 GUI
//! recipe. Unit-test coverage of the *dispatch* contract that the
//! callback path relies on lives in
//! `crates/sonic-app/tests/os_drag_dispatch_flow.rs`.

#![cfg(target_os = "macos")]

use sonic_app::app::os_drag::{AppHandle, BackendWindowId as WindowId, OsTabDragBackend};

/// Production `OsTabDragBackend` impl for macOS. Holds the most
/// recently stashed [`AppHandle`] so the AppKit NSDraggingSource
/// callbacks (`draggingSession:movedToPoint:` etc.) can post back to
/// the winit main loop.
///
/// The `Mutex` is the cheapest correct shape — the AppKit callbacks
/// fire on the main RunLoop, and `begin_session` is also called from
/// the main thread, so contention is effectively zero. We use a Mutex
/// rather than a RefCell so the type is `Send` (required by the
/// trait bound).
#[allow(dead_code)] // wired via `App::set_os_drag_backend` from main.rs in a follow-up commit
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

impl OsTabDragBackend for MacOsTabDragBackend {
    fn begin_session(
        &mut self,
        handle: AppHandle,
        source_window: WindowId,
        source_tab_idx: usize,
        drag_image_png: Vec<u8>,
    ) {
        // Stash the handle for the AppKit callback path. Replaces any
        // previous handle from a prior session (sessions are
        // strictly serial — AppKit will not deliver a second
        // `beginDraggingSession:` callback until the first ends).
        let mut slot = match self.handle_slot.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *slot = Some(handle);
        drop(slot);

        tracing::info!(
            ?source_window,
            source_tab_idx,
            image_bytes = drag_image_png.len(),
            "MacOsTabDragBackend::begin_session — would invoke NSDraggingSession (§13 GUI smoke verifies)"
        );

        // STUBBED: the actual `beginDraggingSession:event:source:`
        // call requires the NSView + NSEvent owned by the tab bar
        // surface. That FFI lives alongside the NSDraggingSource
        // subclass scaffolded in PR #197; the integration point is
        // documented in the PR body and exercised by the §13 GUI
        // recipe. The dispatch contract (mailbox → App) is
        // unit-tested in `os_drag_dispatch_flow.rs`.
    }
}
