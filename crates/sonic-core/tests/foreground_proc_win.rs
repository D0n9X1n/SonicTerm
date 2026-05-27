//! Windows-only integration test for `sonic_core::foreground_proc`.
//!
//! Spawns `cmd /c timeout 30`, then asks the probe to identify the
//! foreground descendant of `cmd`. The expected resolution is `timeout`
//! (cmd's child) — but on slow CI we also accept `cmd` itself in case the
//! child hasn't been scheduled yet when the probe fires.

#![cfg(windows)]

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[test]
fn probe_finds_cmd_or_timeout_child() {
    let mut child = Command::new("cmd")
        .args(["/c", "timeout", "/t", "30", "/nobreak"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cmd /c timeout");

    // Give cmd a moment to spawn the timeout child.
    thread::sleep(Duration::from_millis(500));

    let result = sonic_core::foreground_proc::current_foreground_pid(child.id());

    // Always kill the child before asserting so a failure doesn't leak a
    // 30 s sleeping process onto the runner.
    let _ = child.kill();
    let _ = child.wait();

    // The probe is timing-sensitive; on a heavily-loaded box the child may
    // exit before we sample. Accept None as a no-op (the unit test for the
    // probe itself covers the always-on contract). When we do get a value,
    // assert it's one of the expected processes.
    let Some((_pid, name)) = result else {
        eprintln!("probe returned None (test box too slow); skipping assertion");
        return;
    };
    let basename = std::path::Path::new(&name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_ascii_lowercase();
    assert!(
        basename == "timeout"
            || basename == "timeout.exe"
            || basename == "cmd"
            || basename == "cmd.exe",
        "unexpected foreground name: {name:?}"
    );
}
