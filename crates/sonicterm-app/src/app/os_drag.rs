//! OS-level drag *session* hookup.
//!
//! This module is distinct from the *cross-process* drag wire format
//! at [`crate::os_drag`]:
//!
//! * [`crate::os_drag`] (top-level) defines the **wire payload**
//!   ([`crate::os_drag::TabPayload`], [`crate::os_drag::PASTEBOARD_TYPE`])
//!   carried between two SonicTerm *processes* via NSPasteboard / OLE
//!   clipboard. That part already shipped.
//!
//! * **This module** ([`crate::app::os_drag`]) defines the
//!   [`OsTabDragBackend`] trait for platform handoff. Windows uses it for a
//!   native OLE `DoDragDrop` session with cursor capture. The current macOS
//!   implementation publishes to NSPasteboard and immediately cancels the
//!   backend gesture; same-process merging stays in the in-process path.
//!
//! [`crate::app::tab_transfer`] added the pure
//! [`crate::app::App::transfer_tab`] primitive — given a `(src_window,
//! src_tab_idx, dst_window, dst_tab_idx)` 4-tuple, move a tab. Phase
//! C1 added the cross-process wire format. This file provides the shared
//! outcome/registry contract used by the full Windows OLE backend and the
//! pasteboard-only macOS backend.
//!
//! ## Why a trait
//!
//! NSPasteboard/AppKit integration lives in `sonicterm-mac`; OLE `DoDragDrop`
//! lives in `sonicterm-windows`. The `sonicterm-app` crate is platform-agnostic and
//! cannot link AppKit / Win32 directly without breaking the
//! cross-platform build. The trait is the seam:
//!
//! ```text
//!  sonicterm-app (this crate)
//!    ├─ defines OsTabDragBackend trait
//!    └─ App owns Option<Box<dyn OsTabDragBackend>>
//!
//!  sonicterm-mac
//!    └─ MacOsTabDragBackend: OsTabDragBackend  ← publishes NSPasteboard payload
//!
//!  sonicterm-windows
//!    └─ WinOsTabDragBackend: OsTabDragBackend  ← begins OLE DoDragDrop
//! ```
//!
//! ## Callback flow
//!
//! Native backend callbacks cannot borrow `App` directly while
//! `event_loop.run_app(&mut app)` owns it. A backend therefore cannot poke
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
//!   bar" reorders. This file only kicks in when the cursor leaves the source
//!   window's tab bar. Windows then uses native cursor capture; macOS currently
//!   publishes a pasteboard payload and cancels the backend gesture.
//! * It does NOT touch the cross-process wire format in
//!   [`crate::os_drag`]. Same-process drag uses the in-memory
//!   `(src_window, src_idx, dst_window, dst_idx)` tuple; cross-process
//!   drag still flows through `TabPayload` + `OsDragSink::begin_drag`.

use std::sync::{Arc, Mutex};

use winit::event_loop::EventLoopProxy;
use winit::window::{Window, WindowAttributes, WindowId};

/// Re-export of [`winit::window::Window`] for the same reason as
/// [`BackendWindowId`] — platform backend crates need to spell the
/// `register_window` trait signature without taking a direct winit
/// dep just for the type name.
pub use winit::window::Window as BackendWindow;
/// Re-export of [`winit::window::WindowId`] so platform backend crates
/// (`sonicterm-mac`, `sonicterm-windows`) that already depend on `sonicterm-app`
/// don't have to add a direct `winit` dep just to spell the trait
/// signature. Keeps the dependency surface minimal.
pub use winit::window::WindowId as BackendWindowId;

use super::{os_drag, window_event, App, PendingTearOut, UserEvent};

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
    /// User let go over a SonicTerm window's tab bar — perform a transfer.
    /// `target_window == None` means the App's main window; `Some(id)`
    /// means a torn-out child window. `target_slot` is the insertion
    /// index in the destination bar (`[0, len]`). This is the "real"
    /// drop-on-bar outcome the C2 spec asks for — the backend MUST hit
    /// test the destination bar and post the resolved slot rather than
    /// a placeholder zero.
    DroppedOnBar { target_window: Option<WindowId>, target_slot: usize },
    /// User let go over empty space (no SonicTerm tab bar under the
    /// cursor) — tear out to a new floating child
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
/// Single method — payload publication, optional cursor capture/hit-testing,
/// and callback dispatch live inside the platform implementation. The source
/// tab remains live while the backend reports its outcome. Windows owns the
/// native gesture end-to-end; macOS currently publishes to NSPasteboard and
/// immediately posts a cancelled outcome.
///
/// **Threading:** `begin_session` is called from the winit main
/// thread. Platform backends may spin up worker threads internally
/// (OLE does), but every interaction with [`AppHandle`] uses the
/// thread-safe [`EventLoopProxy`] it wraps.
pub trait OsTabDragBackend: Send {
    /// Start the platform handoff. A full backend owns cursor capture and posts
    /// move/end events; a publication-only backend may post a terminal cancelled
    /// outcome immediately after making the payload available.
    ///
    /// `payload_json` is the full [`crate::os_drag::TabPayload`]
    /// serialized to JSON, ready to be written to the platform
    /// pasteboard / OLE clipboard under
    /// [`crate::os_drag::PASTEBOARD_TYPE`] /
    /// `CF_SONIC_TAB`. Backends MUST write the full schema so peer
    /// SonicTerm windows / processes can parse it via
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
    /// interception, see `sonicterm-mac/src/tab_drag_os.rs`), so the
    /// legacy sink path remains a valid mirror.
    fn handles_full_gesture(&self) -> bool {
        false
    }

