//! Crash-dump capture tests — push synthetic events into the ring,
//! invoke the dump writer, and assert content + location.

use std::fs;

use sonic_logging::crash::{__test_push, __test_write_dump, RING_CAPACITY};
use tempfile::tempdir;
use tracing::Level;

#[test]
fn dump_includes_ring_events_and_message() {
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
