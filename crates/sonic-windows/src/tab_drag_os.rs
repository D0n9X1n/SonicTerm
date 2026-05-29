//! Phase C2 — Windows `OsTabDragBackend` implementation.
//!
//! Wraps the existing `os_drag_win` scaffolding (which handles the
//! cross-process OLE clipboard write) plus the OLE `DoDragDrop` call
//! that captures the cursor across HWND boundaries for same-process
//! drags.
//!
//! ## What's wired vs. what ships as a stub
//!
//! * **Wired (this PR):**
//!   - `WinOsTabDragBackend` struct + `OsTabDragBackend` trait impl
//!   - On session begin we immediately stash the `AppHandle` for the
//!     callback path that `IDropSource::QueryContinueDrag` /
//!     `IDropTarget::Drop` will invoke.
//!
//! * **Stubbed for §13 GUI manual smoke (PM verifies):**
//!   - The actual `DoDragDrop` call spins a private OLE message
//!     pump for the duration of the drag, which is incompatible with
//!     winit's event loop unless invoked synchronously from the
//!     window's WndProc. Production wiring threads the call through
//!     the `os_drag_win` IDataObject scaffolding; the integration
//!     point is documented in the PR body and exercised by the §13
//!     GUI recipe.
//!   - `IDropTarget::DragOver` returns `DROPEFFECT_MOVE` and
//!     `IDropTarget::Drop` reads the payload, posting
//!     `UserEvent::DragEnded` via the stashed `AppHandle` (winit's
//!     `EventLoopProxy` is thread-safe so this is safe from the OLE
//!     worker context).
//!
//! Unit-test coverage of the *dispatch* contract that the callback
//! path relies on lives in
//! `crates/sonic-app/tests/os_drag_dispatch_flow.rs` — identical to
//! the macOS path.

#![cfg(target_os = "windows")]

use sonic_app::app::os_drag::{AppHandle, BackendWindowId as WindowId, OsTabDragBackend};

/// Production `OsTabDragBackend` impl for Windows. Holds the most
/// recently stashed [`AppHandle`] so the OLE `IDropSource` /
/// `IDropTarget` callbacks can post back to the winit main loop via
/// the wrapped `EventLoopProxy`.
#[allow(dead_code)] // wired via `App::set_os_drag_backend` from main.rs in a follow-up commit
pub struct WinOsTabDragBackend {
    handle_slot: std::sync::Mutex<Option<AppHandle>>,
}

#[allow(dead_code)]
impl WinOsTabDragBackend {
    /// Construct a backend with an empty handle slot.
    pub fn new() -> Self {
        Self { handle_slot: std::sync::Mutex::new(None) }
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

impl OsTabDragBackend for WinOsTabDragBackend {
    fn begin_session(
        &mut self,
        handle: AppHandle,
        source_window: WindowId,
        source_tab_idx: usize,
        drag_image_png: Vec<u8>,
    ) {
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
            "WinOsTabDragBackend::begin_session — would invoke DoDragDrop (§13 GUI smoke verifies)"
        );

        // STUBBED: actual DoDragDrop call lives alongside the
        // IDataObject scaffolding in `os_drag_win.rs`. The dispatch
        // contract (mailbox → App) is unit-tested in
        // `os_drag_dispatch_flow.rs`.
    }
}
