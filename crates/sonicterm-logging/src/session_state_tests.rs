use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch directory unique to one test.
///
/// Every test here writes markers, and the scan reads *every* marker in the
/// directory. A shared directory would make each test observe its siblings'
/// markers, so a test could fail reporting a defect that is not there — and
/// would do so only when the suite ran in a particular order.
fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("sonicterm-session-{label}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Arming writes a marker that a later scan can find.
#[test]
fn arming_leaves_a_marker_on_disk() {
    let dir = scratch("arm");
    let session = arm(&dir, "1.2.3").expect("arm");

    assert!(session.path().exists(), "the marker must exist while the session is running");
    assert!(!session.id().is_empty(), "a session must be identifiable");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A session that marks itself clean is not reported as unclean.
///
/// The false-positive direction is the one that matters. A diagnostic that
/// reports a crash on every ordinary launch trains its reader to ignore it,
/// and then it is worth nothing on the launch that follows a real crash.
#[test]
fn a_clean_shutdown_is_not_reported_as_unclean() {
    let dir = scratch("clean");
    let session = arm(&dir, "1.2.3").expect("arm");
    session.mark_clean().expect("mark clean");

    let prior = scan_prior_sessions(&dir, None);
    assert!(
        !prior.iter().any(|entry| matches!(entry, PriorSession::Unclean(_))),
        "a session that reached its shutdown path must never read as unclean: {prior:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A marker left behind by a dead process reads as an unclean exit.
///
/// The pid is one that cannot be running, so the liveness check cannot mask
/// the staleness this asserts.
#[test]
fn a_marker_from_a_dead_process_reads_as_unclean() {
    let dir = scratch("stale");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    // pid 0 is never a live user process on the platforms SonicTerm ships on.
    let marker = "id=test-stale\npid=0\nversion=1.2.3\nplatform=macos\n\
                  started_at=2026-01-01T00:00:00Z\nstate=armed\n";
    std::fs::write(session_dir(&dir).join("session-test-stale.marker"), marker).expect("write");

    let prior = scan_prior_sessions(&dir, None);
    assert_eq!(prior.len(), 1, "the stale marker must be found: {prior:?}");
    assert!(
        matches!(&prior[0], PriorSession::Unclean(marker) if marker.id == "test-stale"),
        "a marker whose process is gone means that session never reached shutdown: {prior:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An unclean report names no cause.
///
/// A stale marker distinguishes "did not finish" from "finished". It cannot
/// distinguish a `SIGKILL` from a power cut or an OOM kill, and naming one
/// would be a guess presented as a finding.
#[test]
fn an_unclean_report_does_not_invent_a_cause() {
    let dir = scratch("nocause");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    let marker = "id=test-nocause\npid=0\nversion=1.2.3\nplatform=macos\n\
                  started_at=2026-01-01T00:00:00Z\nstate=armed\n";
    std::fs::write(session_dir(&dir).join("session-test-nocause.marker"), marker).expect("write");

    let prior = scan_prior_sessions(&dir, None);
    let rendered = prior[0].to_string();

    for invented in ["SIGKILL", "killed", "out of memory", "OOM", "TerminateProcess", "crash"] {
        assert!(
            !rendered.contains(invented),
            "the report claimed {invented:?}, which a stale marker cannot establish: {rendered}"
        );
    }
    assert!(
        rendered.contains("not recorded") || rendered.contains("did not reach"),
        "the report must say what it actually knows: {rendered}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A live sibling process is not reported as a crash.
///
/// Several SonicTerm instances run at once routinely. Without the liveness
/// check, every launch would report every running sibling as an unclean exit —
/// a false positive on the most ordinary workflow there is.
#[test]
fn a_still_running_session_is_not_reported() {
    let dir = scratch("live");
    // Armed by *this* process, which is by definition alive.
    let session = arm(&dir, "1.2.3").expect("arm");

    let prior = scan_prior_sessions(&dir, None);
    assert!(
        prior.is_empty(),
        "a marker whose process is still running has not exited at all: {prior:?}"
    );

    drop(session);
    let _ = std::fs::remove_dir_all(&dir);
}

/// This session's own marker is never reported back to it.
#[test]
fn the_current_session_is_excluded_from_its_own_scan() {
    let dir = scratch("self");
    let session = arm(&dir, "1.2.3").expect("arm");

    let prior = scan_prior_sessions(&dir, Some(session.id()));
    assert!(prior.is_empty(), "a session must not discover itself: {prior:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A truncated marker is evidence, not something to skip.
///
/// A marker half-written when the power failed is exactly the case the marker
/// exists for. Treating an unparseable file as absent would discard it.
#[test]
fn a_truncated_marker_is_reported_as_corrupt() {
    let dir = scratch("corrupt");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    // Cut off mid-field, as an interrupted write would leave it.
    std::fs::write(session_dir(&dir).join("session-partial.marker"), "id=test-partial\npi")
        .expect("write");

    let prior = scan_prior_sessions(&dir, None);
    assert_eq!(prior.len(), 1, "the partial marker must be surfaced: {prior:?}");
    assert!(
        matches!(&prior[0], PriorSession::Corrupt { .. }),
        "an unparseable marker is evidence of an interrupted session, not a file to ignore: \
         {prior:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A kill before the atomic rename leaves a temporary file that is still evidence.
#[test]
fn a_partial_atomic_temp_marker_is_reported_as_corrupt() {
    let dir = scratch("partial-temp");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    let path = session_dir(&dir).join("session-20260801T120000.000Z-4294967295-0.tmp");
    std::fs::write(&path, "id=20260801T120000.000Z-4294967295-0\npi")
        .expect("write partial temp marker");

    let prior = scan_prior_sessions(&dir, None);
    assert!(
        matches!(&prior[..], [PriorSession::Corrupt { path: found }] if found == &path),
        "a torn temp marker from a dead process must be surfaced: {prior:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A live process may be between temp write and rename; do not call that stale.
#[test]
fn a_live_concurrent_atomic_temp_marker_is_not_reported() {
    let dir = scratch("live-temp");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    let path = session_dir(&dir)
        .join(format!("session-20260801T120000.000Z-{}-0.tmp", std::process::id()));
    std::fs::write(&path, "partial").expect("write live temp marker");
    assert_eq!(
        temp_marker_pid(&path),
        Some(std::process::id()),
        "the production temp filename must expose its owner pid"
    );

    assert!(
        scan_prior_sessions(&dir, None).is_empty(),
        "a live sibling may still be completing its atomic rename"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An empty marker file is corrupt rather than silently clean.
#[test]
fn an_empty_marker_is_not_mistaken_for_a_clean_exit() {
    let dir = scratch("empty");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    std::fs::write(session_dir(&dir).join("session-empty.marker"), "").expect("write");

    let prior = scan_prior_sessions(&dir, None);
    assert!(
        matches!(&prior[0], PriorSession::Corrupt { .. }),
        "an empty marker must not read as a clean exit: {prior:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Concurrent sessions each get their own marker.
///
/// One shared file would be overwritten by the second instance, and the first
/// session's evidence would be gone — reporting it clean when it was not.
#[test]
fn concurrent_sessions_do_not_overwrite_each_other() {
    let dir = scratch("concurrent");
    let first = arm(&dir, "1.2.3").expect("arm first");
    let second = arm(&dir, "1.2.3").expect("arm second");

    assert_ne!(first.id(), second.id(), "each launch must be distinguishable");
    assert_ne!(first.path(), second.path(), "a shared path would lose the first session");
    assert!(first.path().exists() && second.path().exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A marker carries process identity and nothing about the user's session.
///
/// The privacy boundary, asserted on the bytes actually written rather than on
/// the struct: a field added later that carried a title or a command would
/// show up here.
#[test]
fn a_marker_records_no_user_content() {
    let dir = scratch("privacy");
    let session = arm(&dir, "1.2.3").expect("arm");
    let written = std::fs::read_to_string(session.path()).expect("read marker");

    for field in ["id=", "pid=", "version=", "platform=", "started_at=", "state="] {
        assert!(written.contains(field), "marker must carry {field:?}: {written}");
    }
    // Only the six known keys may appear.
    for line in written.lines().filter(|line| !line.is_empty()) {
        let key = line.split_once('=').map(|(key, _)| key).unwrap_or(line);
        assert!(
            ["id", "pid", "version", "platform", "started_at", "state"].contains(&key),
            "unexpected field {key:?} in a marker; every field is a privacy decision: {written}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The public arm API cannot turn its version field into a free-form channel.
#[test]
fn a_marker_rejects_multiline_or_key_value_version_content() {
    for version in
        ["1.2.3\ncommand=ssh host", "TOKEN=secret", "1.2.3 shell=/bin/zsh", "../../../environment"]
    {
        let dir = scratch("unsafe-version");
        let error = arm(&dir, version).expect_err("unsafe version content must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{version:?}");
        assert!(
            std::fs::read_dir(session_dir(&dir))
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(true),
            "an invalid version must not leave a marker behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Safe non-shipping platform identifiers remain parseable in Unix CI.
#[test]
fn a_marker_accepts_a_safe_non_shipping_platform_identifier() {
    let dir = scratch("linux-platform");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    let marker = "id=test-linux\npid=0\nversion=1.2.3\nplatform=linux\n\
                  started_at=2026-01-01T00:00:00Z\nstate=armed\n";
    std::fs::write(session_dir(&dir).join("session-test-linux.marker"), marker).expect("write");

    assert!(
        matches!(scan_prior_sessions(&dir, None).as_slice(), [PriorSession::Unclean(marker)] if marker.platform == "linux"),
        "a safe build-target platform must not make an otherwise valid marker corrupt"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Reporting a marker clears it, so it is reported once.
#[test]
fn a_reported_marker_can_be_cleared() {
    let dir = scratch("clear");
    std::fs::create_dir_all(session_dir(&dir)).expect("session dir");
    let path = session_dir(&dir).join("session-once.marker");
    let marker = "id=test-once\npid=0\nversion=1.2.3\nplatform=macos\n\
                  started_at=2026-01-01T00:00:00Z\nstate=armed\n";
    std::fs::write(&path, marker).expect("write");

    assert_eq!(scan_prior_sessions(&dir, None).len(), 1);
    clear(&path).expect("clear");
    assert!(
        scan_prior_sessions(&dir, None).is_empty(),
        "a cleared marker must not be reported again on the next launch"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Scanning a directory that has never held a marker is not an error.
#[test]
fn scanning_a_fresh_install_finds_nothing() {
    let dir = scratch("fresh");
    assert!(scan_prior_sessions(&dir, None).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real killed process leaves a marker that the next launch finds.
///
/// Every other test here writes marker bytes directly, which proves the
/// parsing and the liveness check but not that a *killed process* leaves the
/// file behind. This spawns a child that arms a marker and then sleeps,
/// `SIGKILL`s it, and scans — the closest reachable analogue of the failure
/// this module exists for.
///
/// `SIGKILL` is uncatchable, so the child runs no cleanup and writes no dump.
/// That is the point: the marker is the only evidence, and this asserts it is
/// enough to detect the exit.
#[cfg(unix)]
#[test]
fn a_sigkilled_child_is_detected_on_the_next_launch() {
    let dir = scratch("sigkill");
    let helper = std::env::current_exe().expect("test binary path");

    let mut child = std::process::Command::new(&helper)
        // Run only the helper test, which arms a marker and then blocks.
        .arg("--exact")
        .arg("session_state::session_state_tests::sigkill_helper_arms_and_waits")
        .arg("--nocapture")
        .arg("--ignored")
        .env("SONICTERM_TEST_SESSION_DIR", &dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn helper");

    // Wait for the child to arm, bounded so a helper that never starts fails
    // the test rather than hanging the suite.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut armed = false;
    while std::time::Instant::now() < deadline {
        let markers = std::fs::read_dir(session_dir(&dir))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        if markers > 0 {
            armed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // SAFETY: `kill` targets a child this process spawned and owns.
    let killed = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
    let _ = child.wait();

    assert!(armed, "the helper never wrote a marker; the kill proved nothing");
    assert_eq!(killed, 0, "SIGKILL must reach the child");

    let prior = scan_prior_sessions(&dir, None);
    assert!(
        prior.iter().any(|entry| matches!(entry, PriorSession::Unclean(_))),
        "a SIGKILLed session must be detected as unclean on the next launch. SonicTerm cannot \
         write a dump for an uncatchable kill, so this marker is the only evidence there is: \
         {prior:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Helper for the kill test: arm a marker, then block until killed.
///
/// `#[ignore]` so it never runs in an ordinary pass — it is invoked by name,
/// with an environment variable naming the directory to write into. The sleep
/// is bounded so an orphaned helper exits on its own rather than lingering if
/// the parent dies before the kill.
#[cfg(unix)]
#[test]
#[ignore = "spawned by a_sigkilled_child_is_detected_on_the_next_launch"]
fn sigkill_helper_arms_and_waits() {
    let Some(dir) = std::env::var_os("SONICTERM_TEST_SESSION_DIR") else { return };
    let session = arm(Path::new(&dir), "1.2.3").expect("arm in helper");
    // Deliberately leaked: a clean mark here would defeat the test.
    std::mem::forget(session);
    std::thread::sleep(std::time::Duration::from_secs(60));
}
