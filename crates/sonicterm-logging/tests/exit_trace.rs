//! Integration tests for `sonicterm_logging::exit_trace`.
//!
//! Every test spawns the `exit_test_child` example binary with
//! `SONIC_EXIT_TEST_MODE` set, points it at a fresh tempdir via
//! `SONIC_LOG_DIR`, then asserts on the contents of `sonicterm.log` and
//! `crashes/` after the child terminates.
//!
//! These tests are non-trivial because they exercise process-wide
//! global state (panic hook, signal handlers, drop guards) that
//! cannot be tested in-process — installing a SIGSEGV handler in the
//! `cargo test` driver would prevent panic-style assertion failures
//! from producing useful output.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn child_binary() -> PathBuf {
    // Build the example via `cargo` in case `cargo test` hasn't
    // already done so. `--message-format=json` would let us read the
    // path back; the simpler route is `cargo build --example` and
    // then locate the produced binary under target/.
    let status = Command::new(env!("CARGO"))
        .args(["build", "--example", "exit_test_child", "--package", "sonicterm-logging"])
        .status()
        .expect("cargo build --example");
    assert!(status.success(), "cargo build --example failed");

    // CARGO_MANIFEST_DIR is the crate dir; walk up to workspace root,
    // then into target/debug/examples.
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();
    let exe_name = if cfg!(windows) { "exit_test_child.exe" } else { "exit_test_child" };
    workspace.join("target/debug/examples").join(exe_name)
}

fn run_child(mode: &str, log_dir: &Path) -> std::process::Output {
    run_child_with_env(mode, log_dir, &[])
}

fn run_child_with_env(mode: &str, log_dir: &Path, env: &[(&str, &str)]) -> std::process::Output {
    let bin = child_binary();
    let mut cmd = Command::new(bin);
    cmd.env("SONIC_EXIT_TEST_MODE", mode).env("SONIC_LOG_DIR", log_dir).env_remove("RUST_LOG");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn child")
}

fn read_all_logs(dir: &Path) -> String {
    let mut buf = String::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file()
                && p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("sonicterm.log"))
                    .unwrap_or(false)
            {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    buf.push_str(&s);
                }
            }
        }
    }
    buf
}

fn wait_for<F: Fn() -> bool>(deadline: Duration, f: F) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

#[test]
fn clean_main_return_logs_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child("clean", tmp.path());
    assert!(out.status.success(), "child should exit 0, got {:?}", out.status);
    wait_for(Duration::from_secs(2), || {
        read_all_logs(tmp.path()).contains("sonic exiting: clean main return")
    });
    let logs = read_all_logs(tmp.path());
    assert!(
        logs.contains("sonic exiting: clean main return"),
        "expected clean-exit marker, got:\n{logs}"
    );
}

#[test]
fn clean_main_return_logs_marker_under_default_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child("clean", tmp.path());
    assert!(out.status.success(), "child should exit 0, got {:?}", out.status);
    wait_for(Duration::from_secs(2), || {
        read_all_logs(tmp.path()).contains("sonic exiting: clean main return")
    });
    let logs = read_all_logs(tmp.path());
    assert!(
        logs.contains("sonic exiting: clean main return"),
        "expected default-filter clean-exit marker, got:\n{logs}"
    );
}

#[test]
fn clean_main_return_survives_strict_sonic_exit_target_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child_with_env(
        "target_filter_clean",
        tmp.path(),
        &[("SONIC_EXIT_TEST_FILTER", "sonic_exit=warn")],
    );
    assert!(out.status.success(), "child should exit 0, got {:?}", out.status);
    wait_for(Duration::from_secs(2), || {
        read_all_logs(tmp.path()).contains("sonic exiting: clean main return")
    });
    let logs = read_all_logs(tmp.path());
    assert!(
        logs.contains("sonic exiting: clean main return"),
        "expected clean-exit marker to survive sonic_exit=warn filter, got:\n{logs}"
    );
    assert!(
        !logs.contains("filtered clean child: returning normally"),
        "sonic_exit INFO control line should be filtered while WARN exit marker survives:\n{logs}"
    );
}

#[test]
fn panic_on_main_thread_writes_crash_file() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child("panic_main", tmp.path());
    assert!(!out.status.success(), "panic child should exit non-zero");
    let crashes = tmp.path().join("crashes");
    wait_for(Duration::from_secs(2), || {
        crashes.exists() && std::fs::read_dir(&crashes).map(|r| r.count() > 0).unwrap_or(false)
    });
    let n = std::fs::read_dir(&crashes).map(|r| r.count()).unwrap_or(0);
    assert!(n > 0, "expected at least one crash file in {}", crashes.display());
}

#[test]
fn panic_on_spawned_thread_writes_crash_file() {
    let tmp = tempfile::tempdir().unwrap();
    let _out = run_child("panic_thread", tmp.path());
    // Thread panics don't abort by default — but our panic hook still
    // fires on every thread, so a crash file must exist.
    let crashes = tmp.path().join("crashes");
    wait_for(Duration::from_secs(2), || {
        crashes.exists() && std::fs::read_dir(&crashes).map(|r| r.count() > 0).unwrap_or(false)
    });
    let n = std::fs::read_dir(&crashes).map(|r| r.count()).unwrap_or(0);
    assert!(n > 0, "expected crash file from spawned-thread panic");
}

#[cfg(unix)]
#[test]
fn segv_writes_signal_marker_to_log() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child("segv", tmp.path());
    assert!(!out.status.success(), "segv child should exit non-zero");
    wait_for(Duration::from_secs(2), || read_all_logs(tmp.path()).contains("FATAL: SIGSEGV"));
    let logs = read_all_logs(tmp.path());
    assert!(
        logs.contains("FATAL: SIGSEGV"),
        "expected SIGSEGV marker, got:\n{logs}\n--- stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn raw_process_exit_does_not_drop_guard() {
    // Documents the gap: uninstrumented std::process::exit(3) skips
    // the drop guard, so no "clean exit" line is written. The CI
    // grep gate (scripts/check-no-raw-process-exit.sh) is what
    // prevents production code from taking this path.
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child("exit3", tmp.path());
    assert_eq!(out.status.code(), Some(3));
    let logs = read_all_logs(tmp.path());
    assert!(
        !logs.contains("sonic exiting: clean main return"),
        "raw process::exit SHOULD bypass the drop guard, but it didn't:\n{logs}"
    );
}

#[test]
fn exit_with_helper_logs_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_child("exit_with", tmp.path());
    assert_eq!(out.status.code(), Some(4));
    wait_for(Duration::from_secs(2), || read_all_logs(tmp.path()).contains("test-explicit-exit"));
    let logs = read_all_logs(tmp.path());
    assert!(logs.contains("test-explicit-exit"), "expected exit_with reason in log, got:\n{logs}");
}
