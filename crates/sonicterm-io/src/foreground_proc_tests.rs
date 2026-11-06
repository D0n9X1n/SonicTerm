use super::*;

#[test]
fn direct_token_elevation_and_gsudo_broker_both_require_the_warning() {
    // Protect gsudo elevation when its regular client brokers a high-integrity descendant.
    assert!(!foreground_process_is_privileged("pwsh", Some(false), false));
    assert!(foreground_process_is_privileged("pwsh", Some(true), false));
    assert!(foreground_process_is_privileged("gsudo", Some(false), false));
    assert!(foreground_process_is_privileged("GSUDO.EXE", None, false));
    assert!(foreground_process_is_privileged("pwsh", None, true));
}

#[test]
fn token_elevation_maps_zero_and_nonzero_values() {
    // Protect the Win32 TOKEN_ELEVATION DWORD contract independently of process access.
    assert!(!token_elevation_value_is_privileged(0));
    assert!(token_elevation_value_is_privileged(1));
    assert!(token_elevation_value_is_privileged(u32::MAX));
}

#[test]
fn selected_leaf_path_reaches_the_pty_root_through_gsudo() {
    // Protect broker recognition from scanning unrelated process names outside the selected ancestry.
    let snapshot = [
        ProcEntry { pid: 10, parent: 1, create_time: 1 },
        ProcEntry { pid: 20, parent: 10, create_time: 2 },
        ProcEntry { pid: 30, parent: 20, create_time: 3 },
        ProcEntry { pid: 99, parent: 1, create_time: 4 },
    ];

    assert_eq!(path_to_root(&snapshot, 10, 30), Some(vec![30, 20, 10]));
    assert_eq!(path_to_root(&snapshot, 10, 99), None);
}

#[test]
fn deepest_leaf_prefers_depth_then_recent_creation_time() {
    // Protect one shared process index from selecting a stale sibling over the active descendant.
    let snapshot = [
        ProcEntry { pid: 10, parent: 1, create_time: 1 },
        ProcEntry { pid: 20, parent: 10, create_time: 2 },
        ProcEntry { pid: 30, parent: 10, create_time: 3 },
        ProcEntry { pid: 40, parent: 20, create_time: 4 },
        ProcEntry { pid: 50, parent: 30, create_time: 5 },
    ];

    assert_eq!(pick_deepest_leaf(&snapshot, 10), Some(50));
    assert_eq!(pick_deepest_leaf(&snapshot, 99), Some(99));
}

#[test]
fn cyclic_or_detached_ancestry_is_rejected() {
    // Protect the fallback walk from looping on corrupt or racing process snapshots.
    let snapshot = [
        ProcEntry { pid: 10, parent: 1, create_time: 1 },
        ProcEntry { pid: 20, parent: 20, create_time: 2 },
    ];

    assert_eq!(path_to_root(&snapshot, 10, 20), None);
}

#[test]
fn current_process_token_query_returns_an_observation() {
    // Exercise the real TOKEN_QUERY boundary without assuming CI integrity level.
    assert!(process_token_is_elevated(std::process::id()).is_some());
}

/// Run `walk` on another thread and fail after 5 s instead of hanging the suite on a cycle.
fn within_deadline<T: Send + 'static>(walk: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(walk());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the process-tree walk did not finish: it looped on a cycle")
}

#[test]
fn deepest_leaf_ends_on_two_and_three_pid_cycles() {
    // Protect the probe thread from looping forever when a racing snapshot links pids in a cycle
    // below the root. The cycle members share a creation time, so only the visited set (never the
    // stale-pid filter) breaks the loop, and the separate real leaf 60 is still chosen.
    let two_cycle = within_deadline(|| {
        let snapshot = [
            ProcEntry { pid: 10, parent: 1, create_time: 1 },
            ProcEntry { pid: 20, parent: 10, create_time: 5 },
            ProcEntry { pid: 30, parent: 20, create_time: 5 },
            ProcEntry { pid: 20, parent: 30, create_time: 5 },
            ProcEntry { pid: 50, parent: 10, create_time: 6 },
            ProcEntry { pid: 60, parent: 50, create_time: 10 },
        ];
        pick_deepest_leaf(&snapshot, 10)
    });
    assert_eq!(two_cycle, Some(60));
    let three_cycle = within_deadline(|| {
        let snapshot = [
            ProcEntry { pid: 10, parent: 1, create_time: 1 },
            ProcEntry { pid: 20, parent: 10, create_time: 5 },
            ProcEntry { pid: 30, parent: 20, create_time: 5 },
            ProcEntry { pid: 40, parent: 30, create_time: 5 },
            ProcEntry { pid: 20, parent: 40, create_time: 5 },
            ProcEntry { pid: 50, parent: 10, create_time: 6 },
            ProcEntry { pid: 60, parent: 50, create_time: 7 },
            ProcEntry { pid: 70, parent: 60, create_time: 20 },
        ];
        pick_deepest_leaf(&snapshot, 10)
    });
    assert_eq!(three_cycle, Some(70));
}

#[test]
fn a_child_created_before_its_parent_is_a_reused_pid_not_a_descendant() {
    // Protect the leaf choice from a recycled pid whose stale parent link names the shell: a
    // process cannot be younger than its child, so that link is skipped.
    let snapshot = [
        ProcEntry { pid: 10, parent: 1, create_time: 100 },
        ProcEntry { pid: 20, parent: 10, create_time: 200 },
        ProcEntry { pid: 30, parent: 10, create_time: 50 },
    ];
    assert_eq!(pick_deepest_leaf(&snapshot, 10), Some(20));
}

#[test]
fn a_descendant_whose_only_child_is_stale_is_still_the_leaf() {
    // Protect leaf choice over admissible edges: pid 30's only listed child is a recycled pid
    // created before it, so 30 itself is the deepest real process, not the shallower 20 or root.
    let snapshot = [
        ProcEntry { pid: 10, parent: 1, create_time: 100 },
        ProcEntry { pid: 20, parent: 10, create_time: 200 },
        ProcEntry { pid: 30, parent: 20, create_time: 300 },
        ProcEntry { pid: 40, parent: 30, create_time: 150 },
        ProcEntry { pid: 50, parent: 10, create_time: 400 },
    ];
    assert_eq!(pick_deepest_leaf(&snapshot, 10), Some(30));
}
