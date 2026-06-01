//! End-to-end smoke for `install_panic_hook`: install it, panic on a
//! background thread (the silent-exit class of bug we're plugging),
//! and assert a `crash-*.log` file lands in the configured crash dir
//! with the panic payload + thread name.
//!
//! This is the only test that exercises the real
//! `std::panic::set_hook` path; the other crash_dump tests poke the
//! ring + dump writer directly to keep the panic runtime out of the
//! parallel test process.

use std::fs;
use std::panic;
use std::sync::{Arc, Barrier};

use sonicterm_logging::install_panic_hook;
use tempfile::tempdir;

/// Serial guard: `std::panic::set_hook` is process-global; running
/// this test in parallel with anything else that swaps the hook
/// would race. The other crash_dump tests use `__test_serial` for
/// the ring; this test stands on its own because it does not touch
/// the ring.
fn install_and_panic_in_thread(crash_dir: &std::path::Path, message: &str) {
    install_panic_hook(crash_dir.to_path_buf());

    let barrier = Arc::new(Barrier::new(2));
    let b = barrier.clone();
    let msg = message.to_string();
    let h = std::thread::Builder::new()
        .name("sonic-test-panic".to_string())
        .spawn(move || {
            b.wait();
            // catch_unwind so the test harness doesn't itself abort
            // — the hook still fires before unwind begins.
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                panic!("{msg}");
            }));
        })
        .unwrap();
    barrier.wait();
    h.join().unwrap();
}

#[test]
fn panic_on_background_thread_writes_crash_file() {
    let dir = tempdir().unwrap();
    let crashes = dir.path().join("crashes");
    install_and_panic_in_thread(&crashes, "synthetic-panic-from-bg-thread");

    // Find the crash-*.log file the hook wrote.
    let entries: Vec<_> = fs::read_dir(&crashes)
        .expect("crash dir should exist")
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with("crash-")
                && e.file_name().to_string_lossy().ends_with(".log")
        })
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least one crash-*.log file under {}",
        crashes.display()
    );

    let body = fs::read_to_string(entries[0].path()).unwrap();
    assert!(body.contains("sonic crash dump"), "dump header missing: {body}");
    assert!(body.contains("synthetic-panic-from-bg-thread"), "payload missing: {body}");
    assert!(body.contains("sonic-test-panic"), "thread name missing: {body}");
    assert!(body.contains("== backtrace =="), "backtrace section missing");
}