    /// Select exclusive native drop-target ownership before any window is created.
    fn owns_native_drop_target(&self) -> bool {
        false
    }

    /// Register a winit window with the backend so OS-level drag drops
    /// targeting that window are routed back into the App. On Windows
    /// this MUST call `RegisterDragDrop` against the HWND extracted
    /// from `window`'s raw handle; without this, drops landing on
    /// torn-out child windows are silently dropped by the OS (drops
    /// never reach `IDropTarget::Drop`). On macOS this is a no-op —
    /// AppKit's pasteboard-publish model does not need per-window
    /// IDropTarget registration.
    ///
    /// Called once per window: by `App::resumed` for the main window
    /// and by `App::tear_out_tab` / `App::tear_out_from_child` for each
    /// torn-out child window. Default impl is a no-op so mock backends
    /// in tests can opt in / out trivially.
    fn register_window(
        &mut self,
        _handle: AppHandle,
        _window_id: WindowId,
        _window: &Arc<Window>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Release any platform registration associated with a closing window.
    ///
    /// Windows uses this to pair `RegisterDragDrop` with `RevokeDragDrop`
    /// before the HWND is destroyed. Backends without per-window state keep
    /// the default no-op.
    fn unregister_window(&mut self, _window_id: WindowId) -> Result<(), String> {
        Ok(())
    }
}

/// Snapshot of a single window's tab bar, in **screen** coordinates,
/// published by the App into a [`TabBarRegistry`] each frame so a
/// platform OS-drag backend running outside the app borrow (currently the
/// Windows OLE `IDropTarget::Drop` path) can hit-test the drop cursor without
/// calling back into `App` state. The macOS pasteboard-only backend does not
/// consume this registry today.
///
/// Visible tabs carry screen extents and absolute indices; slot resolution mirrors the live layout.
#[derive(Debug, Clone)]
pub struct TabBarSnapshot {
    /// Identifies the destination window. `None` means "the App's main
    /// window" (mirrors the convention in [`DragOutcome::DroppedOnBar`]).
    pub window: Option<WindowId>,
    /// Window's outer rect in **screen** coordinates (origin top-left,
    /// y-down — same convention as Win32 `GetWindowRect` and macOS
    /// `screen.frame()` after CG-flip). The dispatcher hit-tests the
    /// drop point against this first to pick a window.
    pub window_rect: (i32, i32, i32, i32),
    /// Tab bar's rect in **screen** coordinates. A drop inside
    /// `window_rect` but outside `bar_rect` resolves to "in window but
    /// not on bar" — see [`TabBarRegistry::resolve_screen_pos`].
    pub bar_rect: (i32, i32, i32, i32),
    /// Integer screen thresholds matching the live layout's fractional midpoint comparisons.
    pub tab_midpoints: Vec<i32>,
    /// Absolute indices of the visible tabs, not their positions in this snapshot.
    pub tab_indices: Vec<usize>,
    /// Total tabs used by overflow's append-to-end drop target.
    pub total_tabs: usize,
    /// Screen X where the overflow control begins, if present.
    pub overflow_append_from: Option<i32>,
}

impl TabBarSnapshot {
    /// Returns `true` iff the screen point `(sx, sy)` is inside this
    /// window's outer rect (inclusive of left/top, exclusive of
    /// right/bottom — matches Win32 `RECT` semantics).
    pub fn window_contains(&self, sx: i32, sy: i32) -> bool {
        let (l, t, r, b) = self.window_rect;
        sx >= l && sx < r && sy >= t && sy < b
    }

