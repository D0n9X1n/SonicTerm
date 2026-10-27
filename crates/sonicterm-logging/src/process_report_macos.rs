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
        Some(info.pbi_start_tvsec.saturating_mul(1_000_000).saturating_add(info.pbi_start_tvusec))
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
        rusage(pid)
            .map_or(MemoryMetric::Unsupported, |info| MemoryMetric::Bytes(info.ri_phys_footprint))
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
        current: info
            .map_or(MemoryMetric::Unsupported, |info| MemoryMetric::Bytes(info.ri_phys_footprint)),
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

/// User plus system CPU time of every thread of this process, from `getrusage(RUSAGE_SELF)`.
pub(super) fn cpu_time() -> Option<std::time::Duration> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let status =
        // SAFETY: `usage` is a writable `rusage`; `getrusage` fills it and reads nothing from it.
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        // When: status is nonzero, the kernel refused the query and `usage` is not filled.
        return None;
    }
    let usage =
        // SAFETY: `getrusage` returned 0, so it initialized `usage`.
        unsafe { usage.assume_init() };
    let micros = |time: libc::timeval| {
        u64::try_from(time.tv_sec)
            .ok()?
            .checked_mul(1_000_000)?
            .checked_add(u64::try_from(time.tv_usec).ok()?)
    };
    Some(std::time::Duration::from_micros(
        micros(usage.ru_utime)?.checked_add(micros(usage.ru_stime)?)?,
    ))
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
