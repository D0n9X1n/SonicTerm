//! OS figures for the Memory Usage report: this process's footprint and peak,
//! its threads, and each pane shell's descendant processes.
//!
//! [`super::process_memory`] answers the periodic log line's question with
//! three fixed figures. The report asks two more: what number the operating
//! system's own monitor shows for this process (and its peak), and how much
//! memory each pane's shell tree holds outside SonicTerm. Each figure carries
//! a label naming what the OS measured, because "footprint", "commit" and
//! "resident" are different quantities and a report that called them all
//! "memory" would invite comparisons that do not hold.
//!
//! Every query here is bounded: a descendant walk stops at a caller-supplied
//! limit, checks a cancel flag between processes, and treats a process that
//! exits mid-walk as absent rather than as an error.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::process_memory::MemoryMetric;

/// Most descendants the report lists under one pane's shell.
pub const MAX_DESCENDANTS_PER_ROOT: usize = 64;

/// What the OS monitor shows for this process, with labels naming each figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFootprint {
    /// What the platform calls this process's memory: macOS physical footprint,
    /// Windows private commit, Linux resident set.
    pub current: MemoryMetric,
    /// Label naming exactly what [`Self::current`] measures.
    pub current_label: &'static str,
    /// The highest value [`Self::current`] (or its nearest OS equivalent) reached.
    pub peak: MemoryMetric,
    /// Label naming exactly what [`Self::peak`] measures.
    pub peak_label: &'static str,
}

/// Label for the per-process figure in a descendant row, naming what the OS measured.
#[cfg(target_os = "macos")]
pub const CHILD_MEMORY_LABEL: &str = "footprint";
/// Label for the per-process figure in a descendant row, naming what the OS measured.
#[cfg(windows)]
pub const CHILD_MEMORY_LABEL: &str = "private";
/// Label for the per-process figure in a descendant row, naming what the OS measured.
#[cfg(not(any(target_os = "macos", windows)))]
pub const CHILD_MEMORY_LABEL: &str = "RSS";

/// Read this process's footprint and peak from the OS.
///
/// Fixed cost; never walks the address space. A failed query reports
/// [`MemoryMetric::Unsupported`] for the figure it could not read.
#[must_use]
pub fn sample_footprint() -> ProcessFootprint {
    platform::footprint()
}

/// The current local wall-clock time, formatted for a report header (`2026-10-17 12:22:23 -07:00`).
#[must_use]
pub fn local_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string()
}

/// Seconds since this process started, or `None` when the OS does not say.
#[must_use]
pub fn process_uptime_secs() -> Option<u64> {
    platform::uptime_secs()
}

/// One process under a pane's shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProcess {
    /// Operating-system process id.
    pub pid: u32,
    /// Distance from the root: 0 is the shell itself, 1 its child, and so on.
    pub depth: usize,
    /// Executable name as the OS reports it; untrusted text.
    pub name: String,
    /// The OS figure named by [`CHILD_MEMORY_LABEL`].
    pub memory: MemoryMetric,
}

/// How a descendant walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkOutcome {
    /// Every descendant reachable at sampling time was listed.
    Complete,
    /// The walk reached its descendant limit; more processes may exist.
    Truncated,
    /// The root pid no longer names the shell it was taken for.
    RootGone,
    /// A newer refresh cancelled this walk.
    Cancelled,
    /// This platform has no process-table query.
    Unsupported,
}

/// A pane shell and its descendants, in depth-first order with the shell first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTree {
    /// The shell (depth 0) followed by its descendants (depth 1 and below).
    pub processes: Vec<ChildProcess>,
    /// Why the list ended where it did.
    pub outcome: WalkOutcome,
}

