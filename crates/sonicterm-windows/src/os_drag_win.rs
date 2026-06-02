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

use std::sync::{Arc, Mutex, OnceLock};

use sonicterm_app::os_drag::{DragAck, OsDragSink, PendingPayloadSlot, TabPayload};

use windows::core::HRESULT;
use windows::core::{implement, w, BOOL, PCWSTR};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DV_E_FORMATETC, DV_E_TYMED, E_NOTIMPL, HWND,
    OLE_E_ADVISENOTSUPPORTED, POINTL, S_OK, WPARAM,
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
        // SAFETY: `RegisterClipboardFormatW` is process-global and
        // thread-safe; the wide string literal is null-terminated.
        let id = unsafe { RegisterClipboardFormatW(w!("com.sonic-terminal.tab.v1")) };
        if id == 0 {
            tracing::error!("RegisterClipboardFormatW(com.sonic-terminal.tab.v1) returned 0");
        }
        id as u16
    })
}

// ---- Pending-payload slot ----------------------------------------------------

/// Global single-slot mailbox written by the OLE worker thread (via
/// [`DropTarget::Drop`]) and drained by the winit main thread via
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

// ---- Phase C2 AppHandle slot -------------------------------------------------

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

/// Call once on Windows startup, before any drag-drop API. Idempotent
/// across re-invocations within the same process (Windows refcounts
/// internally) but should still be paired with a single
/// [`shutdown_ole`] at exit.
pub fn init_ole() {
    // SAFETY: `OleInitialize` is the documented one-call-per-thread
    // init for the apartment-threaded COM model OLE drag-drop needs.
    let hr = unsafe { OleInitialize(None) };
    if hr.is_err() {
        tracing::error!(?hr, "OleInitialize failed");
    }
}

/// Tear down OLE. Safe to call on a thread that never called
/// `OleInitialize` — Windows will simply ignore it.
pub fn shutdown_ole() {
    // SAFETY: paired with init_ole; harmless if init failed.
    unsafe { OleUninitialize() };
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
        // SAFETY: caller guarantees pformatetcin is a valid FORMATETC.
        let fmt = unsafe { &*pformatetcin };
        if !self.matches(fmt) {
            return Err(DV_E_FORMATETC.into());
        }
        // Allocate moveable HGLOBAL and copy JSON bytes in.
        let len = self.json.len();
        // SAFETY: GMEM_MOVEABLE + positive size is the documented
        // allocator pattern for clipboard/drag payloads.
        let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, len) }
            .map_err(|_| windows::core::Error::from(E_NOTIMPL))?;
        // SAFETY: pointer returned by GlobalLock is valid for `len`
        // bytes until GlobalUnlock; we only touch it within this scope.
        unsafe {
            let dst = GlobalLock(hglobal) as *mut u8;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(self.json.as_ptr(), dst, len);
                let _ = GlobalUnlock(hglobal);
            }
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
        // SAFETY: caller guarantees pformatetc is a valid pointer.
        let fmt = unsafe { &*pformatetc };
        if self.matches(fmt) {
            S_OK
        } else {
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
            return DRAGDROP_S_CANCEL;
        }
        // ESC also checked explicitly (callers occasionally feed
        // grfKeyState without the BOOL).
        // SAFETY: GetAsyncKeyState is thread-safe.
        if unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } as u16 & 0x8000 != 0 {
            return DRAGDROP_S_CANCEL;
        }
        // Primary button released → drop.
        if (grfkeystate & MK_LBUTTON).0 == 0 {
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
struct DragDropOutcome {
    hr: HRESULT,
    effect: u32,
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
    let data: IDataObject = SonicTermDataObject { json: payload_json.as_bytes().to_vec() }.into();
    let source: IDropSource = SonicTermDropSource.into();
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: DoDragDrop is the documented entry point. Both COM
    // objects outlive the call (kept on the stack here).
    let hr = unsafe {
        DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_MOVE, &mut effect as *mut _)
    };
    if hr.is_err() {
        tracing::warn!(?hr, "DoDragDrop returned error");
    }
    DragDropOutcome { hr, effect: effect.0 }
}

#[allow(dead_code)]
pub fn begin_tab_drag(payload_json: &str) -> u32 {
    begin_tab_drag_outcome(payload_json).effect
}

// ---- OsDragSink wiring ------------------------------------------------------

/// `OsDragSink` impl that, on `begin_drag`, kicks off the OLE drag
/// loop synchronously. A normal SonicTerm drop target returns
/// `DROPEFFECT_MOVE`; a drop on bare desktop / non-SonicTerm targets returns
/// `DROPEFFECT_NONE`, which we treat as a Windows tear-out by spawning a
/// child `sonicterm-windows.exe` with the serialized payload.
pub struct WinOsDragSink;