    /// Returns `true` iff the screen point `(sx, sy)` is inside this
    /// window's tab bar rect.
    pub fn bar_contains(&self, sx: i32, sy: i32) -> bool {
        let (l, t, r, b) = self.bar_rect;
        sx >= l && sx < r && sy >= t && sy < b
    }

    /// Build a [`TabBarSnapshot`] from a computed `TabBarLayout` in
    /// window-local raster px plus the destination window's raster-px
    /// inner origin. All output rects are in screen-global raster px —
    /// the same coordinate system the OS reports drop cursors in.
    ///
    /// `inner_origin` is the window's `inner_position()` (top-left of
    /// the client area, screen-global raster px). `inner_size` is
    /// `inner_size()` in raster px.
    ///
    /// Used by the App's per-frame redraw path to publish the live tab
    /// bar geometry into the shared [`TabBarRegistry`] so platform OS-drag
    /// backends can hit-test drop cursors without re-entering the App.
    pub fn from_layout(
        window: Option<WindowId>,
        inner_origin: (i32, i32),
        inner_size: (u32, u32),
        layout: &sonicterm_ui::tabbar_view::TabBarLayout,
    ) -> Self {
        let (ox, oy) = inner_origin;
        let (iw, ih) = inner_size;
        let window_rect = (ox, oy, ox + iw as i32, oy + ih as i32);
        let bar = &layout.bar;
        let bar_rect = (
            ox + bar.x.round() as i32,
            oy + bar.y.round() as i32,
            ox + (bar.x + bar.w).round() as i32,
            oy + (bar.y + bar.h).round() as i32,
        );
        let mut tab_indices = Vec::with_capacity(layout.tabs.len());
        let mut tab_midpoints = Vec::with_capacity(layout.tabs.len());
        for t in &layout.tabs {
            tab_indices.push(t.idx);
            tab_midpoints.push(ox + (t.bg_rect.x + t.bg_rect.w * 0.5).ceil() as i32);
        }
        Self {
            window,
            window_rect,
            bar_rect,
            tab_midpoints,
            tab_indices,
            total_tabs: layout.total_tabs,
            overflow_append_from: layout.overflow.map(|control| ox + control.x.ceil() as i32),
        }
    }

    /// Resolve the same absolute visible-gap or overflow-append slot as TabBarLayout in screen coordinates.
    pub fn drop_slot(&self, sx: i32) -> usize {
        if self.overflow_append_from.is_some_and(|start| sx >= start) {
            // When: `sx` reaches overflow, preserve append-to-end independently of the visible segment.
            return self.total_tabs;
        }
        debug_assert_eq!(self.tab_midpoints.len(), self.tab_indices.len());
        for (&midpoint, &index) in self.tab_midpoints.iter().zip(&self.tab_indices) {
            if sx < midpoint {
                // When: `sx` precedes a visible midpoint, insert before its absolute `index`.
                return index;
            }
        }
        self.tab_indices.last().map_or(0, |index| (index + 1).min(self.total_tabs))
    }
}

/// Registry of currently-published [`TabBarSnapshot`]s, one per live
/// SonicTerm window in this process. The App publishes into it on every
/// resize / tab add-or-remove / window-move; the platform OS-drag
/// backend reads from it inside its drop callback to translate a
/// raw screen-coordinate drop into a `(WindowId, slot)` pair.
///
/// Thread-safe via an internal `Mutex<Vec<_>>`. The expected access
/// pattern is "many publishes (winit thread) / occasional reads (OLE
/// worker thread on drop)", so contention is a non-issue.
#[derive(Debug, Default)]
pub struct TabBarRegistry {
    snapshots: Mutex<Vec<TabBarSnapshot>>,
}

impl TabBarRegistry {
    /// Construct an empty registry. The App owns one and shares
    /// `Arc<TabBarRegistry>` clones with backends through
    /// [`AppHandle::tab_bar_registry`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace any existing snapshot for the same `window` (matched by
    /// `WindowId` equality / `None == None`) and append the new one.
    /// Called by the App each frame.
    pub fn publish(&self, snapshot: TabBarSnapshot) {
        let mut g = self.snapshots.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|s| s.window != snapshot.window);
        g.push(snapshot);
    }

    /// Remove the snapshot for `window` if any. Called when a window
    /// closes so the registry doesn't keep a stale rect that would
    /// false-positive a hit-test on later drops.
    pub fn remove(&self, window: Option<WindowId>) {
        let mut g = self.snapshots.lock().unwrap_or_else(|p| p.into_inner());
        g.retain(|s| s.window != window);
    }

