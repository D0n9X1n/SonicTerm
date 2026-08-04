//! Windows OLE drag-and-drop for SonicTerm.
//!
//! Implements both ends of the cross-process tab-drag wire defined in
//! [`sonicterm_app::os_drag`]:
//!
//!   * **Source** ([`begin_tab_drag`] + [`WinOsDragSink`]): builds an
//!     `IDataObject` that exposes the [`TabPayload`] JSON under the
//!     custom clipboard format `CF_SONIC_TAB`
//!     (= `RegisterClipboardFormatW("com.sonic-terminal.tab.v1")`) and
//!     calls `DoDragDrop` with an `IDropSource` whose
//!     `QueryContinueDrag` honours ESC (cancel) and primary-button
//!     release (drop). If OLE returns `DROPEFFECT_NONE` (no target
//!     accepted the drop), the sink spawns a new `sonicterm-windows.exe`
//!     with `--tear-out-payload <json>` and reports acceptance so the
//!     source tab can be removed.
//!   * **Destination** ([`DropTarget`] / [`register_for_window`]):
//!     `IDropTarget` registered on the winit HWND via `RegisterDragDrop`.
//!     `Drop()` accepts either `CF_SONIC_TAB` (parsed into a
//!     [`TabPayload`] and stashed in [`PENDING_PAYLOAD`] for the main
//!     event loop to drain) or `CF_HDROP` (Explorer file drop —
//!     shell-quoted paths are sent to the focused pane).
//!
//! Thread model: OLE callbacks run on the OLE worker thread. The
//! [`PendingPayloadSlot`] guarantees safe hand-off to the winit main
//! thread, which polls it from
//! [`take_pending_payload`].
//!
//! All entry points are `#[cfg(target_os = "windows")]`-gated so the
//! file compiles to an empty module on macOS — that's deliberate so
//! the Mac local gate keeps catching unrelated regressions without
//! pulling Windows COM into a Mac build.

#![cfg(target_os = "windows")]

use std::cell::Cell;
use std::sync::{Arc, Mutex, OnceLock};

use sonicterm_app::os_drag::{DragAck, OsDragSink, PendingPayloadSlot, TabPayload};

use windows::core::HRESULT;
use windows::core::{implement, w, BOOL, PCWSTR};
use windows::Win32::Foundation::{
    GlobalFree, CO_E_NOTINITIALIZED, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DV_E_FORMATETC,
    DV_E_TYMED, E_INVALIDARG, E_NOTIMPL, HWND, OLE_E_ADVISENOTSUPPORTED, POINTL, S_OK, WPARAM,
};
use windows::Win32::System::Com::{
    IDataObject, IDataObject_Impl, IEnumFORMATETC, DATADIR_GET, DVASPECT_CONTENT, FORMATETC,
    STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleInitialize,
    OleUninitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

// ---- Custom clipboard format -------------------------------------------------

/// Lazily-registered `CF_SONIC_TAB` value (Windows recycles the same
/// numeric ID per-process per-name, so caching is correct).
fn cf_sonic_tab() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| {
        let id =
            // SAFETY: RegisterClipboardFormatW is process-global and callable from any thread,
            // and the wide literal is null-terminated by the `w!` macro.
            unsafe { RegisterClipboardFormatW(w!("com.sonic-terminal.tab.v1")) };
        if id == 0 {
            tracing::error!("RegisterClipboardFormatW(com.sonic-terminal.tab.v1) returned 0");
        }
        id as u16
    })
}

// ---- Pending-payload slot ----------------------------------------------------

/// Global single-slot mailbox written by the OLE worker thread (via
/// the `IDropTarget::Drop` callback) and drained by the winit main thread via
/// [`take_pending_payload`]. Mac uses NSPasteboard instead, so this
/// slot is Windows-only.
static PENDING_PAYLOAD: PendingPayloadSlot = PendingPayloadSlot::new();