impl WinOsDragSink {
    pub fn arc() -> Arc<dyn OsDragSink> {
        Arc::new(WinOsDragSink)
    }
}

impl OsDragSink for WinOsDragSink {
    fn begin_drag(&self, payload: &TabPayload) -> DragAck {
        let json = match payload.to_json() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(?e, "TabPayload serialize failed; not starting drag");
                return DragAck::NotAcknowledged;
            }
        };
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
        return spawn_child(payload_json);
    }
    if outcome.hr == DRAGDROP_S_DROP && outcome.effect == DROPEFFECT_MOVE.0 {
        return DragAck::Accepted;
    }
    if outcome.hr == DRAGDROP_S_CANCEL {
        return DragAck::NotAcknowledged;
    }
    DragAck::NotAcknowledged
}

fn spawn_tearout_child(payload_json: &str) -> DragAck {
    // #536 profile: explicit Instant timing for tear_out_spawn_exe.
    // Opus diagnosis expects this to dominate tear-out wall-clock
    // cost. Using `Instant::now()` instead of `info_span!.entered()`
    // because the default `sonicterm-logging` subscriber doesn't have
    // `with_span_events(FmtSpan::CLOSE)` configured, so span timing
    // would never be emitted.
    let __t_spawn = std::time::Instant::now();
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(?e, "Windows tab tear-out: current_exe failed");
            tracing::info!(elapsed = ?__t_spawn.elapsed(), "[perf] tear_out_spawn_exe (failed)");
            return DragAck::NotAcknowledged;
        }
    };
    match std::process::Command::new(exe).arg("--tear-out-payload").arg(payload_json).spawn() {
        Ok(child) => {
            tracing::info!(pid = child.id(), "Windows tab tear-out: spawned child window");
            tracing::info!(elapsed = ?__t_spawn.elapsed(), "[perf] tear_out_spawn_exe");
            DragAck::Accepted
        }
        Err(e) => {
            tracing::error!(?e, "Windows tab tear-out: failed to spawn child window");
            tracing::info!(elapsed = ?__t_spawn.elapsed(), "[perf] tear_out_spawn_exe (failed)");
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
            return DROPEFFECT_MOVE;
        }
        if has_format(data, CF_HDROP.0, TYMED_HGLOBAL.0 as u32) {
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
            // SAFETY: caller-provided out-pointer is non-null per OLE.
            unsafe { *pdweffect = DROPEFFECT_NONE };
            return Ok(());
        };
        let eff = DropTarget::preferred_effect(data);
        // SAFETY: out-param is OLE-managed.
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
        // SAFETY: out-param is OLE-managed.
        unsafe {
            if (*pdweffect).0 == 0 {
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
            // SAFETY: out-param is OLE-managed.
            unsafe { *pdweffect = DROPEFFECT_NONE };
            if let Some(handle) = snapshot_drop_outcome_handle() {
                handle.post_drag_ended(sonicterm_app::app::os_drag::DragOutcome::Cancelled);
            }
            return Ok(());
        };
        // CF_SONIC_TAB takes priority.
        if let Some(json) = read_hglobal_utf8(data, cf_sonic_tab()) {
            match TabPayload::from_json(&json) {
                Ok(_p) => {
                    // Phase C2 (PR #295 review fix): resolve the real
                    // destination via the shared TabBarRegistry the App
                    // publishes into every frame. Falls back to
                    // DroppedOnEmpty (tear out at drop point) when the
                    // cursor isn't over any registered SonicTerm tab bar
                    // but IS over a SonicTerm window's client area. If the
                    // cursor isn't over any SonicTerm window at all we
                    // still report DroppedOnEmpty so the source side
                    // spawns a tear-out at the drop point.
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
                    // SAFETY: OLE out-param.
                    unsafe { *pdweffect = DROPEFFECT_MOVE };
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(?e, "CF_SONIC_TAB JSON malformed; ignoring");
                    if let Some(handle) = snapshot_drop_outcome_handle() {
                        handle.post_drag_ended(sonicterm_app::app::os_drag::DragOutcome::Cancelled);
                    }
                }
            }
        }
        // Fall through to CF_HDROP file drop.
        if let Some(paths) = read_hdrop(data) {
            // Route through the bridge so the main thread spawns the
            // paste action under the App borrow. Falling back to the
            // legacy install_file_drop_sink callback if one was
            // installed for tests / future use.
            let pathbufs: Vec<std::path::PathBuf> =
                paths.iter().map(std::path::PathBuf::from).collect();
            sonicterm_app::os_drag_bridge::push_files(pathbufs);
            if let Some(sink) = file_drop_sink().lock().unwrap_or_else(|p| p.into_inner()).clone() {
                let quoted = paths.iter().map(|p| shell_quote(p)).collect::<Vec<_>>().join(" ");
                sink(quoted);
            } else {
                tracing::debug!(?paths, "CF_HDROP routed via os_drag_bridge");
            }
            // SAFETY: OLE out-param.
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
        // SAFETY: OLE out-param.
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
    // SAFETY: QueryGetData accepts a borrowed FORMATETC by pointer.
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
    // SAFETY: GetData returns an STGMEDIUM owned by the caller; we
    // ReleaseStgMedium it before returning.
    let mut medium: STGMEDIUM = unsafe { data.GetData(&fmt as *const _).ok()? };
    let result = unsafe {
        let hglobal = windows::Win32::Foundation::HGLOBAL(medium.u.hGlobal.0);
        let size = GlobalSize(hglobal);
        if size == 0 {
            None
        } else {
            let ptr = GlobalLock(hglobal) as *const u8;
            if ptr.is_null() {
                None
            } else {
                let slice = std::slice::from_raw_parts(ptr, size);
                // Strip trailing nulls (some sources pad).
                let end = slice.iter().position(|&b| b == 0).unwrap_or(size);
                let s = String::from_utf8_lossy(&slice[..end]).into_owned();
                let _ = GlobalUnlock(hglobal);
                Some(s)
            }
        }
    };
    // SAFETY: medium came from GetData and must be released.
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
    // SAFETY: GetData / ReleaseStgMedium pair.
    let mut medium: STGMEDIUM = unsafe { data.GetData(&fmt as *const _).ok()? };
    let result = unsafe {
        let hdrop = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        if n == 0 {
            None
        } else {
            let mut out = Vec::with_capacity(n as usize);
            // First call with None to get required buffer length, then
            // again with the buffer.
            for i in 0..n {
                let needed = DragQueryFileW(hdrop, i, None) as usize;
                if needed == 0 {
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
    // SAFETY: medium came from GetData and must be released.
    unsafe { ReleaseStgMedium(&mut medium as *mut _) };
    result
}

// ---- Shell quoting for file-drop paste --------------------------------------

/// Quote a path safely for paste into a POSIX-style shell prompt.
/// Single quotes everything, escaping embedded `'` as `'\''`. Empty
/// input becomes `''`.
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

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
pub unsafe fn register_for_window(hwnd: HWND) {
    let target: IDropTarget = DropTarget.into();
    // SAFETY: contract above.
    let hr = unsafe { RegisterDragDrop(hwnd, &target) };
    if hr.is_err() {
        tracing::error!(?hr, "RegisterDragDrop failed");
    } else {
        // OLE holds its own ref; we can let `target` drop here.
        tracing::debug!("RegisterDragDrop installed");
    }
}

/// Pair of [`register_for_window`]. Safe to call on an HWND that was
/// never registered (OLE simply returns an error which we log).
///
/// # Safety
///
/// Caller must ensure the HWND is still valid.
#[allow(dead_code)]
pub unsafe fn unregister_for_window(hwnd: HWND) {
    // SAFETY: contract above.
    let hr = unsafe { RevokeDragDrop(hwnd) };
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

// NOTE (CLAUDE.md §5): Tests stay inline. `sonicterm-windows` is a `[[bin]]`
// crate (no `lib.rs`), so integration tests under `tests/` cannot
// reference the bin's items by path.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_basic() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("/tmp/file.txt"), "'/tmp/file.txt'");
        assert_eq!(shell_quote("C:\\Users\\me\\My File.txt"), "'C:\\Users\\me\\My File.txt'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a' b' c"), "'a'\\'' b'\\'' c'");
    }

    #[test]
    fn cf_sonic_tab_is_nonzero_and_stable() {
        // Windows recycles per-process-per-name, so two calls must
        // return the same ID.
        let a = cf_sonic_tab();
        let b = cf_sonic_tab();
        assert_eq!(a, b, "cf_sonic_tab must be cached");
        assert_ne!(a, 0, "RegisterClipboardFormatW returned 0");
    }

    #[test]
    fn cancel_does_not_tear_out() {
        let mut spawned = false;
        let ack = drag_ack_for_outcome(
            DragDropOutcome { hr: DRAGDROP_S_CANCEL, effect: DROPEFFECT_NONE.0 },
            "{}",
            |_| {
                spawned = true;
                DragAck::Accepted
            },
        );

        assert_eq!(ack, DragAck::NotAcknowledged);
        assert!(!spawned, "cancel must not spawn tear-out child");
    }

    #[test]
    fn outside_drop_tears_out_only_after_drop_hresult() {
        let mut spawned = false;
        let ack = drag_ack_for_outcome(
            DragDropOutcome { hr: DRAGDROP_S_DROP, effect: DROPEFFECT_NONE.0 },
            "{}",
            |_| {
                spawned = true;
                DragAck::Accepted
            },
        );

        assert_eq!(ack, DragAck::Accepted);
        assert!(spawned, "real outside-target drop should spawn tear-out child");
    }
}