/// The process table a descendant walk reads, abstracted so the walk is testable.
pub trait ProcessTable {
    /// The process's executable name, or `None` when it no longer exists.
    fn name(&self, pid: u32) -> Option<String>;
    /// The process's start token in the format `ProcessIdentity::start` uses, if readable.
    fn start_token(&self, pid: u32) -> Option<u64>;
    /// Direct children of `pid`; empty when it has none or cannot be read.
    fn children(&self, pid: u32) -> Vec<u32>;
    /// The figure named by [`CHILD_MEMORY_LABEL`].
    fn memory(&self, pid: u32) -> MemoryMetric;
}

/// Walk `root` and its descendants in `table`, listing at most `limit` descendants.
///
/// `expected_start` is the start token the root had when its pane spawned it. When both it
/// and the live token are known and they differ, the pid now names another process and the
/// walk reports [`WalkOutcome::RootGone`] instead of describing a stranger's tree.
// Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
pub fn walk_tree(
    table: &dyn ProcessTable,
    root: u32,
    expected_start: Option<u64>,
    limit: usize,
    cancel: &AtomicBool,
) -> ProcessTree {
    let gone = ProcessTree { processes: Vec::new(), outcome: WalkOutcome::RootGone };
    let Some(root_name) = table.name(root) else {
        // When: `table.name(root)` finds no process, the shell exited before the sample.
        return gone;
    };
    if let (Some(expected), Some(live)) = (expected_start, table.start_token(root)) {
        // When: `expected` and `live` start tokens are both known, a mismatch proves the pid was reused.
        if expected != live {
            // When: `expected` differs from `live`, the pid names a different process now.
            return gone;
        }
    }
    let mut processes =
        vec![ChildProcess { pid: root, depth: 0, name: root_name, memory: table.memory(root) }];
    let mut visited = HashSet::from([root]);
    let mut stack: Vec<(u32, usize)> =
        sorted_children(table, root).into_iter().rev().map(|pid| (pid, 1)).collect();
    let mut outcome = WalkOutcome::Complete;
    while let Some((pid, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            // When: `cancel` is set, a newer refresh superseded this walk, so stop at once.
            outcome = WalkOutcome::Cancelled;
            break;
        }
        if !visited.insert(pid) {
            // When: `visited` already holds `pid`, a reused pid formed a cycle; list it once.
            continue;
        }
        if processes.len() > limit {
            // When: `processes` already holds `limit` descendants after the root, stop listing.
            outcome = WalkOutcome::Truncated;
            break;
        }
        let Some(name) = table.name(pid) else {
            // When: `table.name(pid)` is None, the child exited between listing and sampling.
            continue;
        };
        processes.push(ChildProcess { pid, depth, name, memory: table.memory(pid) });
        for child in sorted_children(table, pid).into_iter().rev() {
            stack.push((child, depth.saturating_add(1)));
        }
    }
    ProcessTree { processes, outcome }
}

/// Children of `pid` in ascending pid order, so a walk lists siblings deterministically.
fn sorted_children(table: &dyn ProcessTable, pid: u32) -> Vec<u32> {
    let mut children = table.children(pid);
    children.retain(|child| *child != pid);
    children.sort_unstable();
    children.dedup();
    children
}

/// Walk each root's descendants in the live OS process table.
///
/// Each root is `(pid, expected start token)`. One process-table snapshot serves every root
/// on platforms that take one, so a refresh costs one snapshot rather than one per pane.
#[must_use]
pub fn sample_trees(roots: &[(u32, Option<u64>)], cancel: &AtomicBool) -> Vec<ProcessTree> {
    let Some(table) = platform::table() else {
        // When: `platform::table` is None, this platform cannot enumerate processes.
        return roots
            .iter()
            .map(|_| ProcessTree { processes: Vec::new(), outcome: WalkOutcome::Unsupported })
            .collect();
    };
    roots
        .iter()
        .map(|(pid, start)| walk_tree(&table, *pid, *start, MAX_DESCENDANTS_PER_ROOT, cancel))
        .collect()
}

/// One OS process's name and memory figure, or `None` when it does not exist.
#[must_use]
pub fn sample_process(pid: u32) -> Option<ChildProcess> {
    let table = platform::table()?;
    let name = table.name(pid)?;
    Some(ChildProcess { pid, depth: 0, name, memory: table.memory(pid) })
}

