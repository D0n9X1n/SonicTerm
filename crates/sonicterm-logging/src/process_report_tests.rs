use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;

use super::*;

/// Environment variable that turns the ignored helper test into a long-lived child process.
const HELPER_ENV: &str = "SONICTERM_PROCESS_REPORT_HELPER";

/// An in-memory process table: pid -> (name, start token, children, memory bytes).
struct FakeTable {
    processes: HashMap<u32, (&'static str, u64, Vec<u32>, u64)>,
}

impl ProcessTable for FakeTable {
    fn name(&self, pid: u32) -> Option<String> {
        self.processes.get(&pid).map(|entry| entry.0.to_string())
    }
    fn start_token(&self, pid: u32) -> Option<u64> {
        self.processes.get(&pid).map(|entry| entry.1)
    }
    fn children(&self, pid: u32) -> Vec<u32> {
        self.processes.get(&pid).map(|entry| entry.2.clone()).unwrap_or_default()
    }
    fn memory(&self, pid: u32) -> MemoryMetric {
        self.processes
            .get(&pid)
            .map_or(MemoryMetric::Unsupported, |entry| MemoryMetric::Bytes(entry.3))
    }
}

/// A shell (10) with two children (20, 30), where 20 has a grandchild (21).
fn sample_table() -> FakeTable {
    FakeTable {
        processes: HashMap::from([
            (10, ("zsh", 100, vec![30, 20], 1_000)),
            (20, ("tmux", 200, vec![21], 2_000)),
            (21, ("vim", 210, vec![], 3_000)),
            (30, ("claude", 300, vec![], 4_000)),
        ]),
    }
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

/// Contract: a set cancel flag stops the walk before any descendant and reports cancellation.
#[test]
fn a_cancelled_walk_stops_and_says_so() {
    let cancel = AtomicBool::new(true);
    let tree = walk_tree(&sample_table(), 10, None, 64, &cancel);
    assert_eq!(tree.processes.len(), 1, "only the root was sampled");
    assert_eq!(tree.outcome, WalkOutcome::Cancelled);
}

/// Contract: a parent link that loops back (pid reuse) lists each process once and terminates.
#[test]
fn a_parent_cycle_lists_each_process_once() {
    let table = FakeTable {
        processes: HashMap::from([(1, ("sh", 1, vec![2], 10)), (2, ("loop", 2, vec![1, 2], 20))]),
    };
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
    let threads = sample_threads();
    let total = threads.total.expect("thread count");
    assert!(total >= 1);
    assert_eq!(threads.names.iter().map(|(_, count)| count).sum::<usize>(), total);
    assert!(process_uptime_secs().is_some());
}

/// Contract: a spawned child appears among this process's descendants with a name, and once it
/// has exited and been reaped its pid samples as absent.
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
    let found = found.expect("spawned helper listed as a descendant");
    assert_eq!(found.depth, 1);
    assert!(!found.name.is_empty());
    assert!(sample_process(child_pid).is_none(), "an exited, reaped child samples as absent");
}
