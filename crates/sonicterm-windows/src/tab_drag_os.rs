//! UI-thread OLE gestures and successful drop-target custody for Windows native windows.

#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::os_drag::{
    AppHandle, BackendWindow as Window, BackendWindowId as WindowId, DragOutcome, OsTabDragBackend,
};
use windows::Win32::Foundation::HWND;

struct RegisteredWindow {
    window: Arc<Window>,
    hwnd: usize,
}

/// Native registration outcomes retained after the backend has released every HWND.
#[derive(Debug, Default)]
pub(crate) struct DropRegistrationReport {
    registrations: usize,
    revocations: usize,
    live: usize,
    failures: usize,
}

impl DropRegistrationReport {
    /// Require every scenario-owned registration to be paired with successful teardown.
    pub(crate) fn validate(
        &self,
        scenario: sonicterm_app::app::RuntimeSmokeScenario,
    ) -> Result<(), String> {
        // The early frame-fault process creates only main; the default still proves all three native lifetimes.
        let expected = match scenario {
            sonicterm_app::app::RuntimeSmokeScenario::Default => 3,
            sonicterm_app::app::RuntimeSmokeScenario::FrameValidation => 1,
            sonicterm_app::app::RuntimeSmokeScenario::DeviceRecovery => 2,
        };
        if self.registrations != expected
            || self.revocations != expected
            || self.live != 0
            || self.failures != 0
        {
            // When: registrations, revocations, live or failures differ from the required lifecycle, the smoke cannot credit cleanup.
            return Err(format!("native drop-target lifecycle incomplete: {self:?}"));
        }
        Ok(())
    }
}

/// Keeps each successful native drop target's window alive until OLE revocation.
pub struct WinOsTabDragBackend {
    registered_windows: HashMap<WindowId, RegisteredWindow>,
    report: Arc<Mutex<DropRegistrationReport>>,
}

impl WinOsTabDragBackend {
    fn new() -> Self {
        Self {
            registered_windows: HashMap::new(),
            report: Arc::new(Mutex::new(DropRegistrationReport::default())),
        }
    }

    /// Construct a backend on the thread that owns its OLE initialization.
    /// # Safety
    /// The caller keeps that thread's OleGuard alive until the backend and all registered windows are released.
    // SAFETY: the caller retains the owning thread's OleGuard through WinOsTabDragBackend destruction.
    pub unsafe fn boxed() -> Box<dyn OsTabDragBackend> {
        Box::new(Self::new())
    }

    /// Construct the production backend with a report that outlives its native cleanup.
    /// # Safety
    /// The caller keeps that thread's OleGuard alive until the backend and all registered windows are released.
    // SAFETY: the caller retains the owning thread's OleGuard until native registrations and this backend are released.
    pub(crate) unsafe fn boxed_for_smoke(
    ) -> (Box<dyn OsTabDragBackend>, Arc<Mutex<DropRegistrationReport>>) {
        let backend = Self::new();
        let report = backend.report.clone();
        (Box::new(backend), report)
    }

    fn record_failure(&self) {
        self.report.lock().unwrap_or_else(|error| error.into_inner()).failures += 1;
    }

    fn record_revocation(&self) {
        let mut report = self.report.lock().unwrap_or_else(|error| error.into_inner());
        report.revocations += 1;
        report.live -= 1;
    }
}

// Lifecycle: WinOsTabDragBackend revokes its native registrations before releasing each retained window on the OLE thread.
impl Drop for WinOsTabDragBackend {
    fn drop(&mut self) {
        let registered = std::mem::take(&mut self.registered_windows);
        for (window_id, registration) in registered {
            let hwnd = HWND(registration.hwnd as *mut _);
            let revoked =
                // SAFETY: registration.window retains hwnd; the constructor requires its same-thread OleGuard to outlive this backend.
                unsafe { crate::os_drag_win::unregister_for_window(hwnd) };
            match revoked {
                Ok(()) => self.record_revocation(),
                Err(error) => {
                    // Failed revocation retains failure evidence instead of reporting native cleanup success.
                    self.record_failure();
                    tracing::error!(?window_id, %error, "native drop-target teardown failed");
                }
            }
            drop(registration.window);
        }
    }
}