/// Optional file-drop sink: a callback the app installs to receive
/// shell-quoted file paths from `CF_HDROP` Explorer drops. The Drop
/// handler invokes it from the OLE worker thread; the implementation
/// is expected to either be cheap (it usually just pushes bytes into
/// the focused PTY) or to forward the work to the main thread.
type FileDropSink = Arc<dyn Fn(String) + Send + Sync>;
static FILE_DROP_SINK: OnceLock<Mutex<Option<FileDropSink>>> = OnceLock::new();

fn file_drop_sink() -> &'static Mutex<Option<FileDropSink>> {
    FILE_DROP_SINK.get_or_init(|| Mutex::new(None))
}

/// Install a callback invoked when an Explorer file drop lands on the
/// SonicTerm window. The string passed in is already shell-quoted (POSIX
/// rules — Windows `cmd.exe` users typically run under a POSIX-ish
/// shell inside SonicTerm, mirroring the macOS behavior).
#[allow(dead_code)]
pub fn install_file_drop_sink<F: Fn(String) + Send + Sync + 'static>(f: F) {
    *file_drop_sink().lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(f));
}

/// Drain any payload that an `IDropTarget::Drop` callback may have
/// stashed since the last call. Called from the winit main thread.
pub fn take_pending_payload() -> Option<TabPayload> {
    PENDING_PAYLOAD.take()
}

// ---- AppHandle slot -------------------------------------------------

/// Slot for the [`sonicterm_app::app::os_drag::AppHandle`] the WinOsTabDragBackend
/// installs for the duration of a single `DoDragDrop` call. The
/// `IDropTarget::Drop` callback reads from here to post a real
/// `DragOutcome::Drop` (target_window + target_slot) back to the
/// dispatcher when a peer SonicTerm HWND accepts the drop within the same
/// process.
static DROP_OUTCOME_HANDLE: OnceLock<Mutex<Option<sonicterm_app::app::os_drag::AppHandle>>> =
    OnceLock::new();

fn drop_outcome_handle_slot() -> &'static Mutex<Option<sonicterm_app::app::os_drag::AppHandle>> {
    DROP_OUTCOME_HANDLE.get_or_init(|| Mutex::new(None))
}

/// Install the AppHandle the `IDropTarget::Drop` callback should use to
/// post a `DragOutcome::Drop`. Called by `WinOsTabDragBackend::begin_session`
/// immediately before `DoDragDrop`. Idempotent — replaces any
/// previously-installed handle.
pub fn install_drop_outcome_handle(handle: sonicterm_app::app::os_drag::AppHandle) {
    if let Ok(mut slot) = drop_outcome_handle_slot().lock() {
        *slot = Some(handle);
    }
}

/// Clear the AppHandle. Called by `WinOsTabDragBackend::begin_session`
/// after `DoDragDrop` returns so a subsequent unrelated drop (e.g.
/// from another SonicTerm process) doesn't reuse a stale handle.
pub fn clear_drop_outcome_handle() {
    if let Ok(mut slot) = drop_outcome_handle_slot().lock() {
        *slot = None;
    }
}

fn snapshot_drop_outcome_handle() -> Option<sonicterm_app::app::os_drag::AppHandle> {
    drop_outcome_handle_slot().lock().ok().and_then(|g| g.clone())
}

// ---- OLE process-wide init ---------------------------------------------------

thread_local! {
    static OLE_INIT_DEPTH: Cell<u32> = const { Cell::new(0) };
}

fn ole_initialized_on_current_thread() -> bool {
    OLE_INIT_DEPTH.get() != 0
}

/// Same-thread guard for one successful [`init_ole`] call.
pub struct OleGuard(std::marker::PhantomData<*const ()>);

// Lifecycle: OleGuard Drop calls OleUninitialize to release this thread's
// successful OLE initialization without permitting cross-thread ownership.
impl Drop for OleGuard {
    fn drop(&mut self) {
        OLE_INIT_DEPTH.set(OLE_INIT_DEPTH.get().saturating_sub(1));
        // SAFETY: OleGuard is created only after OleInitialize succeeds and is
        // retained on the initializing thread until this matching drop.
        unsafe { OleUninitialize() };
    }
}

