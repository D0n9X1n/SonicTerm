//! Pins the macOS foreground batch: every pid's start token is read before the one shared
//! process-table snapshot and again after it, and a token that is missing or changed rejects
//! only that pid's observation, in input order.

use std::sync::Mutex;

use sonicterm_io::proc_info::{ForegroundProcess, ProcessIdentity};

use super::*;

/// A named, unprivileged observation.
fn named(name: &str) -> Option<ForegroundProcess> {
    Some(ForegroundProcess { name: name.to_owned(), privileged: false })
}

#[test]
fn tokens_bracket_one_shared_snapshot() {
    // The calls happen in this order: each pid's token, one snapshot of all pids, then each pid's
    // token again, so a pid reused at any point between its two reads is caught.
    let calls = Mutex::new(Vec::<String>::new());
    let identities = [ProcessIdentity { pid: 1, start: 10 }, ProcessIdentity { pid: 2, start: 20 }];
    let token = |pid: u32| {
        calls.lock().unwrap().push(format!("token {pid}"));
        Some(u64::from(pid) * 10)
    };
    let snapshot = |pids: &[u32]| {
        calls.lock().unwrap().push(format!("snapshot {pids:?}"));
        vec![named("vim"), named("cargo")]
    };
    let observations = sample_batch(&identities, &|| false, &token, &snapshot);
    assert_eq!(observations, vec![named("vim"), named("cargo")]);
    assert_eq!(
        *calls.lock().unwrap(),
        ["token 1", "token 2", "snapshot [1, 2]", "token 1", "token 2"],
        "one snapshot, bracketed by every pid's token"
    );
}

#[test]
fn a_changed_or_missing_token_rejects_only_its_own_pid() {
    // Pid 2 was reused (its token changed between reads) and pid 3 left (no token): both lose
    // their observation; pids 1 and 4 keep theirs, in input order.
    let identities = [
        ProcessIdentity { pid: 1, start: 10 },
        ProcessIdentity { pid: 2, start: 20 },
        ProcessIdentity { pid: 3, start: 30 },
        ProcessIdentity { pid: 4, start: 40 },
    ];
    let reads = Mutex::new(std::collections::HashMap::<u32, u32>::new());
    let token = |pid: u32| {
        let mut reads = reads.lock().unwrap();
        let count = reads.entry(pid).or_default();
        *count += 1;
        match (pid, *count) {
            (2, 2) => Some(99),
            (3, _) => None,
            _ => Some(u64::from(pid) * 10),
        }
    };
    let snapshot = |_: &[u32]| vec![named("one"), named("two"), named("three"), named("four")];
    let observations = sample_batch(&identities, &|| false, &token, &snapshot);
    assert_eq!(observations, vec![named("one"), None, None, named("four")]);
}

#[test]
fn a_stop_before_the_snapshot_takes_none() {
    // A dropping App ends the batch before the shared snapshot is ever taken.
    let snapshots = Mutex::new(0usize);
    let identities = [ProcessIdentity { pid: 1, start: 10 }];
    let snapshot = |_: &[u32]| {
        *snapshots.lock().unwrap() += 1;
        vec![named("vim")]
    };
    let observations = sample_batch(&identities, &|| true, &|_| Some(10), &snapshot);
    assert!(observations.is_empty());
    assert_eq!(*snapshots.lock().unwrap(), 0);
}

/// No frame path or timer walks the process table: the probe and `pid()` appear only in
/// the worker's platform sampler.
#[test]
fn no_event_loop_path_probes_the_foreground_process() {
    for (name, source) in [
        ("privilege.rs", include_str!("privilege.rs")),
        ("tab_state.rs", include_str!("tab_state.rs")),
        (
            "window_event.rs",
            concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            ),
        ),
        ("child_window_redraw.rs", include_str!("child_window_redraw.rs")),
        ("event_loop/windows.rs", include_str!("event_loop/windows.rs")),
    ] {
        let source = source.replace("\r\n", "\n");
        for probe in ["foreground_process_info", "foreground_processes_info"] {
            assert!(!source.contains(probe), "{name} probes on the event loop");
        }
        assert!(!source.contains(".pid()"), "{name} reads a pid on the frame path");
    }
    let frame_counters = concat!(
        include_str!("frame_counters.rs"),
        "\n",
        include_str!("frame_counters_app.rs"),
        "\n",
        include_str!("frame_counters_dispatch.rs"),
        "\n",
        include_str!("frame_counters_histogram.rs"),
        "\n",
        include_str!("frame_counters_record.rs")
    );
    assert!(!frame_counters.contains("fn time_probe"), "the event-loop probe timer is deleted");
}
