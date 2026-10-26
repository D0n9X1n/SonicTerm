//! Windows: Tool Help snapshots for processes and threads, `GetProcessMemoryInfo` per process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{CloseHandle, LocalFree, FILETIME, HANDLE, HLOCAL, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, GetThreadDescription, OpenProcess, OpenThread,
    WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    THREAD_QUERY_LIMITED_INFORMATION,
};

use super::{group_thread_names, MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary};

/// A kernel handle closed when this owner drops.
struct OwnedHandle(HANDLE);

// Lifecycle: dropping `OwnedHandle` calls `CloseHandle` on its kernel handle exactly once.
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ =
            // SAFETY: `self.0` is a handle this owner opened and has not closed.
            unsafe { CloseHandle(self.0) };
    }
}

/// Decode a NUL-terminated UTF-16 buffer.
fn wide(chars: &[u16]) -> String {
    let end = chars.iter().position(|unit| *unit == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

/// Open `pid` for limited queries, or `None` when it is gone or protected.
fn open_process(pid: u32) -> Option<OwnedHandle> {
    let handle =
        // SAFETY: `OpenProcess` takes only plain values and returns an owned handle on success.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    Some(OwnedHandle(handle))
}

/// Whether `pid` has exited although some open handle still keeps its process object alive.
///
/// Tool Help keeps listing such a process, so a listed pid is checked here: a process handle
/// is signaled once the process exits. The exit code cannot tell, since `STILL_ACTIVE` (259)
/// is also a legal exit code. A process this one cannot open for `PROCESS_SYNCHRONIZE`, such
/// as a protected one, is not known to have exited and stays listed.
fn has_exited(pid: u32) -> bool {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let opened =
        // SAFETY: `OpenProcess` takes only plain values and returns an owned handle on success.
        unsafe { OpenProcess(access, false, pid) };
    let Ok(raw) = opened else {
        // When: `opened` failed, the process is gone from the table or protected; keep it.
        return false;
    };
    let handle = OwnedHandle(raw);
    let waited =
        // SAFETY: `handle.0` is a live process handle opened with `PROCESS_SYNCHRONIZE`, and a
        // zero timeout only polls its signaled state; `handle` closes it on every path.
        unsafe { WaitForSingleObject(handle.0, 0) };
    waited == WAIT_OBJECT_0
}

/// The creation `FILETIME` of `handle`'s process as one 64-bit token.
fn creation_token(handle: HANDLE) -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `handle` is a live process handle and all four out-parameters are writable.
    unsafe { GetProcessTimes(handle, &mut creation, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// One process's extended memory counters.
fn counters(handle: HANDLE) -> Option<PROCESS_MEMORY_COUNTERS_EX> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?,
        ..Default::default()
    };
    // SAFETY: `counters` is correctly sized and aligned, and `cb` bounds the base-structure cast.
    unsafe {
        GetProcessMemoryInfo(
            handle,
            std::ptr::addr_of_mut!(counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    }
    .ok()?;
    Some(counters)
}

/// One process-table snapshot: parent pid and executable name per pid.
pub(super) struct Table {
    entries: HashMap<u32, (u32, String)>,
}

impl ProcessTable for Table {
    fn name(&self, pid: u32) -> Option<String> {
        let (_, name) = self.entries.get(&pid)?;
        if has_exited(pid) {
            // When: `has_exited(pid)` holds, only a lingering handle keeps it listed; absent.
            return None;
        }
        Some(name.clone())
    }

    fn start_token(&self, pid: u32) -> Option<u64> {
        let handle = open_process(pid)?;
        creation_token(handle.0)
    }

    fn children(&self, pid: u32, limit: usize) -> Vec<u32> {
        // A parent pid can outlive its process and be reused, so a listed child counts only
        // when it started after the parent did.
        let parent_start = self.start_token(pid);
        self.entries
            .iter()
            .filter(|(child, (parent, _))| *parent == pid && **child != pid)
            .map(|(child, _)| *child)
            .filter(|child| match (parent_start, self.start_token(*child)) {
                (Some(parent), Some(started)) => started >= parent,
                _ => true,
            })
            .take(limit)
            .collect()
    }

    fn memory(&self, pid: u32) -> MemoryMetric {
        open_process(pid)
            .and_then(|handle| counters(handle.0))
            .map_or(MemoryMetric::Unsupported, |counters| {
                MemoryMetric::Bytes(counters.PrivateUsage as u64)
            })
    }
}

/// Take one process snapshot, or `None` when Tool Help refuses or `cancel` is set mid-read.
///
/// The map holds one entry per process on the system, which the parent lookup needs; the
/// copy is bounded by the snapshot itself and stops as soon as `cancel` is set.
// Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
pub(super) fn table(cancel: &AtomicBool) -> Option<Table> {
    let raw =
        // SAFETY: `CreateToolhelp32Snapshot` takes plain values and returns an owned handle on success.
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
    let snapshot = OwnedHandle(raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).ok()?,
        ..Default::default()
    };
    let mut entries = HashMap::new();
    let mut more =
        // SAFETY: `snapshot` is live and `entry.dwSize` describes `entry`.
        unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
    while more {
        if cancel.load(Ordering::Relaxed) {
            // When: `cancel` is set, a newer refresh superseded this snapshot, so stop reading it.
            return None;
        }
        entries.insert(entry.th32ProcessID, (entry.th32ParentProcessID, wide(&entry.szExeFile)));
        more =
            // SAFETY: as above; `Process32NextW` fails once the snapshot is exhausted.
            unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
    }
    Some(Table { entries })
}

/// Private commit and its peak, the figures Task Manager's Commit size column reflects.
pub(super) fn footprint() -> ProcessFootprint {
    let counters = crate::process_memory::windows_counters();
    ProcessFootprint {
        current: counters.map_or(MemoryMetric::Unsupported, |counters| {
            MemoryMetric::Bytes(counters.PrivateUsage as u64)
        }),
        current_label: "private committed",
        peak: counters.map_or(MemoryMetric::Unsupported, |counters| {
            MemoryMetric::Bytes(counters.PeakPagefileUsage as u64)
        }),
        peak_label: "peak commit",
    }
}

/// Seconds since this process's creation time.
pub(super) fn uptime_secs() -> Option<u64> {
    let pseudo_handle =
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing.
        unsafe { GetCurrentProcess() };
    let created = creation_token(pseudo_handle)?;
    // FILETIME counts 100 ns intervals since 1601; the Unix epoch is this many intervals later.
    const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;
    let created_secs = created.checked_sub(UNIX_EPOCH_FILETIME)? / 10_000_000;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
    now.as_secs().checked_sub(created_secs)
}

/// User plus kernel CPU time of every thread of this process, from `GetProcessTimes`.
pub(super) fn cpu_time() -> Option<std::time::Duration> {
    let mut creation = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `GetCurrentProcess` is a pseudo-handle needing no close; all four out-parameters are writable.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exited, &mut kernel, &mut user)
    }
    .ok()?;
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    // FILETIME counts 100 ns ticks.
    let total = ticks(kernel).checked_add(ticks(user))?;
    Some(std::time::Duration::from_nanos(total.checked_mul(100)?))
}

/// One thread's description, set by `SetThreadDescription`, if any.
fn thread_name(thread_id: u32) -> Option<String> {
    let raw =
        // SAFETY: `OpenThread` takes plain values and returns an owned handle on success.
        unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, thread_id) }.ok()?;
    let handle = OwnedHandle(raw);
    let description =
        // SAFETY: `handle` is a live thread handle; the returned string is freed below.
        unsafe { GetThreadDescription(handle.0) }.ok()?;
    let name =
        // SAFETY: `description` is a NUL-terminated wide string the call allocated.
        unsafe { description.to_string() }.ok();
    let _ =
        // SAFETY: `description` was allocated with LocalAlloc by `GetThreadDescription` and is freed once.
        unsafe { LocalFree(Some(HLOCAL(description.0.cast()))) };
    name
}