/// This process's threads, grouped by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    /// Threads the OS reports for this process; `None` when it cannot count them.
    pub total: Option<usize>,
    /// `(name, count)` sorted by descending count, then name; unnamed threads are grouped.
    pub names: Vec<(String, usize)>,
}

/// Group thread names into `(name, count)` pairs, most numerous first.
#[must_use]
pub fn group_thread_names(names: impl IntoIterator<Item = Option<String>>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for name in names {
        let key = name.filter(|text| !text.is_empty()).unwrap_or_else(|| "(unnamed)".to_string());
        *counts.entry(key).or_default() += 1;
    }
    let mut grouped: Vec<(String, usize)> = counts.into_iter().collect();
    grouped.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    grouped
}

/// List this process's threads and their names.
#[must_use]
pub fn sample_threads() -> ThreadSummary {
    platform::threads()
}

#[cfg(target_os = "macos")]
mod platform {
    //! macOS: `libproc` (`proc_pid_rusage`, `proc_pidinfo`, `proc_listchildpids`).

    use super::{group_thread_names, MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary};

    /// `PROC_PIDLISTTHREADS` from `<sys/proc_info.h>`; `libc` exports the per-thread flavor only.
    const PROC_PIDLISTTHREADS: libc::c_int = 6;
    /// Most child pids one `proc_listchildpids` call returns; the walk lists far fewer.
    const MAX_CHILD_PIDS: usize = 512;