/// Initialize OLE for this thread before any drag-drop API.
///
/// Returns a guard only on success; dropping it performs the required matching
/// `OleUninitialize`, including when startup exits early.
pub fn init_ole() -> Option<OleGuard> {
    let hr =
        // SAFETY: OleInitialize is the documented one-call-per-thread init for the
        // apartment-threaded COM model OLE drag-drop needs.
        unsafe { OleInitialize(None) };
    if hr.is_err() {
        // When: hr reports failure, so return no guard — an OleGuard would later call
        // OleUninitialize for an initialization that never succeeded.
        tracing::error!(?hr, "OleInitialize failed");
        return None;
    }
    OLE_INIT_DEPTH.set(OLE_INIT_DEPTH.get().saturating_add(1));
    Some(OleGuard(std::marker::PhantomData))
}

// ---- IDataObject implementation ---------------------------------------------

/// Minimal `IDataObject` exposing one `CF_SONIC_TAB` blob as
/// `CF_HGLOBAL`. We do not advertise `CF_HDROP` from the source side —
/// we only consume it as a target.
#[implement(IDataObject)]
struct SonicTermDataObject {
    /// UTF-8 JSON body (from [`TabPayload::to_json`]).
    json: Vec<u8>,
}

impl SonicTermDataObject {
    fn matches(&self, fmt: &FORMATETC) -> bool {
        fmt.cfFormat == cf_sonic_tab()
            && fmt.dwAspect == DVASPECT_CONTENT.0
            && (fmt.tymed & TYMED_HGLOBAL.0 as u32) != 0
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for SonicTermDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let fmt =
            // SAFETY: OLE guarantees pformatetcin points at a live FORMATETC for the duration
            // of this call, and the borrow does not outlive it.
            unsafe { &*pformatetcin };
        if !self.matches(fmt) {
            // When: fmt names a format this object does not publish, so report DV_E_FORMATETC
            // rather than hand back an unrelated medium.
            return Err(DV_E_FORMATETC.into());
        }
        // Allocate moveable HGLOBAL and copy JSON bytes in.
        let len = self.json.len();
        if len == 0 {
            // When: `len == 0`, no valid HGLOBAL medium can be advertised.
            return Err(E_INVALIDARG.into());
        }
        let hglobal =
            // SAFETY: GMEM_MOVEABLE with a positive size is the documented allocator pattern
            // for clipboard and drag payloads.
            unsafe { GlobalAlloc(GMEM_MOVEABLE, len) }
                .map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
        // SAFETY: GlobalLock returns a pointer valid for `len` bytes until GlobalUnlock, and
        // the copy stays inside that window.
        unsafe {
            let dst = GlobalLock(hglobal) as *mut u8;
            if dst.is_null() {
                // When: `dst.is_null()` is true, release the allocation and refuse an empty medium.
                let _ = GlobalFree(Some(hglobal));
                return Err(E_NOTIMPL.into());
            }
            std::ptr::copy_nonoverlapping(self.json.as_ptr(), dst, len);
            let _ = GlobalUnlock(hglobal);
        }
        let mut medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() };
        medium.u.hGlobal = hglobal;
        Ok(medium)
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> windows::core::HRESULT {
        let fmt =
            // SAFETY: OLE guarantees pformatetc points at a live FORMATETC for this call, and
            // the borrow does not outlive it.
            unsafe { &*pformatetc };
        if self.matches(fmt) {
            S_OK
        } else {
            // When: fmt names a format this object does not publish, so answer DV_E_FORMATETC
            // and let the caller offer another format.
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        _pformatetcout: *mut FORMATETC,
    ) -> windows::core::HRESULT {
        DV_E_FORMATETC
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, _dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        // EnumFormatEtc is optional; Explorer / native drop targets
        // can fall back to QueryGetData. Return E_NOTIMPL.
        Err(E_NOTIMPL.into())
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: windows::core::Ref<windows::Win32::System::Com::IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<windows::Win32::System::Com::IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

// ---- IDropSource implementation ---------------------------------------------

#[implement(IDropSource)]
struct SonicTermDropSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for SonicTermDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: BOOL,
        grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        use windows::Win32::System::SystemServices::MK_LBUTTON;
        if fescapepressed.as_bool() {
            // When: fescapepressed is OLE's own ESC report, so cancel before looking at any
            // button state.
            return DRAGDROP_S_CANCEL;
        }
        let escape_held =
            // SAFETY: GetAsyncKeyState reads process-global keyboard state and is callable
            // from any thread; VK_ESCAPE is a constant virtual-key code.
            unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) };
        if escape_held as u16 & 0x8000 != 0 {
            // When: escape_held has its high bit set, so ESC is down even though callers
            // sometimes pass grfkeystate without setting the escape BOOL.
            return DRAGDROP_S_CANCEL;
        }
        if (grfkeystate & MK_LBUTTON).0 == 0 {
            // When: grfkeystate no longer carries MK_LBUTTON, so the primary button was
            // released and OLE should finish the drop.
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        // Use OS default cursors.
        const DRAGDROP_S_USEDEFAULTCURSORS: windows::core::HRESULT =
            windows::core::HRESULT(0x00040102u32 as i32);
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

// ---- Public source-side entry points ----------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DragDropOutcome {
    pub(crate) hr: HRESULT,
    pub(crate) effect: u32,
}

/// Synchronously run a `DoDragDrop` loop carrying `payload_json` as the
/// `CF_SONIC_TAB` blob. Returns both the HRESULT and final `DROPEFFECT`
/// reported by OLE (`DROPEFFECT_COPY`, `DROPEFFECT_MOVE`, or
/// `DROPEFFECT_NONE`). The call blocks the calling thread until the user
/// releases the mouse or presses ESC.
///
/// MUST be called on a thread that has called `OleInitialize` —
/// typically the main UI thread.
fn begin_tab_drag_outcome(payload_json: &str) -> DragDropOutcome {
    if !ole_initialized_on_current_thread() {
        // When: `ole_initialized_on_current_thread()` is false, DoDragDrop cannot be called safely.
        return DragDropOutcome { hr: CO_E_NOTINITIALIZED, effect: DROPEFFECT_NONE.0 };
    }
    if payload_json.is_empty() {
        // When: `payload_json.is_empty()` is true, reject it before constructing IDataObject.
        return DragDropOutcome { hr: E_INVALIDARG, effect: DROPEFFECT_NONE.0 };
    }
    let data: IDataObject = SonicTermDataObject { json: payload_json.as_bytes().to_vec() }.into();
    let source: IDropSource = SonicTermDropSource.into();
    let mut effect = DROPEFFECT_NONE;
    let hr =
        // SAFETY: the caller is the OLE-initialized UI thread; `data`, `source`,
        // and `effect` are stack-owned here and outlive the modal call.
        unsafe {
            DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_MOVE, &mut effect as *mut _)
        };
    if hr.is_err() {
        tracing::warn!(?hr, "DoDragDrop returned error");
    }
    DragDropOutcome { hr, effect: effect.0 }
}

