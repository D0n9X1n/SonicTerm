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
use winit::window::{Window, WindowId};

/// Re-export of [`winit::window::Window`] for the same reason as
/// [`BackendWindowId`] — platform backend crates need to spell the
/// `register_window` trait signature without taking a direct winit
/// dep just for the type name.
pub use winit::window::Window as BackendWindow;
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
    fn register_window(&mut self, _handle: AppHandle, _window_id: WindowId, _window: &Arc<Window>) {
    }
}

/// Snapshot of a single window's tab bar, in **screen** coordinates,
/// published by the App into a [`TabBarRegistry`] each frame so a
/// platform OS-drag backend running off the winit thread (Windows OLE
/// IDropTarget::Drop on the OLE worker thread, macOS NSDraggingDestination
/// on the AppKit main loop) can hit-test the drop cursor without having
/// to call back into the App's borrowed state.
///
/// Tabs are described by their horizontal extents only (`tab_lefts` +
/// `tab_rights`) — the vertical coordinate is covered by `bar_rect`'s
/// `top` / `bottom`. Slot resolution mirrors
/// [`sonic_ui::tabbar_view::TabBarLayout::drop_slot`] exactly: left of
/// tab `i`'s midpoint → slot `i`; right of the last midpoint → `n`.
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
    /// Left edge of each tab in screen X, in tab order. Length == number
    /// of tabs. Empty if the bar has no tabs (resolves to slot 0).
    pub tab_lefts: Vec<i32>,
    /// Right edge of each tab in screen X. Same length as `tab_lefts`.
    pub tab_rights: Vec<i32>,
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

    /// Build a [`TabBarSnapshot`] from a computed [`TabBarLayout`]
    /// (in window-local **logical** pixels) plus the destination window's
    /// **physical** inner-origin and HiDPI scale factor. All output rects
    /// are in screen-global **physical** pixels — the same coordinate
    /// system the OS reports drop cursors in.
    ///
    /// `inner_origin` is the window's `inner_position()` (top-left of
    /// the client area, screen-global, physical px). `inner_size` is
    /// `inner_size()` (physical px). `scale_factor` is `Window::scale_factor()`.
    ///
    /// Used by the App's per-frame redraw path to publish the live tab
    /// bar geometry into the shared [`TabBarRegistry`] so platform OS-drag
    /// backends can hit-test drop cursors without re-entering the App.
    pub fn from_layout(
        window: Option<WindowId>,
        inner_origin: (i32, i32),
        inner_size: (u32, u32),
        scale_factor: f32,
        layout: &sonic_ui::tabbar_view::TabBarLayout,
    ) -> Self {
        let sf = if scale_factor > 0.0 { scale_factor } else { 1.0 };
        let (ox, oy) = inner_origin;
        let (iw, ih) = inner_size;
        let window_rect = (ox, oy, ox + iw as i32, oy + ih as i32);
        // Bar rect: logical → physical, then translate by inner origin.
        let bar = &layout.bar;
        let bar_rect = (
            ox + (bar.x * sf).round() as i32,
            oy + (bar.y * sf).round() as i32,
            ox + ((bar.x + bar.w) * sf).round() as i32,
            oy + ((bar.y + bar.h) * sf).round() as i32,
        );
        let mut tab_lefts = Vec::with_capacity(layout.tabs.len());
        let mut tab_rights = Vec::with_capacity(layout.tabs.len());
        for t in &layout.tabs {
            tab_lefts.push(ox + (t.bg_rect.x * sf).round() as i32);
            tab_rights.push(ox + ((t.bg_rect.x + t.bg_rect.w) * sf).round() as i32);
        }
        Self { window, window_rect, bar_rect, tab_lefts, tab_rights }
    }

    /// Compute the insertion slot for a tab dropped at screen X `sx`.
    /// Mirrors `TabBarLayout::drop_slot`: returns the index of the
    /// first tab whose horizontal midpoint is to the right of `sx`, or
    /// `tab_lefts.len()` if `sx` is past the last midpoint. Empty bar
    /// → slot 0.
    pub fn drop_slot(&self, sx: i32) -> usize {
        debug_assert_eq!(self.tab_lefts.len(), self.tab_rights.len());
        for (i, (&l, &r)) in self.tab_lefts.iter().zip(self.tab_rights.iter()).enumerate() {
            let midx = (l + r) / 2;
            if sx < midx {
                return i;
            }
        }
        self.tab_lefts.len()
    }
}

/// Registry of currently-published [`TabBarSnapshot`]s, one per live
/// Sonic window in this process. The App publishes into it on every
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
    ///     window's tab bar — `slot` is the insertion index in `[0,
    ///     n]`.
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
                return Some((snap.window, snap.drop_slot(sx)));
            }
        }
        None
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
}

impl AppHandle {
    /// Wrap an existing [`EventLoopProxy`] + freshly-allocated mailbox.
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            pending: Arc::new(PendingDragOutcome::default()),
            bars: Arc::new(TabBarRegistry::default()),
        }
    }

    /// Reuse an existing mailbox — used by the App-side dispatcher so
    /// the same `Arc<PendingDragOutcome>` is shared between the
    /// backend's [`AppHandle`] clone and the App's own drain path.
    pub fn with_pending(
        proxy: EventLoopProxy<UserEvent>,
        pending: Arc<PendingDragOutcome>,
    ) -> Self {
        Self { proxy, pending, bars: Arc::new(TabBarRegistry::default()) }
    }

    /// Reuse both the mailbox and a shared [`TabBarRegistry`]. The App
    /// uses this so its own publishing path and the backend's reading
    /// path see the same registry instance.
    pub fn with_pending_and_bars(
        proxy: EventLoopProxy<UserEvent>,
        pending: Arc<PendingDragOutcome>,
        bars: Arc<TabBarRegistry>,
    ) -> Self {
        Self { proxy, pending, bars }
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
