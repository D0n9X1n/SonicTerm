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
    /// The shell's start token was expected but could not be read, so the pid is not trusted.
    Unverified,
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
    /// At most `limit` direct children of `pid`; empty when it has none or cannot be read.
    ///
    /// Implementations stop collecting at `limit`, so a process with thousands of children
    /// costs no more than the walk can list.
    fn children(&self, pid: u32, limit: usize) -> Vec<u32>;
    /// The figure named by [`CHILD_MEMORY_LABEL`].
    fn memory(&self, pid: u32) -> MemoryMetric;
}

/// Check `root` against the start token its pane recorded at spawn.
///
/// `None` means the walk may describe `root`. A recorded token that the live process no longer
/// has means the pid was reused ([`WalkOutcome::RootGone`]); a recorded token that cannot be read
/// now means the pid cannot be trusted ([`WalkOutcome::Unverified`]). Without a recorded token,
/// as on platforms that keep none, the pid is described as it is.
fn verify_root(table: &dyn ProcessTable, root: u32, expected: Option<u64>) -> Option<WalkOutcome> {
    let expected = expected?;
    match table.start_token(root) {
        Some(live) if live == expected => None,
        Some(_) => Some(WalkOutcome::RootGone),
        None => Some(WalkOutcome::Unverified),
    }
}

/// Walk `root` and its descendants in `table`, listing at most `limit` descendants.
///
/// The root is verified against `expected_start` before anything is read and again after the
/// walk, as the foreground-process probe does, so a pid reused mid-walk never attributes another
/// program's tree to the pane. Cancellation is checked before the root and before each process.
// Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
pub fn walk_tree(
    table: &dyn ProcessTable,
    root: u32,
    expected_start: Option<u64>,
    limit: usize,
    cancel: &AtomicBool,
) -> ProcessTree {
    let empty = |outcome| ProcessTree { processes: Vec::new(), outcome };
    if cancel.load(Ordering::Relaxed) {
        // When: `cancel` is already set, a newer refresh superseded this walk before it began.
        return empty(WalkOutcome::Cancelled);
    }
    let Some(root_name) = table.name(root) else {
        // When: `table.name(root)` finds no process, the shell exited before the sample.
        return empty(WalkOutcome::RootGone);
    };
    if let Some(failed) = verify_root(table, root, expected_start) {
        // When: `verify_root` rejected the root, its pid was reused or cannot be checked.
        return empty(failed);
    }
    let mut processes =
        vec![ChildProcess { pid: root, depth: 0, name: root_name, memory: table.memory(root) }];
    let mut visited = HashSet::from([root]);
    let mut stack: Vec<(u32, usize)> = Vec::new();
    let mut room = limit;
    let mut outcome = WalkOutcome::Complete;
    if admit_children(table, root, 1, &mut room, &visited, &mut stack) {
        outcome = WalkOutcome::Truncated;
    }
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
        let Some(name) = table.name(pid) else {
            // When: `table.name(pid)` is None, the child exited between listing and sampling.
            continue;
        };
        processes.push(ChildProcess { pid, depth, name, memory: table.memory(pid) });
        if admit_children(table, pid, depth.saturating_add(1), &mut room, &visited, &mut stack) {
            outcome = WalkOutcome::Truncated;
        }
    }
    // A cancelled walk is discarded by its caller, so only a finished one is checked again.
    let recheck = (outcome != WalkOutcome::Cancelled)
        .then(|| verify_root(table, root, expected_start))
        .flatten();
    if let Some(failed) = recheck {
        // When: `recheck` failed after collection, the pid changed hands mid-walk; list nothing.
        return empty(failed);
    }
    ProcessTree { processes, outcome }
}

/// Push up to `room` unvisited children of `pid` onto `stack` so they pop in ascending pid order.
///
/// Asks the table for one more child than `room` holds, so it can tell a full list from a cut
/// one, and returns whether children were left out.
fn admit_children(
    table: &dyn ProcessTable,
    pid: u32,
    depth: usize,
    room: &mut usize,
    visited: &HashSet<u32>,
    stack: &mut Vec<(u32, usize)>,
) -> bool {
    let mut children = table.children(pid, room.saturating_add(1));
    children.retain(|child| *child != pid && !visited.contains(child));
    children.sort_unstable();
    children.dedup();
    let truncated = children.len() > *room;
    children.truncate(*room);
    *room -= children.len();
    stack.extend(children.into_iter().rev().map(|child| (child, depth)));
    truncated
}