/// Begin a native OLE drag and return its HRESULT and final effect.
///
/// # Safety
///
/// The caller must have successfully called [`init_ole`] on this thread and
/// must keep that initialization alive for the duration of the modal drag.
#[allow(dead_code)]
// SAFETY: callers must uphold the OLE apartment contract required by
// `begin_tab_drag_outcome` and `DoDragDrop`.
pub(crate) unsafe fn begin_tab_drag(payload_json: &str) -> DragDropOutcome {
    begin_tab_drag_outcome(payload_json)
}

// ---- OsDragSink wiring ------------------------------------------------------

/// `OsDragSink` impl that, on `begin_drag`, kicks off the OLE drag
/// loop synchronously. A normal SonicTerm drop target returns
/// `DROPEFFECT_MOVE`; a drop on bare desktop / non-SonicTerm targets returns
/// `DROPEFFECT_NONE`, which we treat as a Windows tear-out by spawning a
/// child `sonicterm-windows.exe` with the serialized payload.
pub struct WinOsDragSink;

impl WinOsDragSink {
    /// Construct the sink after OLE initialization succeeds on the UI thread.
    ///
    /// # Safety
    ///
    /// The returned sink must be invoked only on the thread whose live
    /// [`OleGuard`] represents that initialization.
    // SAFETY: callers must keep the matching `OleGuard` live and invoke the
    // sink only on its OLE-initialized UI thread.
    pub unsafe fn arc() -> Arc<dyn OsDragSink> {
        Arc::new(WinOsDragSink)
    }
}

