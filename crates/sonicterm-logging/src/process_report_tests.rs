use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::*;

/// Environment variable that turns the ignored helper test into a long-lived child process.
const HELPER_ENV: &str = "SONICTERM_PROCESS_REPORT_HELPER";

/// An in-memory process table: pid -> (name, start token, children, memory bytes).
///
/// `token_reads`, when not empty, injects the start-token reader: each read pops the next value,
/// so a test can make the token unreadable or change it between the walk's two checks.
/// `child_limits` records each `children` request, and `cancel_on_name` sets a flag when one pid's
/// name is read, so a test can cancel a walk partway through.
struct FakeTable {
    processes: HashMap<u32, (&'static str, u64, Vec<u32>, u64)>,
    token_reads: RefCell<VecDeque<Option<u64>>>,
    child_limits: RefCell<Vec<usize>>,
    cancel_on_name: Option<(u32, Arc<AtomicBool>)>,
}

impl FakeTable {
    /// A table over `processes` with the real token reader and no cancel trigger.
    fn new(processes: HashMap<u32, (&'static str, u64, Vec<u32>, u64)>) -> Self {
        Self {
            processes,
            token_reads: RefCell::new(VecDeque::new()),
            child_limits: RefCell::new(Vec::new()),
            cancel_on_name: None,
        }
    }
}

impl ProcessTable for FakeTable {
    fn name(&self, pid: u32) -> Option<String> {
        if let Some((trigger, cancel)) = &self.cancel_on_name {
            if *trigger == pid {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        self.processes.get(&pid).map(|entry| entry.0.to_string())
    }
    fn start_token(&self, pid: u32) -> Option<u64> {
        if let Some(injected) = self.token_reads.borrow_mut().pop_front() {
            return injected;
        }
        self.processes.get(&pid).map(|entry| entry.1)
    }
    fn children(&self, pid: u32, limit: usize) -> Vec<u32> {
        self.child_limits.borrow_mut().push(limit);
        let all = self.processes.get(&pid).map(|entry| entry.2.clone()).unwrap_or_default();
        all.into_iter().take(limit).collect()
    }
    fn memory(&self, pid: u32) -> MemoryMetric {
        self.processes
            .get(&pid)
            .map_or(MemoryMetric::Unsupported, |entry| MemoryMetric::Bytes(entry.3))
    }
}

/// A shell (10) with two children (20, 30), where 20 has a grandchild (21).
fn sample_table() -> FakeTable {
    FakeTable::new(HashMap::from([
        (10, ("zsh", 100, vec![30, 20], 1_000)),
        (20, ("tmux", 200, vec![21], 2_000)),
        (21, ("vim", 210, vec![], 3_000)),
        (30, ("claude", 300, vec![], 4_000)),
    ]))
}

/// Spawn this test binary as a child that blocks until its stdin closes.
fn spawn_helper() -> Child {
    Command::new(std::env::current_exe().expect("test binary path"))
        .args([
            "--exact",
            "process_report::process_report_tests::helper_blocks_until_stdin_closes",
            "--ignored",
            "--nocapture",
        ])
        .env(HELPER_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn helper")
}

/// Not a test of its own: when spawned with `HELPER_ENV`, this run of the test binary stays alive
/// until the parent closes its stdin, giving the walk a portable live child process to find.
#[test]
#[ignore = "helper process body, run only as a spawned child"]
fn helper_blocks_until_stdin_closes() {
    if std::env::var_os(HELPER_ENV).is_none() {
        // When: `HELPER_ENV` is unset, this is a normal ignored-test run, so return at once.
        return;
    }
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
}

/// Contract: the walk lists the shell first, then descendants depth-first in pid order with
/// their depth, names and memory, and reports a complete walk.
#[test]
fn walk_lists_the_root_then_descendants_depth_first() {
    let cancel = AtomicBool::new(false);
    let tree = walk_tree(&sample_table(), 10, Some(100), 64, &cancel);
    let listed: Vec<(u32, usize)> =
        tree.processes.iter().map(|process| (process.pid, process.depth)).collect();
    assert_eq!(listed, vec![(10, 0), (20, 1), (21, 2), (30, 1)]);
    assert_eq!(tree.processes[3].name, "claude");
    assert_eq!(tree.processes[3].memory, MemoryMetric::Bytes(4_000));
    assert_eq!(tree.outcome, WalkOutcome::Complete);
}

/// Contract: the walk stops after `limit` descendants and says it was truncated.
#[test]
fn walk_stops_at_the_descendant_limit() {
    let cancel = AtomicBool::new(false);
    let tree = walk_tree(&sample_table(), 10, None, 2, &cancel);
    assert_eq!(tree.processes.len(), 3, "the root plus two descendants");
    assert_eq!(tree.outcome, WalkOutcome::Truncated);
}

/// Contract: a limit that exactly covers every descendant is a complete walk, not a truncated one.
#[test]
fn walk_with_an_exact_limit_is_complete() {
    let cancel = AtomicBool::new(false);
    let tree = walk_tree(&sample_table(), 10, None, 3, &cancel);
    assert_eq!(tree.processes.len(), 4);
    assert_eq!(tree.outcome, WalkOutcome::Complete);
}

/// Contract: a root whose live start token differs from the pane's recorded one is a reused pid,
/// so the walk lists nothing rather than describing another program's tree.
#[test]
fn a_reused_root_pid_is_reported_gone() {
    let cancel = AtomicBool::new(false);
    let tree = walk_tree(&sample_table(), 10, Some(999), 64, &cancel);
    assert!(tree.processes.is_empty());
    assert_eq!(tree.outcome, WalkOutcome::RootGone);
    let missing = walk_tree(&sample_table(), 77, None, 64, &cancel);
    assert_eq!(missing.outcome, WalkOutcome::RootGone);
}

/// Contract: a cancel flag set before the walk stops it before the root is read at all.
#[test]
fn a_cancelled_walk_stops_and_says_so() {
    let cancel = AtomicBool::new(true);
    let table = sample_table();
    let tree = walk_tree(&table, 10, None, 64, &cancel);
    assert!(tree.processes.is_empty(), "nothing is sampled once cancelled");
    assert_eq!(tree.outcome, WalkOutcome::Cancelled);
    assert!(table.child_limits.borrow().is_empty(), "no child list was requested");
}

/// Contract: a recorded start token that cannot be read now fails closed: the pane's tree is
/// reported unverified and nothing is listed, rather than trusting a pid that may be reused.
#[test]
fn an_unreadable_root_token_fails_closed() {
    let cancel = AtomicBool::new(false);
    let table = sample_table();
    table.token_reads.borrow_mut().push_back(None);
    let tree = walk_tree(&table, 10, Some(100), 64, &cancel);
    assert!(tree.processes.is_empty());
    assert_eq!(tree.outcome, WalkOutcome::Unverified);
}

/// Contract: the root token is checked again after the walk, so a pid that changed hands while
/// its children were read is reported gone and none of the collected rows are kept.
#[test]
fn a_root_token_changed_during_the_walk_discards_the_tree() {
    let cancel = AtomicBool::new(false);
    let table = sample_table();
    table.token_reads.borrow_mut().extend([Some(100), Some(555)]);
    let tree = walk_tree(&table, 10, Some(100), 64, &cancel);
    assert!(tree.processes.is_empty());
    assert_eq!(tree.outcome, WalkOutcome::RootGone);
    let after_failure = sample_table();
    after_failure.token_reads.borrow_mut().extend([Some(100), None]);
    let tree = walk_tree(&after_failure, 10, Some(100), 64, &cancel);
    assert_eq!(tree.outcome, WalkOutcome::Unverified, "an unreadable recheck also fails closed");
}

/// Contract: child lists are requested with a bound of the remaining room plus one, so a process
/// with many children never costs more than the walk can list.
#[test]
fn child_lists_are_requested_within_the_limit() {
    let cancel = AtomicBool::new(false);
    let mut processes = HashMap::from([(1, ("sh", 1, (2..2_000).collect::<Vec<u32>>(), 10))]);
    for child in 2..2_000 {
        processes.insert(child, ("job", 2, Vec::new(), 1));
    }
    let table = FakeTable::new(processes);
    let tree = walk_tree(&table, 1, None, 5, &cancel);
    assert_eq!(tree.processes.len(), 6, "the root plus five descendants");
    assert_eq!(tree.outcome, WalkOutcome::Truncated);
    assert!(
        table.child_limits.borrow().iter().all(|limit| *limit <= 6),
        "{:?}",
        table.child_limits
    );
}

/// Contract: cancellation is checked before each root and between processes, so a refresh that
/// lands mid-walk stops the current root and skips every later one.
#[test]
fn cancellation_between_roots_skips_the_rest() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut table = sample_table();
    table.processes.insert(40, ("bash", 400, vec![41], 5_000));
    table.processes.insert(41, ("make", 410, Vec::new(), 6_000));
    table.cancel_on_name = Some((20, Arc::clone(&cancel)));
    let trees = walk_roots(&table, &[(10, None), (40, None)], 64, &cancel);
    assert_eq!(trees[0].outcome, WalkOutcome::Cancelled);
    assert!(trees[0].processes.iter().all(|process| process.pid != 30), "the walk stopped");
    assert_eq!(trees[1].outcome, WalkOutcome::Cancelled);
    assert!(trees[1].processes.is_empty(), "a later root is never read");
}

/// Contract: a parent link that loops back (pid reuse) lists each process once and terminates.
#[test]
fn a_parent_cycle_lists_each_process_once() {
    let table = FakeTable::new(HashMap::from([
        (1, ("sh", 1, vec![2], 10)),
        (2, ("loop", 2, vec![1, 2], 20)),
    ]));
    let cancel = AtomicBool::new(false);
    let tree = walk_tree(&table, 1, None, 64, &cancel);
    let pids: Vec<u32> = tree.processes.iter().map(|process| process.pid).collect();
    assert_eq!(pids, vec![1, 2]);
    assert_eq!(tree.outcome, WalkOutcome::Complete);
}

/// Contract: thread names group by count, most numerous first, with empty names as `(unnamed)`.
#[test]
fn thread_names_group_by_count_then_name() {
    let grouped = group_thread_names([
        Some("sonic-pty-reader".to_string()),
        None,
        Some("sonic-pty-reader".to_string()),
        Some(String::new()),
        Some("main".to_string()),
    ]);
    assert_eq!(
        grouped,
        vec![
            ("(unnamed)".to_string(), 2),
            ("sonic-pty-reader".to_string(), 2),
            ("main".to_string(), 1)
        ]
    );
}

/// Contract: on supported platforms the live table finds this test process, its footprint and
/// peak are measured with the peak at least the current figure, and its threads are counted.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
#[test]
fn the_test_process_finds_itself() {
    let own = sample_process(std::process::id()).expect("own process");
    assert!(!own.name.is_empty());
    let footprint = sample_footprint();
    let current = footprint.current.bytes().expect("current footprint measured");
    let peak = footprint.peak.bytes().expect("peak footprint measured");
    assert!(current > 0);
    assert!(peak >= current, "peak {peak} below current {current}");
    let threads = sample_threads(&AtomicBool::new(false));
    let total = threads.total.expect("thread count");
    assert!(total >= 1);
    assert_eq!(threads.names.iter().map(|(_, count)| count).sum::<usize>(), total);
    assert!(process_uptime_secs().is_some());
}

/// Contract: a spawned child appears among this process's descendants with a name, and once it
/// has exited and been reaped its pid samples as absent. The `Child` is dropped before sampling,
/// so no handle of ours keeps the exited process object alive on Windows either.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
#[test]
fn a_spawned_child_is_listed_and_an_exited_one_is_absent() {
    let mut child = spawn_helper();
    let child_pid = child.id();
    let cancel = AtomicBool::new(false);
    let trees = sample_trees(&[(std::process::id(), None)], &cancel);
    let found = trees[0].processes.iter().find(|process| process.pid == child_pid).cloned();
    drop(child.stdin.take());
    let _ = child.wait();
    drop(child);
    let found = found.expect("spawned helper listed as a descendant");
    assert_eq!(found.depth, 1);
    assert!(!found.name.is_empty());
    assert!(sample_process(child_pid).is_none(), "an exited, reaped child samples as absent");
}

/// Contract: on Windows an exited child whose `Child` still holds its process handle samples as
/// absent. That open handle keeps the process object, so Tool Help still lists the pid; this test
/// failed on Windows before `Table::name` checked whether the handle is signaled. It compiles only
/// for Windows, so Windows CI is where it runs.
#[cfg(windows)]
#[test]
fn an_exited_child_whose_handle_is_still_open_samples_as_absent() {
    let mut child = spawn_helper();
    drop(child.stdin.take());
    child.wait().expect("helper exits once its stdin closes");
    // `child` stays alive here, so its process handle is still open while sampling.
    assert!(sample_process(child.id()).is_none(), "an exited child samples as absent");
    drop(child);
}

/// A generated children stream `"1 2 3 … end "` that counts the bytes it hands out.
struct SyntheticPids {
    next: u32,
    end: u32,
    pending: Vec<u8>,
    served: std::rc::Rc<std::cell::Cell<usize>>,
}

impl std::io::Read for SyntheticPids {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.pending.len() < buffer.len() && self.next <= self.end {
            self.pending.extend_from_slice(format!("{} ", self.next).as_bytes());
            self.next += 1;
        }
        let count = buffer.len().min(self.pending.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        self.served.set(self.served.get() + count);
        Ok(count)
    }
}

/// Contract: a children list of a million pids is parsed incrementally and reading stops at the
/// limit, so the bytes consumed stay a few buffer reads, not the whole stream.
#[test]
fn read_pids_stops_at_the_limit_on_a_huge_stream() {
    let served = std::rc::Rc::new(std::cell::Cell::new(0));
    let stream = SyntheticPids {
        next: 1,
        end: 1_000_000,
        pending: Vec::new(),
        served: std::rc::Rc::clone(&served),
    };
    let reader = std::io::BufReader::with_capacity(64, stream);
    let pids = read_pids(reader, 65, &AtomicBool::new(false));
    assert_eq!(pids, (1..=65).collect::<Vec<u32>>());
    assert!(served.get() < 1_024, "read {} bytes for 65 pids", served.get());
}

/// Contract: tokens that are not decimal `u32` pids are skipped, the last token is kept without a
/// trailing separator, and a set cancel flag reads nothing.
#[test]
fn read_pids_skips_invalid_tokens_and_honours_cancel() {
    let text = "12 x7 99999999999 4294967296 4294967295\n\t8";
    let pids = read_pids(text.as_bytes(), 64, &AtomicBool::new(false));
    assert_eq!(pids, vec![12, 4_294_967_295, 8]);
    assert!(read_pids("1 2 3".as_bytes(), 64, &AtomicBool::new(true)).is_empty());
    assert_eq!(read_pids("1 2 3".as_bytes(), 0, &AtomicBool::new(false)), Vec::<u32>::new());
}

#[test]
fn cpu_time_is_monotone_and_advances_over_a_busy_loop() {
    // Two samples around a busy loop never go backwards, and the loop's work shows up where the
    // platform has a query; elsewhere the sample is `None`.
    let first = sample_cpu_time();
    assert_eq!(first.is_some(), cfg!(any(target_os = "macos", windows, target_os = "linux")));
    let Some(before) = first else {
        return;
    };
    let started = std::time::Instant::now();
    let mut spin: u64 = 0;
    while started.elapsed() < std::time::Duration::from_millis(50) {
        spin = std::hint::black_box(spin.wrapping_add(1));
    }
    let after = sample_cpu_time().unwrap();
    assert!(after >= before);
    assert!(after > before, "50 ms of spinning adds CPU time");
}