/// Walk each root in `table`, checking `cancel` before each one.
///
/// A root not reached because `cancel` was set reports [`WalkOutcome::Cancelled`].
pub fn walk_roots(
    table: &dyn ProcessTable,
    roots: &[(u32, Option<u64>)],
    limit: usize,
    cancel: &AtomicBool,
) -> Vec<ProcessTree> {
    roots.iter().map(|(pid, start)| walk_tree(table, *pid, *start, limit, cancel)).collect()
}

/// Walk each root's descendants in the live OS process table.
///
/// Each root is `(pid, expected start token)`. One process-table snapshot serves every root
/// on platforms that take one, so a refresh costs one snapshot rather than one per pane.
// Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
#[must_use]
pub fn sample_trees(roots: &[(u32, Option<u64>)], cancel: &AtomicBool) -> Vec<ProcessTree> {
    let every = |outcome| {
        roots.iter().map(|_| ProcessTree { processes: Vec::new(), outcome }).collect::<Vec<_>>()
    };
    if cancel.load(Ordering::Relaxed) {
        // When: `cancel` is already set, skip the process-table snapshot entirely.
        return every(WalkOutcome::Cancelled);
    }
    let Some(table) = platform::table(cancel) else {
        // When: `platform::table` is None, enumeration was cancelled or this platform has none.
        // A set `cancel` stopped the snapshot; otherwise the platform has no process query.
        let outcome = if cancel.load(Ordering::Relaxed) {
            WalkOutcome::Cancelled
        } else {
            WalkOutcome::Unsupported
        };
        return every(outcome);
    };
    walk_roots(&table, roots, MAX_DESCENDANTS_PER_ROOT, cancel)
}

/// Longest decimal pid token accepted; a longer token cannot be a `u32` and is skipped.
const MAX_PID_DIGITS: usize = 10;

/// Read up to `limit` whitespace-separated pids from `reader`, stopping as soon as it has them.
///
/// Parses as it reads, one buffered chunk at a time, so a children list of any length costs no
/// more than the pids taken; `cancel` is checked before each chunk. Tokens that are not decimal
/// `u32` values are skipped. A read error ends the list with the pids found so far.
// Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
pub fn read_pids(mut reader: impl std::io::BufRead, limit: usize, cancel: &AtomicBool) -> Vec<u32> {
    let mut pids = Vec::new();
    let mut token = PidToken::default();
    while pids.len() < limit {
        if cancel.load(Ordering::Relaxed) {
            // When: `cancel` is set, a newer refresh superseded this read, so stop reading.
            return pids;
        }
        let Ok(chunk) = reader.fill_buf() else {
            // When: `fill_buf` failed, keep the pids already read.
            return pids;
        };
        if chunk.is_empty() {
            // When: `chunk` is empty at end of input, the last token ends here.
            token.finish_into(&mut pids, limit);
            return pids;
        }
        let mut used = 0;
        for byte in chunk {
            used += 1;
            if byte.is_ascii_whitespace() {
                // When: `byte` is whitespace, the current token ends and is kept if it is a pid.
                token.finish_into(&mut pids, limit);
                if pids.len() >= limit {
                    // When: `pids` reached `limit`, stop here; the rest of the stream is never read.
                    break;
                }
            } else {
                // When: `byte` is not whitespace, it extends the current token.
                token.push(*byte);
            }
        }
        reader.consume(used);
    }
    pids
}

/// One pid token being parsed: its value so far, digit count, and whether it is still valid.
#[derive(Debug, Default)]
struct PidToken {
    value: u64,
    digits: usize,
    invalid: bool,
}

