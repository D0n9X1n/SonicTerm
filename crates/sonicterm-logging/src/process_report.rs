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

/// This process's own CPU time (user plus system, every thread; children excluded), or `None`
/// where the OS has no query. The status line's `cpu` divides its change by wall time.
#[must_use]
pub fn sample_cpu_time() -> Option<std::time::Duration> {
    platform::cpu_time()
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
#[path = "process_report_macos.rs"]
mod platform;

#[cfg(windows)]
#[path = "process_report_windows.rs"]
mod platform;

#[cfg(target_os = "linux")]
#[path = "process_report_linux.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
#[path = "process_report_unsupported.rs"]
mod platform;

#[cfg(test)]
#[path = "process_report_tests.rs"]
mod process_report_tests;