    /// Translate a screen-coordinate drop into a `(window, slot)` pair.
    /// Returns:
    ///   * `Some((window, slot))` if `(sx, sy)` falls inside any
    ///     window's tab bar — `slot` is an absolute visible-gap index or overflow's global append slot.
    ///   * `None` if no window contains the point, OR a window contains
    ///     the point but the point isn't on its bar — in the latter case
    ///     the caller (Windows IDropTarget::Drop) treats it as
    ///     `DroppedOnEmpty` so the source tab tears out at the drop
    ///     point. Distinguishing those is the caller's job (it knows
    ///     `(sx, sy)` and can re-run `window_contains` on each
    ///     snapshot).
    pub fn resolve_screen_pos(&self, sx: i32, sy: i32) -> Option<(Option<WindowId>, usize)> {
        let g = self.snapshots.lock().unwrap_or_else(|p| p.into_inner());
        for snap in g.iter() {
            if snap.bar_contains(sx, sy) {
                // When: `snap.bar_contains(sx, sy)` is true, return that window's resolved destination slot.
                return Some((snap.window, snap.drop_slot(sx)));
            }
        }
        None
    }

    /// Resolve a slot only within the named window, even when native windows overlap.
    pub fn resolve_window_screen_pos(
        &self,
        window: Option<WindowId>,
        sx: i32,
        sy: i32,
    ) -> Option<usize> {
        let snapshots = self.snapshots.lock().unwrap_or_else(|p| p.into_inner());
        snapshots
            .iter()
            .find(|snapshot| snapshot.window == window && snapshot.bar_contains(sx, sy))
            .map(|snapshot| snapshot.drop_slot(sx))
    }

    /// Returns `true` iff any registered window's outer rect (not bar)
    /// contains `(sx, sy)`. Used by the Windows IDropTarget::Drop
    /// fallback to decide whether to treat "in window but not on bar"
    /// as `DroppedOnEmpty` (tear out at drop point) vs an unknown drop.
    pub fn any_window_contains(&self, sx: i32, sy: i32) -> bool {
        let g = self.snapshots.lock().unwrap_or_else(|p| p.into_inner());
        g.iter().any(|s| s.window_contains(sx, sy))
    }