    /// Read `rusage_info_v4` for `pid`, which carries the physical footprint and its peak.
    fn rusage(pid: u32) -> Option<libc::rusage_info_v4> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut info: libc::rusage_info_v4 =
            // SAFETY: `rusage_info_v4` is a plain C struct of integers, so all-zero bytes are valid.
            unsafe { std::mem::zeroed() };
        let status =
            // SAFETY: `proc_pid_rusage` writes one `rusage_info_v4` for flavor V4 through the pointer, which addresses `info`.
            unsafe {
                libc::proc_pid_rusage(
                    pid,
                    libc::RUSAGE_INFO_V4,
                    std::ptr::addr_of_mut!(info).cast::<libc::rusage_info_t>(),
                )
            };
        (status == 0).then_some(info)
    }

    /// Read `proc_bsdinfo` for `pid`: its name and start time.
    fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut info: libc::proc_bsdinfo =
            // SAFETY: `proc_bsdinfo` is a plain C struct of integers and arrays, so all-zero bytes are valid.
            unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        let written =
            // SAFETY: `proc_pidinfo` writes at most `size` bytes into `info`, which is that size.
            unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    std::ptr::addr_of_mut!(info).cast::<libc::c_void>(),
                    size,
                )
            };
        (written == size).then_some(info)
    }

    /// Decode a NUL-terminated C character array.
    fn c_chars(chars: &[libc::c_char]) -> String {
        let bytes: Vec<u8> =
            chars.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The live macOS process table, queried per pid.
    pub(super) struct Table;

    impl ProcessTable for Table {
        fn name(&self, pid: u32) -> Option<String> {
            let info = bsd_info(pid)?;
            let long_name = c_chars(&info.pbi_name);
            if long_name.is_empty() {
                // When: `long_name` is empty, the kernel kept only the short `pbi_comm` name.
                return Some(c_chars(&info.pbi_comm));
            }
            Some(long_name)
        }

        fn start_token(&self, pid: u32) -> Option<u64> {
            let info = bsd_info(pid)?;
            Some(
                info.pbi_start_tvsec
                    .saturating_mul(1_000_000)
                    .saturating_add(info.pbi_start_tvusec),
            )
        }

        fn children(&self, pid: u32) -> Vec<u32> {
            let Ok(pid) = libc::pid_t::try_from(pid) else {
                // When: `pid` does not fit a `pid_t`, the kernel cannot be asked about it.
                return Vec::new();
            };
            let mut buffer = vec![0 as libc::pid_t; MAX_CHILD_PIDS];
            let Ok(buffer_bytes) =
                libc::c_int::try_from(buffer.len() * std::mem::size_of::<libc::pid_t>())
            else {
                // When: the buffer size does not fit a c_int, no query can describe it.
                return Vec::new();
            };
            let returned =
                // SAFETY: `proc_listchildpids` writes at most `buffer_bytes` bytes into `buffer`, which owns that many.
                unsafe {
                    libc::proc_listchildpids(pid, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer_bytes)
                };
            let Ok(returned) = usize::try_from(returned) else {
                // When: `returned` is negative, the query failed and the process has no listable children.
                return Vec::new();
            };
            // The count is clamped to the buffer, and zero entries are dropped, so either reading of
            // the return value (entries or bytes) yields only pids the kernel wrote.
            buffer
                .into_iter()
                .take(returned.min(MAX_CHILD_PIDS))
                .filter_map(|child| u32::try_from(child).ok())
                .filter(|child| *child != 0)
                .collect()
        }

        fn memory(&self, pid: u32) -> MemoryMetric {
            rusage(pid).map_or(MemoryMetric::Unsupported, |info| {
                MemoryMetric::Bytes(info.ri_phys_footprint)
            })
        }
    }

    /// The live table; always available on macOS.
    pub(super) fn table() -> Option<Table> {
        Some(Table)
    }

    /// Physical footprint and its lifetime peak, the figures Activity Monitor shows as Memory.
    pub(super) fn footprint() -> ProcessFootprint {
        let info = rusage(std::process::id());
        ProcessFootprint {
            current: info.map_or(MemoryMetric::Unsupported, |info| {
                MemoryMetric::Bytes(info.ri_phys_footprint)
            }),
            current_label: "physical footprint",
            peak: info.map_or(MemoryMetric::Unsupported, |info| {
                MemoryMetric::Bytes(info.ri_lifetime_max_phys_footprint)
            }),
            peak_label: "lifetime peak footprint",
        }
    }

    /// Seconds since this process's recorded start time.
    pub(super) fn uptime_secs() -> Option<u64> {
        let info = bsd_info(std::process::id())?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
        now.as_secs().checked_sub(info.pbi_start_tvsec)
    }

    /// One thread's name from its kernel thread handle.
    fn thread_name(pid: libc::c_int, handle: u64) -> Option<String> {
        let mut info: libc::proc_threadinfo =
            // SAFETY: `proc_threadinfo` is a plain C struct of integers and a char array, so zero bytes are valid.
            unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_threadinfo>()).ok()?;
        let written =
            // SAFETY: `proc_pidinfo` writes at most `size` bytes into `info`, which is that size.
            unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    handle,
                    std::ptr::addr_of_mut!(info).cast::<libc::c_void>(),
                    size,
                )
            };
        (written == size).then(|| c_chars(&info.pth_name))
    }

    /// List this process's threads by kernel handle and read each one's name.
    pub(super) fn threads() -> ThreadSummary {
        let unknown = ThreadSummary { total: None, names: Vec::new() };
        let Ok(pid) = libc::c_int::try_from(std::process::id()) else {
            // When: this pid does not fit a c_int, no thread query can name it.
            return unknown;
        };
        let Some(task) = crate::process_memory::macos_task_info() else {
            // When: `macos_task_info` failed, the thread count that sizes the list is unknown.
            return unknown;
        };
        // Threads can start between the count and the list, so leave headroom for a few.
        let capacity = usize::try_from(task.pti_threadnum).unwrap_or(0).saturating_add(16);
        let mut handles = vec![0u64; capacity];
        let Ok(buffer_bytes) = libc::c_int::try_from(capacity * std::mem::size_of::<u64>()) else {
            // When: the handle buffer size does not fit a c_int, the list cannot be requested.
            return unknown;
        };
        let written =
            // SAFETY: `proc_pidinfo` writes at most `buffer_bytes` bytes of thread handles into `handles`.
            unsafe {
                libc::proc_pidinfo(
                    pid,
                    PROC_PIDLISTTHREADS,
                    0,
                    handles.as_mut_ptr().cast::<libc::c_void>(),
                    buffer_bytes,
                )
            };
        let Ok(written) = usize::try_from(written) else {
            // When: `written` is negative, the thread list query failed.
            return unknown;
        };
        let listed = (written / std::mem::size_of::<u64>()).min(capacity);
        handles.truncate(listed);
        let names = group_thread_names(handles.iter().map(|handle| thread_name(pid, *handle)));
        ThreadSummary { total: Some(listed), names }
    }
}

