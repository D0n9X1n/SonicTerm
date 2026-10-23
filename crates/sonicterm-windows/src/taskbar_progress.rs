//! `OSC 9;4` progress on each window's taskbar button through `ITaskbarList3`.
//!
//! The list is created on first use on the event-loop thread, which production initializes for
//! OLE before the shell runs. A failure is logged once and later reports are dropped.

use sonicterm_app::taskbar_progress::{
    ProgressReport, ProgressState, ProgressSurface, TaskbarProgressBackend,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    ITaskbarList3, TaskbarList, TBPFLAG, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS,
    TBPF_NORMAL, TBPF_PAUSED,
};

/// The taskbar buttons of SonicTerm's windows.
#[derive(Default)]
pub(crate) struct TaskbarButtons {
    list: Option<ITaskbarList3>,
    failed: bool,
}

impl TaskbarButtons {
    /// The taskbar list, created once; `None` after a creation failure.
    fn list(&mut self) -> Option<&ITaskbarList3> {
        if self.list.is_none() && !self.failed {
            // When: list is None and creation has not failed, the taskbar list is created now.
            let created: windows::core::Result<ITaskbarList3> =
                // SAFETY: `TaskbarList` is the CLSID for `ITaskbarList3`; this runs on the
                // event-loop thread that production initialized for OLE.
                unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
            let initialized = created.and_then(|list| {
                // SAFETY: `HrInit` is called once on the list just created, before any other method.
                unsafe { list.HrInit() }.map(|()| list)
            });
            match initialized {
                Ok(list) => self.list = Some(list),
                Err(error) => {
                    self.failed = true;
                    tracing::warn!(%error, "taskbar progress unavailable");
                }
            }
        }
        self.list.as_ref()
    }
}

/// The taskbar state for `report`.
fn taskbar_state(report: Option<ProgressReport>) -> TBPFLAG {
    match report.map(|report| report.state) {
        None => TBPF_NOPROGRESS,
        Some(ProgressState::Normal) => TBPF_NORMAL,
        Some(ProgressState::Error) => TBPF_ERROR,
        Some(ProgressState::Indeterminate) => TBPF_INDETERMINATE,
        Some(ProgressState::Paused) => TBPF_PAUSED,
    }
}

impl TaskbarProgressBackend for TaskbarButtons {
    fn per_window(&self) -> bool {
        true
    }

    fn deliver(&mut self, surface: ProgressSurface, report: Option<ProgressReport>) {
        let ProgressSurface::Window(native) = surface else {
            // When: surface is the app-wide one, Windows has no such surface.
            return;
        };
        let Some(list) = self.list() else {
            // When: the taskbar list could not be created, progress is not shown.
            return;
        };
        let hwnd = HWND(native as *mut _);
        let state = taskbar_state(report);
        // The value goes first so a paused or error state keeps the reported fraction.
        let value = report
            .filter(|report| report.state != ProgressState::Indeterminate)
            .map(|report| u64::from(report.percent));
        let result =
            // SAFETY: `hwnd` is a top-level handle the App read from its winit window; a destroyed
            // handle makes the call fail, which is logged and ignored.
            unsafe {
            value
                .map_or(Ok(()), |percent| list.SetProgressValue(hwnd, percent, 100))
                .and_then(|()| list.SetProgressState(hwnd, state))
        };
        if let Err(error) = result {
            // A window closing during delivery fails here; the next value tries again.
            tracing::debug!(%error, "taskbar progress update failed");
        }
    }
}