/// List this process's threads from one Tool Help thread snapshot.
///
/// A set `cancel` stops the snapshot read or the naming and reports no thread list.
// Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
pub(super) fn threads(cancel: &AtomicBool) -> ThreadSummary {
    let unknown = ThreadSummary { total: None, names: Vec::new() };
    let snapshot_result =
        // SAFETY: `CreateToolhelp32Snapshot` takes plain values and returns an owned handle on success.
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    let Ok(raw) = snapshot_result else {
        // When: `snapshot_result` is an error, Tool Help refused the snapshot, so no thread can be listed.
        return unknown;
    };
    let snapshot = OwnedHandle(raw);
    let Ok(size) = u32::try_from(std::mem::size_of::<THREADENTRY32>()) else {
        // When: the entry size does not fit a u32, the snapshot cannot be read.
        return unknown;
    };
    let own_pid = std::process::id();
    let mut entry = THREADENTRY32 { dwSize: size, ..Default::default() };
    let mut ids = Vec::new();
    let mut more =
        // SAFETY: `snapshot` is live and `entry.dwSize` describes `entry`.
        unsafe { Thread32First(snapshot.0, &mut entry) }.is_ok();
    while more {
        if cancel.load(Ordering::Relaxed) {
            // When: `cancel` is set, a newer refresh superseded this listing, so stop reading it.
            return unknown;
        }
        if entry.th32OwnerProcessID == own_pid {
            ids.push(entry.th32ThreadID);
        }
        more =
            // SAFETY: as above; `Thread32Next` fails once the snapshot is exhausted.
            unsafe { Thread32Next(snapshot.0, &mut entry) }.is_ok();
    }
    let mut names = Vec::with_capacity(ids.len());
    for id in &ids {
        if cancel.load(Ordering::Relaxed) {
            // When: `cancel` is set, a newer refresh superseded this listing, so stop naming threads.
            return unknown;
        }
        names.push(thread_name(*id));
    }
    ThreadSummary { total: Some(ids.len()), names: group_thread_names(names) }
}