    /// Number of currently-published snapshots. For tests / diagnostics.
    pub fn len(&self) -> usize {
        self.snapshots.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// `true` iff no snapshots are published.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
    bars: Arc<TabBarRegistry>,
    main_window_id: Option<WindowId>,
}

impl AppHandle {
    /// Wrap an existing [`EventLoopProxy`] + freshly-allocated mailbox.
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            pending: Arc::new(PendingDragOutcome::default()),
            bars: Arc::new(TabBarRegistry::default()),
            main_window_id: None,
        }
    }

    /// Reuse an existing mailbox — used by the App-side dispatcher so
    /// the same `Arc<PendingDragOutcome>` is shared between the
    /// backend's [`AppHandle`] clone and the App's own drain path.
    pub fn with_pending(
        proxy: EventLoopProxy<UserEvent>,
        pending: Arc<PendingDragOutcome>,
    ) -> Self {
        Self { proxy, pending, bars: Arc::new(TabBarRegistry::default()), main_window_id: None }
    }

    /// Reuse both the mailbox and a shared [`TabBarRegistry`]. The App
    /// uses this so its own publishing path and the backend's reading
    /// path see the same registry instance.
    pub fn with_pending_and_bars(
        proxy: EventLoopProxy<UserEvent>,
        pending: Arc<PendingDragOutcome>,
        bars: Arc<TabBarRegistry>,
    ) -> Self {
        Self { proxy, pending, bars, main_window_id: None }
    }

    /// Capture the main-window identity used by native drop-target hit testing.
    pub fn with_main_window(mut self, main_window_id: Option<WindowId>) -> Self {
        self.main_window_id = main_window_id;
        self
    }

    /// Resolve the registry's main-window marker without consulting current focus.
    pub fn main_window_id(&self) -> Option<WindowId> {
        self.main_window_id
    }

    /// Hand out an `Arc` clone of the shared [`TabBarRegistry`] so the
    /// backend can stash it for use inside its drop callback.
    pub fn tab_bar_registry(&self) -> Arc<TabBarRegistry> {
        self.bars.clone()
    }

    /// Convenience: same hit-test as
    /// [`TabBarRegistry::resolve_screen_pos`] against the shared
    /// registry. Backends typically call this from their drop
    /// callback.
    pub fn query_tab_bar_slot(&self, sx: i32, sy: i32) -> Option<(Option<WindowId>, usize)> {
        self.bars.resolve_screen_pos(sx, sy)
    }

    /// Resolve the receiving native window's tab slot without borrowing another overlapping window.
    pub fn query_window_tab_bar_slot(
        &self,
        window_id: WindowId,
        sx: i32,
        sy: i32,
    ) -> Option<usize> {
        let target = (Some(window_id) != self.main_window_id).then_some(window_id);
        self.bars.resolve_window_screen_pos(target, sx, sy)
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

impl App {
    /// Install the platform OS handoff backend. `sonicterm-mac` supplies its
    /// pasteboard publisher; `sonicterm-windows` supplies OLE `DoDragDrop`.
    /// Tests use it via
    /// [`Self::__test_set_os_drag_backend`] to inject a mock.
    #[doc(hidden)]
    pub fn set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// build an [`os_drag::AppHandle`] tied to the App's
    /// event-loop proxy and the shared pending-outcome mailbox. The
    /// returned handle is what gets passed to
    /// [`os_drag::OsTabDragBackend::begin_session`] so the backend can
    /// post `DragMoved` / `DragEnded` events back to the main loop.
    ///
    /// Returns `None` when no event-loop proxy has been wired. In that
    /// case the OS drag is not startable, which the caller treats as
    /// "fall back to the existing within-process tear_out path".
    pub fn os_drag_app_handle(&self) -> Option<os_drag::AppHandle> {
        self.event_loop_proxy.clone().map(|p| {
            os_drag::AppHandle::with_pending_and_bars(
                p,
                self.os_drag_pending.clone(),
                self.os_drag_bars.clone(),
            )
            .with_main_window(self.main_window_id)
        })
    }

    /// Hand out an `Arc` clone of the shared [`os_drag::TabBarRegistry`].
    /// Platform glue (e.g. `sonicterm-windows::os_drag_win`) calls this to
    /// stash a reference for use inside the OLE IDropTarget::Drop
    /// callback, where the AppHandle isn't always available.
    pub fn os_drag_bar_registry(&self) -> Arc<os_drag::TabBarRegistry> {
        self.os_drag_bars.clone()
    }

    /// Publish the current tab bar layout for `window` into the shared
    /// registry. Called from the App's per-frame render path with
    /// already-resolved screen coordinates (caller is responsible for
    /// converting logical-px / window-local to screen via
    /// winit's `Window::outer_position`).
    pub fn publish_os_drag_bar_snapshot(&self, snapshot: os_drag::TabBarSnapshot) {
        self.os_drag_bars.publish(snapshot);
    }

    /// Convenience: build a [`os_drag::TabBarSnapshot`] from the main
    /// window's current geometry + tab bar and publish it. No-op if the
    /// main window or renderer aren't yet initialized (pre-`resumed`).
    /// Called from the per-frame `RedrawRequested` handler so the
    /// snapshot registry tracks every visible tab-bar state change.
    pub(super) fn publish_main_window_tab_bar(&self) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(w) = self.main_window() else {
            // When: `main_window` does not exist yet, so there is no surface whose
            // tab-bar geometry could be published.
            return;
        };
        let Some(r) = self.main_renderer() else {
            // When: `main_renderer` is absent, so tab-bar height and insets cannot
            // be measured and any published rect would be invented.
            return;
        };
        let inner_origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = w.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let empty_tabs_pub = sonicterm_ui::tabs::TabBar::new();
        let layout = TabBarLayout::compute_with_height(
            self.main_tabs().unwrap_or(&empty_tabs_pub),
            raster_w,
            r.tab_bar_logical_height(),
        )
        .with_top_offset(r.tab_bar_y_offset())
        .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(w.id()), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    /// Remove a window's snapshot from the registry (called on window
    /// close). Safe to call with `None` (matches main-window convention).
    pub fn remove_os_drag_bar_snapshot(&self, window: Option<WindowId>) {
        self.os_drag_bars.remove(window);
    }

    /// Publish the tab bar snapshot for the child window keyed by `id`.
    /// No-op if the child isn't found. Called from the child's redraw
    /// path right after `Renderer::render`.
    pub fn publish_child_window_tab_bar(&self, id: WindowId) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(child) = self.windows.get(&id) else {
            // When: `id` tracks no window, so there is no child tab bar to
            // publish geometry for.
            return;
        };
        let Some(win) = child.window.as_ref() else {
            // When: this `child` has no live window, so screen geometry cannot be
            // measured and the snapshot would carry stale coordinates.
            return;
        };
        let inner_origin = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = win.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let Some(r) = child.renderer.as_ref() else {
            // When: this `child` has no renderer, so tab-bar height is unknown and
            // the layout below has no metrics to compute against.
            return;
        };
        let layout =
            TabBarLayout::compute_with_height(&child.tabs, raster_w, r.tab_bar_logical_height())
                .with_top_offset(r.tab_bar_y_offset())
                .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(id), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    pub(super) fn finish_tab_drag(
        &mut self,
        session: crate::tab_drag::DragSession<WindowId>,
        action: crate::tab_drag::DragAction<WindowId>,
        tear_out: impl FnOnce(&mut Self, WindowId, usize),
    ) -> bool {
        let Some(index) = self.tab_index_of_id(session.source_window, session.source_tab) else {
            // When: `session.source_tab` is absent from `source_window`, cancel rather than moving its former slot's occupant.
            self.cancel_drag_session();
            return false;
        };
        match action {
            crate::tab_drag::DragAction::ReturnToOriginalBar => {
                // When: `ReturnToOriginalBar` keeps the captured tab in place, no topology mutation is owed.
            }
            crate::tab_drag::DragAction::ReorderTab { to } => {
                if let Some(window) = self.windows.get_mut(&session.source_window) {
                    window.reorder_tab(index, to);
                }
            }
            crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                // When: MergeIntoWindow selects target, preserve the transfer refusal instead of reporting a completed drag.
                let moved = if self.main_window_id == Some(session.source_window) {
                    self.merge_main_into_child(index, target)
                } else {
                    // When: session.source_window is not main_window_id, keep child-specific source close policy.
                    self.merge_child_into_target(session.source_window, index, target)
                };
                if !moved {
                    // When: moved is false, source custody is restored and the gesture must report no movement.
                    return false;
                }
            }
            crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                tear_out(self, session.source_window, index);
            }
        }
        if let Some(window) = self.windows.get(&session.source_window) {
            window.request_redraw();
        }
        true
    }

    /// begin an OS-level tab drag session via the installed
    /// backend. Returns `true` when the backend was invoked, `false`
    /// when no backend is installed or no event-loop proxy exists (in
    /// which case the caller falls back to the existing tear_out path).
    ///
    /// Captures the stable tab id before the native backend can process queued topology changes.
    pub fn begin_os_tab_drag(
        &mut self,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    ) -> bool {
        let Some(source_tab) = self.tab_id_at(source_window, source_tab_idx) else {
            // When: `source_tab_idx` no longer names a live tab, refuse before the native backend takes the gesture.
            return false;
        };
        let Some(handle) = self.os_drag_app_handle() else {
            // When: no `os_drag_app_handle` can be built, so the platform has no
            // drag context and recording a source would strand it.
            return false;
        };
        let Some(backend) = self.os_drag_backend.as_mut() else {
            // When: no `os_drag_backend` is installed, so nothing can carry the
            // session and the source must not be recorded.
            return false;
        };
        self.os_drag_source = Some((source_window, source_tab));
        backend.begin_session(handle, source_window, source_tab_idx, payload_json, drag_image_png);
        true
    }

    /// does the installed backend own the gesture end-to-end?
    /// `try_os_drag_handoff` consults this to decide whether to skip
    /// the legacy `OsDragSink` after `begin_os_tab_drag` returns —
    /// running both on Windows would invoke `DoDragDrop` twice.
    pub fn os_drag_backend_handles_full_gesture(&self) -> bool {
        self.os_drag_backend.as_ref().map(|b| b.handles_full_gesture()).unwrap_or(false)
    }

    pub(super) fn owns_native_drop_target(&self) -> bool {
        self.os_drag_backend.as_ref().is_some_and(|backend| backend.owns_native_drop_target())
    }

    pub(super) fn native_drop_attributes(&self, attrs: WindowAttributes) -> WindowAttributes {
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_drag_and_drop(!self.owns_native_drop_target())
        }
        #[cfg(not(windows))]
        {
            attrs
        }
    }

    /// Register the selected native drop owner before revealing a window or transferring live panes.
    pub fn register_window_with_os_drag_backend(
        &mut self,
        window_id: WindowId,
        window: &std::sync::Arc<winit::window::Window>,
    ) -> Result<(), String> {
        if self.os_drag_backend.is_none() {
            // When: os_drag_backend is absent, winit retains its default file-drop ownership.
            return Ok(());
        }
        let handle = self
            .os_drag_app_handle()
            .ok_or_else(|| "native drop event loop unavailable".to_string())?;
        self.os_drag_backend.as_mut().unwrap().register_window(handle, window_id, window)
    }

    pub(super) fn release_child_window_registries(&mut self, window_id: WindowId) {
        self.cancel_window_rename(window_id);
        self.cancel_tab_edit(window_id);
        self.pending_redraw_windows.remove(&window_id);
        if let Some(workers) = &self.path_workers {
            // A closed window leaves no waiting probe; one already executing is
            // discarded on arrival.
            workers.cancel_window(window_id);
        }
        self.window_keys.remove(window_id);
        self.os_drag_bars.remove(Some(window_id));
        if let Some(backend) = self.os_drag_backend.as_mut() {
            if let Err(error) = backend.unregister_window(window_id) {
                // Failed revocation leaves native custody with the backend for its final cleanup.
                tracing::error!(target: "sonicterm_app::app", ?window_id, %error, "native drop-target release failed");
            }
        }
    }

    /// dispatcher entry point for `UserEvent::DragMoved`.
    /// Drains the mailbox; currently a no-op beyond logging — the
    /// drag-chip overlay is rendered from `tab_drag` state, not from
    /// the OS cursor stream. Reserved for future "highlight drop
    /// target in destination bar" feedback.
    pub fn handle_os_drag_moved(&mut self) -> Option<(i32, i32)> {
        let pos = self.os_drag_pending.take_moved();
        if let Some(p) = pos {
            tracing::trace!(target: "sonicterm_app::app", ?p, "os_drag_session: cursor moved");
        }
        pos
    }

    /// dispatcher entry point for `UserEvent::DragEnded`.
    /// Drains the mailbox outcome and routes it: `DroppedOnBar` →
    /// [`Self::transfer_tab`]; `Cancelled` → [`Self::cancel_drag_session`];
    /// `DroppedOnEmpty` is left for the existing tear_out path (this
    /// dispatcher just clears the in-flight bookkeeping). Returns the
    /// outcome that was processed for tests to assert on.
    pub fn handle_os_drag_ended(&mut self) -> Option<os_drag::DragOutcome> {
        let outcome = self.os_drag_pending.take_ended()?;
        let source = self.os_drag_source.take().and_then(|(window, tab)| {
            self.tab_index_of_id(window, tab).map(|index| (window, index, tab))
        });
        match outcome {
            os_drag::DragOutcome::DroppedOnBar { target_window, target_slot } => {
                // When: the drop landed on a bar, so `target_window` and
                // `target_slot` name where the dragged tab should be inserted.
                let Some((src_win, src_idx, _)) = source else {
                    // When: no `source` was recorded, so there is no tab to move
                    // and the stale drag state is cancelled instead.
                    tracing::warn!(
                        target: "sonicterm_app::app",
                        "os_drag_session: DroppedOnBar arrived with no recorded source — cancelling"
                    );
                    self.cancel_drag_session();
                    return Some(outcome);
                };
                // `source` / `target` are `Option<WindowId>`, where
                // `None` means "the App's main window". The
                // backend always reports a concrete WindowId on the
                // source side, but the *target* may legitimately be the
                // main window. Detect that by comparing against the
                // App's `window` field.
                let src_opt = (self.main_window_id != Some(src_win)).then_some(src_win);
                let tgt_opt = target_window.filter(|id| Some(*id) != self.main_window_id);
                if let Err(e) = self.transfer_tab(src_opt, src_idx, tgt_opt, target_slot) {
                    tracing::warn!(target: "sonicterm_app::app", ?e, "os_drag_session: transfer_tab refused — cancelling");
                    self.cancel_drag_session();
                }
            }
            os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos } => {
                tracing::debug!(
                    target: "sonicterm_app::app",
                    ?drop_screen_pos,
                    "os_drag_session: DroppedOnEmpty — in-process tear-out"
                );
                // replace the legacy
                // out-of-process tear-out (child-window via
                // `spawn_tearout_child` → `Command::new`) with an
                // in-process create. Enqueue a typed `PendingTearOut`
                // request carrying the recorded source tab handle and
                // the Win32 cursor screen position; the next
                // event-loop tick drains it via the existing
                // `drain_pending_window_creates` slot, which now
                // builds the child window directly from the reusable
                // helper extracted from `tear_out.rs`.
                if let Some((src_win, src_idx, source_tab)) = source {
                    self.pending_tear_out = Some(PendingTearOut {
                        source_window: src_win,
                        source_tab_idx: src_idx,
                        source_tab_id: Some(source_tab),
                        drop_screen_pos: Some(drop_screen_pos),
                    });
                } else {
                    // When: no source was recorded, so no tab can be torn out and
                    // the drop is logged rather than acted on.
                    tracing::warn!(
                        target: "sonicterm_app::app",
                        "os_drag_session: DroppedOnEmpty without recorded source — no tear-out"
                    );
                }
                // Do NOT call `cancel_drag_session` inline
                // here. The `DroppedOnEmpty` path triggers a
                // tear-out-spawn that creates a brand new top-level
                // window via the `pending_new_window` /
                // `pending_tear_out` drain. If we cancel inline,
                // cross-window drag-residue cleanup runs BEFORE the
                // new window exists, racing the spawn and potentially
                // freezing Explorer's drag thread on Windows when the
                // OLE drop-target tear-down sequence overlaps with new
                // HWND creation. Defer cancellation to
                // `drain_pending_os_teardown`, which runs AFTER
                // `drain_pending_window_creates` at the event-loop
                // boundary. Order matters; this flag controls only
                // WHEN cancel runs, not WHETHER — the all-windows
                // loop still runs unconditionally on drain (preserves
                // the `os_drag_cleanup.rs:172-201` idempotence
                // guarantee).
                self.pending_os_teardown = true;
            }
            os_drag::DragOutcome::Cancelled => {
                self.cancel_drag_session();
            }
        }
        Some(outcome)
    }

    /// cancel an in-flight drag session. Wired
    /// to the ESC key handler in `window_event.rs` (any window's
    /// `WindowEvent::KeyboardInput` with `NamedKey::Escape` clears
    /// the App's drag_session AND every per-window drag_session) so
    /// the gesture is abandoned with the source tab left in place.
    /// Returns `true` if a drag session was actively cleared, `false`
    /// when no drag was in progress.
    #[doc(hidden)]
    pub fn cancel_drag_session(&mut self) -> bool {
        let mut had = false;
        // (defensive): snapshot window-id keys BEFORE the
        // mutation loop. The loop body calls `clear_drag_chip` /
        // `request_redraw`, neither of which mutate `self.windows`
        // today, but a future per-window handler (or a winit reentrant
        // callback on Windows under heavy load) could
        // insert/remove a window mid-iteration. Iterating a snapshot of
        // `Vec<WindowId>` is panic-free and matches intent: cancel
        // residue on the set of windows that exist RIGHT NOW. The
        // all-windows loop runs UNCONDITIONALLY — never short-circuit;
        // `os_drag_cleanup.rs:172-201` asserts this on a re-armed
        // second invocation.
        let ids: Vec<_> = self.windows.keys().copied().collect();
        // Invoke the test-only
        // post-snapshot hook AFTER `ids` is collected but BEFORE the
        // iteration body starts. The `take()` releases the hook so it
        // never re-fires, and (more importantly) leaves no live borrow
        // on `self` — the closure can freely mutate `self.windows`,
        // which is the exact race we need to exercise to prove the
        // `get_mut(&id).else { continue }` arm below fires. Always
        // `None` in production (the setter is `__test_*`-gated).
        if let Some(hook) = self.test_post_snapshot_hook.take() {
            hook(self);
        }
        // clear ALL per-window drag residue, not just
        // drag_session / drag_target. Previously `pressed_tab` and
        // `mouse_down` were only cleared on the main window, and the
        // renderer's `drag_chip` overlay was never cleared by this path
        // at all — so an OS-drag end (which bypasses the normal
        // MouseInput::Released handlers in window_event.rs / child_window.rs
        // that DO clear drag_chip) left a stale grey chip rectangle
        // floating in empty pane space until the next render forced a
        // refresh. Iterate every WindowState (main + children) and wipe
        // the lot.
        for id in ids {
            let Some(ws) = self.windows.get_mut(&id) else {
                // When: `windows` dropped `id` between the snapshot and this
                // iteration, so its residue is already gone with the window.
                continue;
            };
            if ws.drag_session.take().is_some() {
                had = true;
            }
            ws.drag_target = None;
            ws.pressed_tab = None;
            ws.mouse_down = false;
            window_event::cancel_pointer_gesture(&mut ws.pointer_gesture);
            // also abandon any scrollbar/splitter drag residue —
            // a global drag-cancel should leave no gesture half-held on any
            // window, mirroring the focus-loss cleanup.
            ws.scrollbar_drag = None;
            ws.splitter_drag = None;
            // Clear the renderer's persistent
            // drag-chip overlay AND the headless-test marker via a single
            // helper so production and test paths can never diverge. The
            // per-frame emitter keeps drawing
            // whatever Some(_) value sits in the renderer, so leaving it
            // behind ships a stale chip until something else triggers a
            // set_drag_chip(None). For headless test windows the renderer
            // is None — the `test_drag_chip_marker` mirror is what
            // the cleanup tests assert against.
            ws.clear_drag_chip();
            // Force a repaint so the cleared chip actually leaves the
            // screen instead of waiting for the next external event.
            if let Some(w) = ws.window.as_ref() {
                w.request_redraw();
            }
        }
        self.os_drag_handoff_started = false;
        had
    }
}

#[cfg(test)]
#[path = "os_drag_tests.rs"]
mod os_drag_tests;
