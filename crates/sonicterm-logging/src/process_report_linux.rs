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
                    children.extend(read_pids(std::io::BufReader::new(file), room, self.cancel));
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
            let Some(child) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok())
            else {
                // When: the entry name is not numeric, it is not a process directory.
                continue;
            };
            let parent = stat_fields(child).and_then(|fields| fields.get(1)?.parse::<u32>().ok());
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
    let system_secs = uptime.split_whitespace().next()?.split('.').next()?.parse::<u64>().ok()?;
    system_secs.checked_sub(process_ticks / ticks_per_sec.max(1))
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
