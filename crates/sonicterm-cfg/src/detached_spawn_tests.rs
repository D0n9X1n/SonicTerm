use std::time::Instant;

use super::*;

/// Whether `pid` is still listed as a process, live or a zombie: `ps` shows a zombie until its
/// parent collects the exit status, and lists nothing for a pid that is gone.
#[cfg(unix)]
fn still_unreaped(pid: u32) -> bool {
    let listing = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output();
    listing.is_ok_and(|output| !output.stdout.iter().all(u8::is_ascii_whitespace))
}

/// Wait up to five seconds for `pid` to be collected.
#[cfg(unix)]
fn assert_collected(pid: u32, what: &str) {
    let start = Instant::now();
    while still_unreaped(pid) {
        assert!(start.elapsed() < Duration::from_secs(5), "{what} ({pid}) was never collected");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A helper still running does not hold up the others: with one `sleep` running, a helper that
/// exits at once is collected, and the sleeping one is collected once it ends. One reaper thread
/// owns them all, and none is killed for running long.
#[cfg(unix)]
#[test]
fn every_helper_is_collected_even_beside_one_that_keeps_running() {
    let mut long = Command::new("/bin/sleep");
    long.arg("30");
    let long_child = long.spawn().expect("spawn sleep");
    let long_pid = long_child.id();
    adopt(long_child);
    let quick = Command::new("/usr/bin/true").spawn().expect("spawn true");
    let quick_pid = quick.id();
    adopt(quick);
    assert_collected(quick_pid, "the helper that exited");
    assert!(still_unreaped(long_pid), "the running helper was collected or killed early");
    // The test ends the long helper itself; the reaper then collects it.
    let _ = Command::new("kill").arg(long_pid.to_string()).status();
    assert_collected(long_pid, "the helper that was still running");
}

/// Without a reaper, a helper that exits within the check window is still collected here.
#[cfg(unix)]
#[test]
fn a_helper_checked_inline_is_collected() {
    let quick = Command::new("/usr/bin/true").spawn().expect("spawn true");
    let pid = quick.id();
    collect_briefly(quick);
    assert!(!still_unreaped(pid), "the inline check left the helper unreaped");
}

/// `spawn_detached` returns as soon as the helper starts, before it exits, and reports a missing
/// program as a spawn error.
#[test]
fn spawn_returns_before_the_helper_exits_and_reports_a_missing_program() {
    #[cfg(unix)]
    let mut slow = {
        let mut command = Command::new("/bin/sleep");
        command.arg("2");
        command
    };
    #[cfg(windows)]
    let mut slow = {
        let mut command = Command::new("cmd");
        command.args(["/c", "ping", "-n", "3", "127.0.0.1"]);
        command
    };
    let start = Instant::now();
    spawn_detached(&mut slow).expect("spawn the slow helper");
    assert!(start.elapsed() < Duration::from_secs(1), "spawn_detached waited for the helper");
    let missing = spawn_detached(&mut Command::new("sonicterm-no-such-program"));
    assert!(missing.is_err());
}
