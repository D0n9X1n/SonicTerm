//! Tests for `os_drag_bridge` push/drain queues.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/os_drag_bridge.rs`.
//! These tests use the `__test_drain_*` test bridges to avoid needing
//! access to the crate-private `drain_*` helpers.

use std::path::PathBuf;

use sonicterm_app::os_drag::TabPayload;
use sonicterm_app::os_drag_bridge::{
    __test_drain_files, __test_drain_tabs, push_files, push_tab_payload,
};

fn mk(pid: i32) -> TabPayload {
    TabPayload {
        pty_pid: pid,
        tab_title: String::new(),
        scrollback_b64: String::new(),
        cwd: String::new(),
        cmd: String::new(),
        env: vec![],
    }
}

#[test]
fn tab_queue_drains_in_fifo_order() {
    let _ = __test_drain_tabs();
    push_tab_payload(mk(1));
    push_tab_payload(mk(2));
    let out = __test_drain_tabs();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].pty_pid, 1);
    assert_eq!(out[1].pty_pid, 2);
    assert!(__test_drain_tabs().is_empty());
}

#[test]
fn file_queue_drops_empty_and_drains_in_order() {
    let _ = __test_drain_files();
    assert!(!push_files(vec![]));
    push_files(vec![PathBuf::from("/a"), PathBuf::from("/b c")]);
    push_files(vec![PathBuf::from("/d")]);
    let out = __test_drain_files();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].len(), 2);
    assert_eq!(out[1][0], PathBuf::from("/d"));
    assert!(__test_drain_files().is_empty());
}