impl OsDragSink for WinOsDragSink {
    fn begin_drag(&self, payload: &TabPayload) -> DragAck {
        let json = match payload.to_json() {
            Ok(s) => s,
            Err(e) => {
                // When: to_json failed, so there is no CF_SONIC_TAB blob to publish; never
                // enter DoDragDrop, which would drag an empty payload.
                tracing::error!(?e, "TabPayload serialize failed; not starting drag");
                return DragAck::NotAcknowledged;
            }
        };
        if !ole_initialized_on_current_thread() {
            // When: `ole_initialized_on_current_thread()` is false, retain the source tab.
            tracing::error!("OLE is not initialized on this thread; tab drag ignored");
            return DragAck::NotAcknowledged;
        }
        let outcome = begin_tab_drag_outcome(&json);
        drag_ack_for_outcome(outcome, &json, spawn_tearout_child)
    }
}

fn drag_ack_for_outcome(
    outcome: DragDropOutcome,
    payload_json: &str,
    spawn_child: impl FnOnce(&str) -> DragAck,
) -> DragAck {
    if outcome.hr == DRAGDROP_S_DROP && outcome.effect == DROPEFFECT_NONE.0 {
        // When: DROPEFFECT_NONE after a drop means no OLE target claimed the tab, so treat
        // the release point as a tear-out rather than a cancel.
        return spawn_child(payload_json);
    }
    if outcome.hr == DRAGDROP_S_DROP && outcome.effect == DROPEFFECT_MOVE.0 {
        // When: DROPEFFECT_MOVE means a peer SonicTerm target accepted the tab, so the source
        // tab may be removed.
        return DragAck::Accepted;
    }
    if outcome.hr == DRAGDROP_S_CANCEL {
        // When: DRAGDROP_S_CANCEL means the user pressed ESC, so the source tab stays put.
        return DragAck::NotAcknowledged;
    }
    DragAck::NotAcknowledged
}

fn spawn_tearout_child(payload_json: &str) -> DragAck {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            // When: current_exe failed, so there is no image path to relaunch and the tab must
            // stay in the source window.
            tracing::error!(?e, "Windows tab tear-out: current_exe failed");
            return DragAck::NotAcknowledged;
        }
    };
    match std::process::Command::new(exe).arg("--tear-out-payload").arg(payload_json).spawn() {
        Ok(child) => {
            tracing::info!(pid = child.id(), "Windows tab tear-out: spawned child window");
            DragAck::Accepted
        }
        Err(e) => {
            tracing::error!(?e, "Windows tab tear-out: failed to spawn child window");
            DragAck::NotAcknowledged
        }
    }
}

// ---- IDropTarget implementation ---------------------------------------------

#[implement(IDropTarget)]
struct DropTarget;

