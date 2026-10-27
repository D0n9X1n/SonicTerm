//! Windows retained-process inspection, admission and cleanup custody. regression contracts.

use super::*;

/// A stale numeric parent must never give cleanup ownership of a process present before this fixture started.
#[cfg(windows)]
#[test]
fn candidate_admission_rejects_stale_parent_and_reused_pid() {
    let candidate = ProcessEntry { pid: 7, parent: 11, name: "ping.exe".into() };
    assert!(!candidate_has_owned_origin(&candidate, 11, 120, 100, &BTreeSet::from([7])));
}

/// Even without a snapshot hit, creation before the shell excludes a stale child of a recycled shell PID.
#[cfg(windows)]
#[test]
fn candidate_admission_rejects_creation_before_fixture_origin() {
    let candidate = ProcessEntry { pid: 7, parent: 11, name: "ping.exe".into() };
    assert!(!candidate_has_owned_origin(&candidate, 11, 99, 100, &BTreeSet::new()));
}

/// A rejected discovered candidate only closes its inspection handle; the owned test child must remain running.
#[cfg(windows)]
#[test]
fn candidate_admission_rejection_preserves_the_live_test_child() {
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start candidate rejection fixture");
    let candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let refused = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth + 1,
        &BTreeSet::new(),
        "descendant",
    );
    let survived = child.try_wait().unwrap().is_none();
    // This child belongs to the test; cleanup follows the observation even when an assertion will fail.
    if survived {
        child.kill().expect("stop candidate rejection fixture");
    }
    child.wait().expect("reap candidate rejection fixture");
    assert!(refused.unwrap().is_none(), "older candidate was adopted");
    assert!(survived, "a rejected inspection acquired termination custody");
}

/// Actual admission must retain the opened kernel handle through its live snapshot recheck, without a PID reopen.
#[cfg(windows)]
#[test]
fn candidate_admission_transfers_the_same_inspection_handle() {
    if pty_test_support::isolated() {
        return;
    }
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start handle transfer fixture");
    let candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let raw = inspection.handle.as_raw_handle();
    let birth = inspection.created;
    let owned = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth,
        &BTreeSet::new(),
        "descendant",
    )
    .expect("inspect owned candidate")
    .expect("accept owned candidate");
    let accepted_raw = owned.handle.as_raw_handle();
    // Cleanup precedes the assertion, so even a second-handle acceptance cannot leak this directly owned child.
    drop(owned);
    child.wait().expect("reap handle transfer fixture");
    assert_eq!(accepted_raw, raw, "admission replaced the already-open inspection handle");
}

/// A live inspection cannot borrow another candidate's metadata to acquire termination custody.
#[cfg(windows)]
#[test]
fn candidate_admission_rejects_mismatched_inspection_pid() {
    if pty_test_support::isolated() {
        return;
    }
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start inspection identity fixture");
    let mut candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    // Only this owned child is opened; the synthetic PID must fail before metadata can grant custody.
    candidate.pid = 0;
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let result = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth,
        &BTreeSet::new(),
        "descendant",
    );
    let refused = matches!(&result, Ok(None));
    drop(result);
    let survived = child.try_wait().unwrap().is_none();
    if survived {
        child.kill().expect("stop inspection identity fixture");
    }
    child.wait().expect("reap inspection identity fixture");
    assert!(refused, "mismatched candidate and inspection were admitted");
    assert!(survived, "mismatched inspection acquired termination custody");
}

/// Admission rechecks the live snapshot; a mismatched image name must reject custody without stopping the child.
#[cfg(windows)]
#[test]
fn candidate_admission_recheck_rejects_mismatched_name_without_termination() {
    if pty_test_support::isolated() {
        return;
    }
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start snapshot recheck fixture");
    let mut candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    // Keep PID, parent and birth valid so only the second live snapshot can reject this changed name.
    candidate.name.push_str(".mismatch");
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let result = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth,
        &BTreeSet::new(),
        "descendant",
    );
    let refused = matches!(&result, Ok(None));
    drop(result);
    let survived = child.try_wait().unwrap().is_none();
    if survived {
        child.kill().expect("stop snapshot recheck fixture");
    }
    child.wait().expect("reap snapshot recheck fixture");
    assert!(refused, "snapshot name disagreement was admitted");
    assert!(survived, "snapshot rejection acquired termination custody");
}

/// Candidate metadata cannot replace the live parent check; refusal leaves the directly owned child running.
#[cfg(windows)]
#[test]
fn candidate_admission_recheck_rejects_mismatched_parent_without_termination() {
    if pty_test_support::isolated() {
        return;
    }
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start parent recheck fixture");
    let mut candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    let incorrect_parent = child.id();
    assert_ne!(candidate.parent, incorrect_parent, "fixture child cannot be its own parent");
    // Both claimed parents agree so the preliminary predicate passes; only the live parent comparison can refuse.
    candidate.parent = incorrect_parent;
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let result = admit_process_candidate(
        &candidate,
        inspection,
        incorrect_parent,
        birth,
        &BTreeSet::new(),
        "descendant",
    );
    let refused = matches!(&result, Ok(None));
    drop(result);
    let survived = child.try_wait().unwrap().is_none();
    if survived {
        child.kill().expect("stop parent recheck fixture");
    }
    child.wait().expect("reap parent recheck fixture");
    assert!(refused, "live snapshot parent disagreement was admitted");
    assert!(survived, "parent rejection acquired termination custody");
}

/// A console host can precede the shell, but it must follow the pre-spawn fixture boundary and name our parent.
#[cfg(windows)]
#[test]
fn console_host_admission_uses_fixture_boundary_not_shell_birth() {
    let host = ProcessEntry { pid: 7, parent: 11, name: "conhost.exe".into() };
    assert!(candidate_has_owned_origin(&host, 11, 110, 100, &BTreeSet::new()));
    assert!(!candidate_has_owned_origin(&host, 11, 90, 100, &BTreeSet::new()));
    assert!(!candidate_has_owned_origin(&host, 12, 110, 100, &BTreeSet::new()));
}