#[cfg(windows)]
mod platform {
    //! Windows: Tool Help snapshots for processes and threads, `GetProcessMemoryInfo` per process.

    use std::collections::HashMap;

    use windows::Win32::Foundation::{CloseHandle, LocalFree, FILETIME, HANDLE, HLOCAL};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
        PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, GetThreadDescription, OpenProcess, OpenThread,
        PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
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

    /// The creation `FILETIME` of `handle`'s process as one 64-bit token.
    fn creation_token(handle: HANDLE) -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: `handle` is a live process handle and all four out-parameters are writable.
        unsafe { GetProcessTimes(handle, &mut creation, &mut exited, &mut kernel, &mut user) }
            .ok()?;
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
            self.entries.get(&pid).map(|(_, name)| name.clone())
        }

        fn start_token(&self, pid: u32) -> Option<u64> {
            let handle = open_process(pid)?;
            creation_token(handle.0)
        }

        fn children(&self, pid: u32) -> Vec<u32> {
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

    /// Take one process snapshot, or `None` when Tool Help refuses.
    pub(super) fn table() -> Option<Table> {
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
            entries
                .insert(entry.th32ProcessID, (entry.th32ParentProcessID, wide(&entry.szExeFile)));
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
    pub(super) fn threads() -> ThreadSummary {
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
            if entry.th32OwnerProcessID == own_pid {
                ids.push(entry.th32ThreadID);
            }
            more =
                // SAFETY: as above; `Thread32Next` fails once the snapshot is exhausted.
                unsafe { Thread32Next(snapshot.0, &mut entry) }.is_ok();
        }
        let names = group_thread_names(ids.iter().map(|id| thread_name(*id)));
        ThreadSummary { total: Some(ids.len()), names }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! Linux: `/proc`.

    use super::{group_thread_names, MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary};

    /// A `kB` field from `/proc/<pid>/status`, in bytes.
    fn status_kib(pid: &str, field: &str) -> MemoryMetric {
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            // When: the status file is unreadable, the process is gone or hidden.
            return MemoryMetric::Unsupported;
        };
        status
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .map_or(MemoryMetric::Unsupported, |kib| MemoryMetric::Bytes(kib.saturating_mul(1024)))
    }

    /// The fields of `/proc/<pid>/stat` after the parenthesised command name.
    fn stat_fields(pid: u32) -> Option<Vec<String>> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = &stat[stat.rfind(')')? + 1..];
        Some(after_name.split_whitespace().map(str::to_string).collect())
    }

    /// The live `/proc` table, read per pid.
    pub(super) struct Table;

    impl ProcessTable for Table {
        fn name(&self, pid: u32) -> Option<String> {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            Some(comm.trim_end().to_string())
        }

        fn start_token(&self, pid: u32) -> Option<u64> {
            // Field 22 of stat is start time in clock ticks; index 19 after the name and state.
            stat_fields(pid)?.get(19)?.parse().ok()
        }

        fn children(&self, pid: u32) -> Vec<u32> {
            let mut children = Vec::new();
            let mut listed = false;
            if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
                for task in tasks.flatten() {
                    let path = task.path().join("children");
                    if let Ok(text) = std::fs::read_to_string(path) {
                        listed = true;
                        children.extend(text.split_whitespace().filter_map(|id| id.parse().ok()));
                    }
                }
            }
            if listed {
                // When: some task's `children` file was readable, the kernel listed them directly.
                return children;
            }
            let Ok(entries) = std::fs::read_dir("/proc") else {
                // When: `read_dir` of `/proc` failed into `entries`, no parent scan is possible.
                return children;
            };
            for entry in entries.flatten() {
                let Some(child) =
                    entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok())
                else {
                    // When: the entry name is not numeric, it is not a process directory.
                    continue;
                };
                let parent =
                    stat_fields(child).and_then(|fields| fields.get(1)?.parse::<u32>().ok());
                if parent == Some(pid) {
                    children.push(child);
                }
            }
            children
        }

        fn memory(&self, pid: u32) -> MemoryMetric {
            status_kib(&pid.to_string(), "VmRSS:")
        }
    }

    /// The live table; available whenever `/proc` is mounted.
    pub(super) fn table() -> Option<Table> {
        std::path::Path::new("/proc").is_dir().then_some(Table)
    }

    /// Resident set and its high-water mark.
    pub(super) fn footprint() -> ProcessFootprint {
        ProcessFootprint {
            current: status_kib("self", "VmRSS:"),
            current_label: "resident (RSS)",
            peak: status_kib("self", "VmHWM:"),
            peak_label: "peak RSS",
        }
    }

    /// Seconds since this process started, from its start ticks and the system uptime.
    pub(super) fn uptime_secs() -> Option<u64> {
        let start_ticks: u64 = Table.start_token(std::process::id())?;
        let raw_ticks =
            // SAFETY: `sysconf` reads a system constant and has no memory-safety preconditions.
            unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let ticks_per_sec = u64::try_from(raw_ticks).ok()?;
        let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
        let system_secs =
            uptime.split_whitespace().next()?.split('.').next()?.parse::<u64>().ok()?;
        system_secs.checked_sub(start_ticks / ticks_per_sec.max(1))
    }

    /// Thread names from `/proc/self/task/*/comm`.
    pub(super) fn threads() -> ThreadSummary {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            // When: `read_dir` gave no `tasks`, the task directory is unreadable and threads cannot be listed.
            return ThreadSummary { total: None, names: Vec::new() };
        };
        let names: Vec<Option<String>> = tasks
            .flatten()
            .map(|task| {
                std::fs::read_to_string(task.path().join("comm"))
                    .ok()
                    .map(|name| name.trim_end().to_string())
            })
            .collect();
        ThreadSummary { total: Some(names.len()), names: group_thread_names(names) }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    //! Other platforms: no process-table or footprint query.

    use super::{MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary};

    /// Placeholder table type; never constructed.
    pub(super) struct Table;

    impl ProcessTable for Table {
        fn name(&self, _pid: u32) -> Option<String> {
            None
        }
        fn start_token(&self, _pid: u32) -> Option<u64> {
            None
        }
        fn children(&self, _pid: u32) -> Vec<u32> {
            Vec::new()
        }
        fn memory(&self, _pid: u32) -> MemoryMetric {
            MemoryMetric::Unsupported
        }
    }

    /// No process table on this platform.
    pub(super) fn table() -> Option<Table> {
        None
    }

    /// No footprint query on this platform.
    pub(super) fn footprint() -> ProcessFootprint {
        ProcessFootprint {
            current: MemoryMetric::Unsupported,
            current_label: "footprint",
            peak: MemoryMetric::Unsupported,
            peak_label: "peak",
        }
    }

    /// No uptime query on this platform.
    pub(super) fn uptime_secs() -> Option<u64> {
        None
    }

    /// No thread query on this platform.
    pub(super) fn threads() -> ThreadSummary {
        ThreadSummary { total: None, names: Vec::new() }
    }
}

#[cfg(test)]
#[path = "process_report_tests.rs"]
mod process_report_tests;