impl DropTarget {
    /// Inspect an incoming data object: prefer `CF_SONIC_TAB` over
    /// `CF_HDROP` (a sibling SonicTerm window's tab is more specific than
    /// a generic file drop).
    fn preferred_effect(data: &IDataObject) -> DROPEFFECT {
        if has_format(data, cf_sonic_tab(), TYMED_HGLOBAL.0 as u32) {
            // When: data publishes CF_SONIC_TAB, so a sibling window's tab outranks any file
            // payload and moves rather than copies.
            return DROPEFFECT_MOVE;
        }
        if has_format(data, CF_HDROP.0, TYMED_HGLOBAL.0 as u32) {
            // When: data publishes CF_HDROP, so this is an Explorer file drop, which copies
            // paths into the pane instead of moving anything.
            return DROPEFFECT_COPY;
        }
        DROPEFFECT_NONE
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows::core::Ref<IDataObject>,
        _grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let Some(data) = pdataobj.as_ref() else {
            // When: pdataobj carried no data object, so no format can be inspected and the
            // cursor must show "no drop".
            // SAFETY: OLE owns pdweffect and guarantees it is non-null for this callback.
            unsafe { *pdweffect = DROPEFFECT_NONE };
            return Ok(());
        };
        let eff = DropTarget::preferred_effect(data);
        // SAFETY: OLE owns pdweffect and guarantees it is non-null for this callback.
        unsafe { *pdweffect = eff };
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // Keep whatever DragEnter chose — the cursor will reflect it.
        // SAFETY: OLE owns pdweffect and guarantees it is non-null for this callback.
        unsafe {
            if (*pdweffect).0 == 0 {
                // A cleared effect is restated as DROPEFFECT_NONE so OLE never reads an unset
                // value back out of the out-param.
                *pdweffect = DROPEFFECT_NONE;
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows::core::Ref<IDataObject>,
        _grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let Some(data) = pdataobj.as_ref() else {
            // When: pdataobj carried no data object, so there is nothing to parse; report the
            // gesture as cancelled so the source tab stays where it is.
            // SAFETY: OLE owns pdweffect and guarantees it is non-null for this callback.
            unsafe { *pdweffect = DROPEFFECT_NONE };
            if let Some(handle) = snapshot_drop_outcome_handle() {
                handle.post_drag_ended(sonicterm_app::app::os_drag::DragOutcome::Cancelled);
            }
            return Ok(());
        };
        // CF_SONIC_TAB takes priority.
        if let Some(json) = read_hglobal_utf8(data, cf_sonic_tab()) {
            // When: read_hglobal_utf8 returned a CF_SONIC_TAB blob, so resolve a SonicTerm
            // destination before considering any file payload.
            match TabPayload::from_json(&json) {
                Ok(_p) => {
                    // When: from_json parsed the payload, so resolve the real destination via
                    // the shared TabBarRegistry the App publishes into every frame.

                    // Falls back to DroppedOnEmpty (tear out at drop point) when the cursor
                    // isn't over any registered SonicTerm tab bar but IS over a SonicTerm
                    // window's client area. If the cursor isn't over any SonicTerm window at
                    // all we still report DroppedOnEmpty so the source side spawns a tear-out
                    // at the drop point.
                    if let Some(handle) = snapshot_drop_outcome_handle() {
                        let outcome = match handle.query_tab_bar_slot(pt.x, pt.y) {
                            Some((target_window, target_slot)) => {
                                sonicterm_app::app::os_drag::DragOutcome::DroppedOnBar {
                                    target_window,
                                    target_slot,
                                }
                            }
                            None => sonicterm_app::app::os_drag::DragOutcome::DroppedOnEmpty {
                                drop_screen_pos: (pt.x, pt.y),
                            },
                        };
                        handle.post_drag_ended(outcome);
                    }
                    // SAFETY: OLE owns pdweffect and guarantees it is non-null for this
                    // callback.
                    unsafe { *pdweffect = DROPEFFECT_MOVE };
                    return Ok(());
                }
                Err(e) => {
                    // A malformed blob means another producer published our clipboard format,
                    // so report Cancelled and let the file-drop path below try the same data.
                    tracing::warn!(?e, "CF_SONIC_TAB JSON malformed; ignoring");
                    if let Some(handle) = snapshot_drop_outcome_handle() {
                        handle.post_drag_ended(sonicterm_app::app::os_drag::DragOutcome::Cancelled);
                    }
                }
            }
        }
        // Fall through to CF_HDROP file drop.
        if let Some(paths) = read_hdrop(data) {
            // When: the drop carries CF_HDROP, so route the paths through the bridge and let
            // the main thread spawn the paste action under the App borrow.
            let pathbufs: Vec<std::path::PathBuf> =
                paths.iter().map(std::path::PathBuf::from).collect();
            sonicterm_app::os_drag_bridge::push_files(pathbufs);
            if let Some(sink) = file_drop_sink().lock().unwrap_or_else(|p| p.into_inner()).clone() {
                let quoted = paths.iter().map(|p| shell_quote(p)).collect::<Vec<_>>().join(" ");
                sink(quoted);
            } else {
                // When: no file_drop_sink is installed, so the bridge push above is the only
                // delivery and this records that the paths took that route.
                tracing::debug!(?paths, "CF_HDROP routed via os_drag_bridge");
            }
            // SAFETY: OLE owns pdweffect and guarantees it is non-null for this callback.
            unsafe { *pdweffect = DROPEFFECT_COPY };
            return Ok(());
        }
        // No recognised format → DroppedOnEmpty so the source-side
        // dispatcher can spawn a tear-out window at the drop point
        // (real outcome, not silent Cancelled).
        if let Some(handle) = snapshot_drop_outcome_handle() {
            handle.post_drag_ended(sonicterm_app::app::os_drag::DragOutcome::DroppedOnEmpty {
                drop_screen_pos: (pt.x, pt.y),
            });
        }
        // SAFETY: OLE owns pdweffect and guarantees it is non-null for this callback.
        unsafe { *pdweffect = DROPEFFECT_NONE };
        Ok(())
    }
}

// ---- IDataObject reading helpers --------------------------------------------

fn has_format(data: &IDataObject, cf: u16, tymed: u32) -> bool {
    let fmt = FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed,
    };
    // SAFETY: QueryGetData borrows fmt for the duration of the call and does not retain it;
    // fmt lives on this stack frame.
    unsafe { data.QueryGetData(&fmt as *const _).is_ok() }
}

/// Read an `HGLOBAL` payload by format and return it as a UTF-8 string
/// (lossy on invalid bytes). Returns `None` if the format isn't
/// offered or the buffer is empty.
fn read_hglobal_utf8(data: &IDataObject, cf: u16) -> Option<String> {
    let fmt = FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let mut medium: STGMEDIUM =
        // SAFETY: GetData yields an STGMEDIUM this function owns; the matching
        // ReleaseStgMedium below runs on every return path.
        unsafe { data.GetData(&fmt as *const _).ok()? };
    let result =
        // SAFETY: the medium's HGLOBAL stays valid until ReleaseStgMedium, and the slice is
        // read only between GlobalLock and GlobalUnlock.
        unsafe {
            let hglobal = windows::Win32::Foundation::HGLOBAL(medium.u.hGlobal.0);
            let size = GlobalSize(hglobal);
            if size == 0 {
                None
            } else {
                // When: size is non-zero, so the blob has bytes worth locking and decoding.
                let ptr = GlobalLock(hglobal) as *const u8;
                if ptr.is_null() {
                    None
                } else {
                    // When: ptr is a live lock, so the bytes can be read and copied out before
                    // GlobalUnlock.
                    let slice = std::slice::from_raw_parts(ptr, size);
                    // Strip trailing nulls (some sources pad).
                    let end = slice.iter().position(|&b| b == 0).unwrap_or(size);
                    let s = String::from_utf8_lossy(&slice[..end]).into_owned();
                    let _ = GlobalUnlock(hglobal);
                    Some(s)
                }
            }
        };
    // SAFETY: medium came from GetData above and must be released exactly once.
    unsafe { ReleaseStgMedium(&mut medium as *mut _) };
    result
}

/// Pull file paths out of an `HDROP` (`CF_HDROP`) payload.
fn read_hdrop(data: &IDataObject) -> Option<Vec<String>> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let mut medium: STGMEDIUM =
        // SAFETY: GetData yields an STGMEDIUM this function owns; the matching
        // ReleaseStgMedium below runs on every return path.
        unsafe { data.GetData(&fmt as *const _).ok()? };
    let result =
        // SAFETY: the medium's HDROP stays valid until ReleaseStgMedium, and every
        // DragQueryFileW call writes only into a buffer sized by its own length query.
        unsafe {
            let hdrop = HDROP(medium.u.hGlobal.0);
            let n = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
            if n == 0 {
                None
            } else {
                // When: n is non-zero, so the HDROP names files worth querying one by one.
                let mut out = Vec::with_capacity(n as usize);
                // First call with None to get required buffer length, then
                // again with the buffer.
                for i in 0..n {
                    let needed = DragQueryFileW(hdrop, i, None) as usize;
                    if needed == 0 {
                        // When: needed is zero, so this entry has no path text; skip it rather
                        // than push an empty string into the paste.
                        continue;
                    }
                    let mut buf = vec![0u16; needed + 1];
                    let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                    buf.truncate(got);
                    out.push(String::from_utf16_lossy(&buf));
                }
                Some(out)
            }
        };
    // SAFETY: medium came from GetData above and must be released exactly once.
    unsafe { ReleaseStgMedium(&mut medium as *mut _) };
    result
}