fn unresolved_drag_outcome(
    hr: windows::core::HRESULT,
    effect: u32,
    cursor_position: impl FnOnce() -> (i32, i32),
) -> DragOutcome {
    if hr != windows::Win32::Foundation::DRAGDROP_S_DROP
        || effect == windows::Win32::System::Ole::DROPEFFECT_MOVE.0
    {
        // When: hr cancels or effect is MOVE without a destination, preserve the captured source tab.
        return DragOutcome::Cancelled;
    }
    DragOutcome::DroppedOnEmpty { drop_screen_pos: cursor_position() }
}

impl OsTabDragBackend for WinOsTabDragBackend {
    fn owns_native_drop_target(&self) -> bool {
        true
    }

    fn handles_full_gesture(&self) -> bool {
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
        if payload_json.is_empty() {
            // When: payload_json is absent, no destination can validate the same-process transfer.
            handle.post_drag_ended(DragOutcome::Cancelled);
            return;
        }
        tracing::info!(
            ?source_window,
            source_tab_idx,
            image_bytes = drag_image_png.len(),
            payload_bytes = payload_json.len(),
            "native tab drag started"
        );
        crate::os_drag_win::install_drop_outcome_handle(handle.clone(), &payload_json);
        let outcome =
            // SAFETY: backend construction requires its same-thread OleGuard to remain live during the modal native gesture.
            unsafe { crate::os_drag_win::begin_tab_drag(&payload_json) };
        crate::os_drag_win::clear_drop_outcome_handle();
        if handle.pending_handle().peek_ended().is_some() {
            // When: peek_ended contains a destination or rejection, preserve it instead of reinterpreting the coarse OLE effect.
            return;
        }
        let outcome = unresolved_drag_outcome(outcome.hr, outcome.effect, || {
            let mut point = windows::Win32::Foundation::POINT::default();
            // SAFETY: GetCursorPos writes only the live stack POINT and retains no pointer.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point);
            }
            (point.x, point.y)
        });
        handle.post_drag_ended(outcome);
    }

    fn register_window(
        &mut self,
        _handle: AppHandle,
        window_id: WindowId,
        window: &Arc<Window>,
    ) -> Result<(), String> {
        if let Some(registered) = self.registered_windows.get(&window_id) {
            // When: window_id already owns this same Arc, registration is idempotent without a second native call.
            if Arc::ptr_eq(&registered.window, window) {
                // When: ptr_eq confirms existing window custody, no duplicate RegisterDragDrop call is necessary.
                return Ok(());
            }
            self.record_failure();
            return Err("native drop-target window identity changed".to_owned());
        }
        let raw = window.window_handle().map_err(|error| {
            self.record_failure();
            format!("native drop target has no window handle: {error}")
        })?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            // When: raw does not identify an HWND, this backend cannot own the native target.
            self.record_failure();
            return Err("native drop target requires a Win32 window".to_owned());
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        // SAFETY: window owns this UI-thread HWND; successful registration stores its Arc before returning.
        unsafe { crate::os_drag_win::register_for_window(hwnd, window_id) }.map_err(|error| {
            self.record_failure();
            format!("RegisterDragDrop failed for {window_id:?}: {error}")
        })?;
        self.registered_windows.insert(
            window_id,
            RegisteredWindow { window: window.clone(), hwnd: raw.hwnd.get() as usize },
        );
        let mut report = self.report.lock().unwrap_or_else(|error| error.into_inner());
        report.registrations += 1;
        report.live += 1;
        tracing::info!(?window_id, "native drop target registered");
        Ok(())
    }

    fn unregister_window(&mut self, window_id: WindowId) -> Result<(), String> {
        let Some(registered) = self.registered_windows.get(&window_id) else {
            // When: window_id never registered successfully, no native target belongs to this backend to revoke.
            return Ok(());
        };
        let hwnd = HWND(registered.hwnd as *mut _);
        // SAFETY: registered.window pins this same-thread HWND until native revocation succeeds.
        unsafe { crate::os_drag_win::unregister_for_window(hwnd) }.map_err(|error| {
            self.record_failure();
            format!("RevokeDragDrop failed for {window_id:?}: {error}")
        })?;
        self.registered_windows.remove(&window_id);
        self.record_revocation();
        tracing::info!(?window_id, "native drop target revoked");
        Ok(())
    }
}

#[cfg(test)]
#[path = "tab_drag_os_tests.rs"]
mod tab_drag_os_tests;