impl PidToken {
    /// Add one byte; anything but a digit, or too many digits, invalidates the token.
    fn push(&mut self, byte: u8) {
        if !byte.is_ascii_digit() || self.digits >= MAX_PID_DIGITS {
            // When: `byte` is not a digit or the token passed `MAX_PID_DIGITS`, it is no pid.
            self.invalid = true;
            return;
        }
        self.value = self.value * 10 + u64::from(byte - b'0');
        self.digits += 1;
    }

    /// End the token: push it to `pids` when it is a valid `u32` and room remains, then reset.
    fn finish_into(&mut self, pids: &mut Vec<u32>, limit: usize) {
        let token = std::mem::take(self);
        if token.digits > 0 && !token.invalid && pids.len() < limit {
            if let Ok(pid) = u32::try_from(token.value) {
                pids.push(pid);
            }
        }
    }
}

/// One OS process's name and memory figure, or `None` when it does not exist.
#[must_use]
pub fn sample_process(pid: u32) -> Option<ChildProcess> {
    let never_cancelled = AtomicBool::new(false);
    let table = platform::table(&never_cancelled)?;
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

/// List this process's threads and their names; a set `cancel` stops the listing early.
#[must_use]
pub fn sample_threads(cancel: &AtomicBool) -> ThreadSummary {
    platform::threads(cancel)
}

#[cfg(target_os = "macos")]
mod platform {
    //! macOS: `libproc` (`proc_pid_rusage`, `proc_pidinfo`, `proc_listchildpids`).

    use std::sync::atomic::{AtomicBool, Ordering};

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

        fn children(&self, pid: u32, limit: usize) -> Vec<u32> {
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
                .take(limit)
                .collect()
        }

        fn memory(&self, pid: u32) -> MemoryMetric {
            rusage(pid).map_or(MemoryMetric::Unsupported, |info| {
                MemoryMetric::Bytes(info.ri_phys_footprint)
            })
        }
    }

    /// The live table, queried per pid; always available on macOS, so `_cancel` is not consulted.
    pub(super) fn table(_cancel: &AtomicBool) -> Option<Table> {
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
    ///
    /// A set `cancel` stops between names and reports no thread list.
    // Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
    pub(super) fn threads(cancel: &AtomicBool) -> ThreadSummary {
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
        let mut names = Vec::with_capacity(listed);
        for handle in &handles {
            if cancel.load(Ordering::Relaxed) {
                // When: `cancel` is set, a newer refresh superseded this listing, so stop naming threads.
                return unknown;
            }
            names.push(thread_name(pid, *handle));
        }
        ThreadSummary { total: Some(listed), names: group_thread_names(names) }
    }
}

#[cfg(windows)]
mod platform {
    //! Windows: Tool Help snapshots for processes and threads, `GetProcessMemoryInfo` per process.

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, FILETIME, HANDLE, HLOCAL, WAIT_OBJECT_0,
    };
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
}

