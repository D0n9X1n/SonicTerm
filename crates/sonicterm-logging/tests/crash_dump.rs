//! Crash-dump capture tests — push synthetic events into the ring,
//! invoke the dump writer, and assert content + location.
//!
//! The ring buffer is a process-global `static`; both tests in this
//! file mutate it. Cargo runs integration tests in a binary
//! concurrently by default, which pre-v0.8.1 caused
//! `dump_includes_ring_events_and_message` to flake when
//! `ring_caps_at_capacity` evicted the `alpha`/`beta` entries before
//! the former finished reading. Each test now takes the
//! `__test_serial` guard and `__test_reset`s the ring at entry to
//! keep assertions deterministic.

use std::fs;

use sonicterm_logging::crash::{
    __test_push, __test_reset, __test_serial, __test_write_dump, RING_CAPACITY,
};
use tempfile::tempdir;
use tracing::Level;

#[test]
fn dump_includes_ring_events_and_message() {
    let _serial = __test_serial();
    __test_reset();
    let dir = tempdir().unwrap();
    __test_push(Level::INFO, "sonic_test", "alpha event");
    __test_push(Level::WARN, "sonic_test", "beta event");
    let path = __test_write_dump(dir.path(), "boom").unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("sonic crash dump"));
    assert!(body.contains("boom"), "dump should embed the panic message");
    assert!(body.contains("alpha event"), "dump should include ring events");
    assert!(body.contains("beta event"));
    assert!(body.contains("sonic_test"));
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("crash-") && name.ends_with(".log"));
}

#[test]
fn ring_caps_at_capacity() {
    let _serial = __test_serial();
    __test_reset();
    // Push more than RING_CAPACITY events; oldest must be evicted.
    for i in 0..(RING_CAPACITY + 50) {
        __test_push(Level::INFO, "sonic_test_cap", &format!("evt-{i}"));
    }
    let dir = tempdir().unwrap();
    let path = __test_write_dump(dir.path(), "cap-test").unwrap();
    let body = fs::read_to_string(&path).unwrap();
    // The very first event is well beyond the cap by now.
    assert!(!body.contains("evt-0 "), "ring should have evicted evt-0");
    // The most recent event must be present.
    assert!(body.contains(&format!("evt-{}", RING_CAPACITY + 49)));
}
