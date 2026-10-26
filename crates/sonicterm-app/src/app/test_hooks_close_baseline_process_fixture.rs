//! Windows retained-process inspection, admission and cleanup custody.

use super::*;

#[cfg(windows)]
pub(super) struct NativeProcess {
    pub(super) pid: u32,
    pub(super) role: &'static str,
    pub(super) handle: std::os::windows::io::OwnedHandle,
    pub(super) created: u64,
}

/// Inspection owns only handle closure. Refusal and query errors never acquire termination custody.
#[cfg(windows)]
pub(super) struct ProcessInspection {
    pub(super) pid: u32,
    pub(super) handle: std::os::windows::io::OwnedHandle,
    pub(super) created: u64,
}

#[cfg(windows)]
impl ProcessInspection {
    pub(super) fn open(pid: u32) -> Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::{Foundation::FILETIME, System::Threading::*};
        let handle =
            // SAFETY: pid is queried by value; the returned handle is immediately placed in a close-only RAII owner.
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? };
        let owned =
            // SAFETY: handle is a newly owned process handle, transferred exactly once to OwnedHandle.
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) };
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: owned keeps handle open; every output points to writable FILETIME storage.
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)? };
        Ok(Self { pid, handle: owned, created: filetime_value(created) })
    }

    pub(super) fn into_owned(self, role: &'static str) -> NativeProcess {
        NativeProcess { pid: self.pid, role, handle: self.handle, created: self.created }
    }
}

#[cfg(windows)]
pub(super) fn filetime_value(time: windows::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[cfg(windows)]
pub(super) fn fixture_birth_boundary() -> u64 {
    let now =
        // SAFETY: this value-only query returns the current FILETIME in the same units as process creation times.
        unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
    filetime_value(now)
}

#[cfg(windows)]
impl NativeProcess {
    /// Only directly spawned fixture processes use this entry; discovered candidates must pass admission first.
    pub(super) fn open(pid: u32, role: &'static str) -> Result<Self> {
        Ok(ProcessInspection::open(pid)?.into_owned(role))
    }

    pub(super) fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        windows::Win32::Foundation::HANDLE(self.handle.as_raw_handle())
    }

    pub(super) fn running(&self) -> Result<bool> {
        use windows::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        let state =
            // SAFETY: this retained handle pins the process identity; a zero timeout only observes its state.
            unsafe { WaitForSingleObject(self.raw(), 0) };
        match state {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => Ok(true),
            _ => Err(io::Error::last_os_error().into()),
        }
    }

    pub(super) fn observe(&self) -> Result<(bool, &'static str)> {
        let running = self.running()?;
        Ok((!running, if running { "running" } else { "signaled" }))
    }
    pub(super) fn settled(&self) -> Result<bool> {
        Ok(self.observe()?.0)
    }
    pub(super) fn identity(&self) -> String {
        self.created.to_string()
    }
    pub(super) fn observation(&self) -> &'static str {
        "signaled_handle"
    }

    pub(super) fn terminate_if_running(&self) -> Result<()> {
        if self.running()? {
            let result =
                // SAFETY: the handle belongs to this fixture process and remains retained across termination.
                unsafe { windows::Win32::System::Threading::TerminateProcess(self.raw(), 1) };
            if self.running()? {
                result?;
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub(super) struct ProcessEntry {
    pub(super) pid: u32,
    pub(super) parent: u32,
    pub(super) name: String,
}

/// Validate discovered ownership before moving the same close-only handle into termination custody.
#[cfg(windows)]
pub(super) fn admit_process_candidate(
    candidate: &ProcessEntry,
    inspection: ProcessInspection,
    expected_parent: u32,
    not_before: u64,
    preexisting: &BTreeSet<u32>,
    role: &'static str,
) -> Result<Option<NativeProcess>> {
    if candidate.pid != inspection.pid
        || candidate.parent != expected_parent
        || preexisting.contains(&candidate.pid)
    {
        return Ok(None);
    }
    if !candidate_has_owned_origin(
        candidate,
        expected_parent,
        inspection.created,
        not_before,
        preexisting,
    ) {
        return Ok(None);
    }
    // Keep the inspected handle alive across the table recheck and transfer this handle, never a PID reopen.
    let current = process_snapshot()?;
    let Some(confirmed) = current.iter().find(|entry| entry.pid == inspection.pid) else {
        return Ok(None);
    };
    if confirmed.parent != expected_parent || !confirmed.name.eq_ignore_ascii_case(&candidate.name)
    {
        return Ok(None);
    }
    Ok(Some(inspection.into_owned(role)))
}

/// Metadata-only discovered-process admission predicate; no process is opened by these checks.
#[cfg(windows)]
pub(super) fn candidate_has_owned_origin(
    candidate: &ProcessEntry,
    expected_parent: u32,
    created: u64,
    not_before: u64,
    preexisting: &BTreeSet<u32>,
) -> bool {
    candidate.parent == expected_parent
        && !preexisting.contains(&candidate.pid)
        && created >= not_before
}

#[cfg(windows)]
pub(super) fn process_snapshot() -> Result<Vec<ProcessEntry>> {
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::{Foundation::ERROR_NO_MORE_FILES, System::Diagnostics::ToolHelp::*};
    let snapshot =
        // SAFETY: snapshot takes only flags; its returned handle is immediately given one RAII owner.
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? };
    let _owned =
        // SAFETY: snapshot is a valid owned handle transferred from CreateToolhelp32Snapshot.
        unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(snapshot.0) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: entry advertises its writable size and snapshot remains open through enumeration.
    unsafe { Process32FirstW(snapshot, &mut entry)? };
    let mut entries = Vec::new();
    loop {
        let end =
            entry.szExeFile.iter().position(|value| *value == 0).unwrap_or(entry.szExeFile.len());
        entries.push(ProcessEntry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&entry.szExeFile[..end]),
        });
        ensure!(entries.len() < 65536, "process snapshot exceeded fixture bound");
        let next =
            // SAFETY: entry remains sized writable storage and the snapshot guard has not been dropped.
            unsafe { Process32NextW(snapshot, &mut entry) };
        match next {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(entries),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
#[path = "test_hooks_close_baseline_process_fixture_tests.rs"]
mod test_hooks_close_baseline_process_fixture_tests;