#[cfg(target_os = "linux")]
mod platform {
    //! Linux: `/proc`.

    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        group_thread_names, read_pids, MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary,
    };

    /// Longest `/proc/<pid>/stat` read; the kernel line is far shorter.
    const MAX_STAT_BYTES: u64 = 4096;

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
        let file = std::fs::File::open(format!("/proc/{pid}/stat")).ok()?;
        let mut stat = String::new();
        file.take(MAX_STAT_BYTES).read_to_string(&mut stat).ok()?;
        let after_name = &stat[stat.rfind(')')? + 1..];
        Some(after_name.split_whitespace().map(str::to_string).collect())
    }

    /// Start time of `pid` in clock ticks since boot: field 22 of stat, index 19 after the name.
    fn start_ticks(pid: u32) -> Option<u64> {
        stat_fields(pid)?.get(19)?.parse::<u64>().ok()
    }

    /// The live `/proc` table, read per pid, stopping its parent scan when `cancel` is set.
    pub(super) struct Table<'cancel> {
        cancel: &'cancel AtomicBool,
    }

    impl ProcessTable for Table<'_> {
        fn name(&self, pid: u32) -> Option<String> {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            Some(comm.trim_end().to_string())
        }

        fn start_token(&self, pid: u32) -> Option<u64> {
            start_ticks(pid)
        }

        // Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
        fn children(&self, pid: u32, limit: usize) -> Vec<u32> {
            let mut children: Vec<u32> = Vec::new();
            let mut listed = false;
            if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
                // When: `tasks` lists the process's threads, each thread's `children` file is read.
                for task in tasks.flatten() {
                    if children.len() >= limit || self.cancel.load(Ordering::Relaxed) {
                        // When: `children` reached `limit` or `cancel` is set, stop reading task files.
                        break;
                    }
                    if let Ok(file) = std::fs::File::open(task.path().join("children")) {
                        listed = true;
                        let room = limit.saturating_sub(children.len());
                        children.extend(read_pids(
                            std::io::BufReader::new(file),
                            room,
                            self.cancel,
                        ));
                    }
                }
            }
            if listed {
                // When: some task's `children` file was readable, the kernel listed them directly.
                children.truncate(limit);
                return children;
            }
            let Ok(entries) = std::fs::read_dir("/proc") else {
                // When: `read_dir` of `/proc` failed into `entries`, no parent scan is possible.
                return children;
            };
            for entry in entries.flatten() {
                if children.len() >= limit || self.cancel.load(Ordering::Relaxed) {
                    // When: `children` reached `limit` or `cancel` is set, stop the parent scan.
                    break;
                }
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
    pub(super) fn table(cancel: &AtomicBool) -> Option<Table<'_>> {
        std::path::Path::new("/proc").is_dir().then_some(Table { cancel })
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
        let process_ticks = start_ticks(std::process::id())?;
        let raw_ticks =
            // SAFETY: `sysconf` reads a system constant and has no memory-safety preconditions.
            unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let ticks_per_sec = u64::try_from(raw_ticks).ok()?;
        let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
        let system_secs =
            uptime.split_whitespace().next()?.split('.').next()?.parse::<u64>().ok()?;
        system_secs.checked_sub(process_ticks / ticks_per_sec.max(1))
    }

    /// Thread names from `/proc/self/task/*/comm`; a set `cancel` reports no thread list.
    // Ordering: `cancel` is loaded Relaxed; it is a standalone stop request that publishes no other memory.
    pub(super) fn threads(cancel: &AtomicBool) -> ThreadSummary {
        let unknown = ThreadSummary { total: None, names: Vec::new() };
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            // When: `read_dir` gave no `tasks`, the task directory is unreadable and threads cannot be listed.
            return unknown;
        };
        let mut names: Vec<Option<String>> = Vec::new();
        for task in tasks.flatten() {
            if cancel.load(Ordering::Relaxed) {
                // When: `cancel` is set, a newer refresh superseded this listing, so stop reading names.
                return unknown;
            }
            names.push(
                std::fs::read_to_string(task.path().join("comm"))
                    .ok()
                    .map(|name| name.trim_end().to_string()),
            );
        }
        ThreadSummary { total: Some(names.len()), names: group_thread_names(names) }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    //! Other platforms: no process-table or footprint query.

    use std::sync::atomic::AtomicBool;

    use super::{MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary};

    /// Placeholder table type; `table` never returns one on these platforms.
    #[allow(dead_code)] // Named only in `table`'s signature, which always returns `None`.
    pub(super) struct Table;

    impl ProcessTable for Table {
        fn name(&self, _pid: u32) -> Option<String> {
            None
        }
        fn start_token(&self, _pid: u32) -> Option<u64> {
            None
        }
        fn children(&self, _pid: u32, _limit: usize) -> Vec<u32> {
            Vec::new()
        }
        fn memory(&self, _pid: u32) -> MemoryMetric {
            MemoryMetric::Unsupported
        }
    }

    /// No process table on this platform.
    pub(super) fn table(_cancel: &AtomicBool) -> Option<Table> {
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
    pub(super) fn threads(_cancel: &AtomicBool) -> ThreadSummary {
        ThreadSummary { total: None, names: Vec::new() }
    }
}

#[cfg(test)]
#[path = "process_report_tests.rs"]
mod process_report_tests;