// ---- Shell quoting for file-drop paste --------------------------------------

/// Quote a path safely for paste into a POSIX-style shell prompt.
/// Re-exported from the shared `sonicterm-types` implementation so file
/// drops on macOS and Windows paste the same bytes.
pub use sonicterm_types::shell_quote_posix as shell_quote;

// ---- IDropTarget registration ----------------------------------------------

/// Register the global `DropTarget` against an HWND. Idempotent only
/// per-HWND in the OLE sense — Windows lets you re-register but it
/// leaks the previous registration. Pair with [`unregister_for_window`]
/// at shutdown.
///
/// # Safety
///
/// The HWND must be a valid, currently-alive window owned by the
/// calling thread, and OLE must have been initialized via
/// [`init_ole`] on that same thread.
// SAFETY: the caller's contract above guarantees a live, thread-owned HWND and an OLE-initialized
// thread, which is exactly what RegisterDragDrop requires.
pub unsafe fn register_for_window(hwnd: HWND) -> bool {
    if !ole_initialized_on_current_thread() {
        // When: `ole_initialized_on_current_thread()` is false, registration cannot satisfy OLE's apartment contract.
        tracing::error!("RegisterDragDrop skipped because OLE is not initialized on this thread");
        return false;
    }
    let target: IDropTarget = DropTarget.into();
    let hr =
        // SAFETY: hwnd is caller-guaranteed live and OLE-initialized on this thread; OLE
        // takes its own reference to `target`, so the local may drop afterwards.
        unsafe { RegisterDragDrop(hwnd, &target) };
    if hr.is_err() {
        tracing::error!(?hr, "RegisterDragDrop failed");
        false
    } else {
        // When: hr reports success, so OLE now holds its own reference and this window will
        // receive IDropTarget callbacks.
        tracing::debug!("RegisterDragDrop installed");
        true
    }
}

/// Pair of [`register_for_window`]. Safe to call on an HWND that was
/// never registered (OLE simply returns an error which we log).
///
/// # Safety
///
/// Caller must ensure the HWND is still valid.
// SAFETY: the caller's contract above guarantees the HWND is still valid, which is all
// RevokeDragDrop requires; an unregistered window simply returns an error.
#[allow(dead_code)]
pub unsafe fn unregister_for_window(hwnd: HWND) {
    let hr =
        // SAFETY: hwnd is caller-guaranteed still valid, and RevokeDragDrop tolerates a
        // window that was never registered by returning an error.
        unsafe { RevokeDragDrop(hwnd) };
    if hr.is_err() {
        tracing::debug!(?hr, "RevokeDragDrop returned (ignorable if never registered)");
    }
}

// Suppress unused warnings for items consumed only by test/external entries.
#[allow(dead_code)]
fn _suppress() {
    let _ = WPARAM(0);
    let _ = DATADIR_GET;
    let _ = DV_E_TYMED;
    let _ = PCWSTR::null();
}

// `sonicterm-windows` is a `[[bin]]` crate with no `lib.rs`, so integration
// tests under `tests/` cannot reference this module's items by path. Coverage
// for these entry points therefore lives in-crate, in the sibling
// `<module>_tests.rs` files this crate declares from their own modules.
