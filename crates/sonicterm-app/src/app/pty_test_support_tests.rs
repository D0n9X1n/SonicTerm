use super::*;

/// An isolated test deadline must kill its real PTY shell and retain the last phase and non-success exit.
#[test]
fn timeout_reaps_real_pty_and_reports_last_phase() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::timeout_reaps_real_pty_and_reports_last_phase";
    if is_test_child(NAME) {
        phase(0, "spawn-begin");
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
        #[cfg(unix)]
        let (program, args) = ("/bin/sh", vec!["-s".into()]);
        let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).unwrap();
        let pid = pty.pid().unwrap();
        record_process(pid, true);
        phase(pid as u64, "intentional-wait");
        std::thread::sleep(Duration::from_secs(8));
        phase(pid as u64, "drop-begin");
        drop(pty);
        record_process(pid, false);
        return;
    }
    let result = run_test_child(NAME, Duration::from_secs(3));
    assert!(result.timed_out, "deadline was not enforced: {}", result.diagnostic());
    assert!(!result.status.success(), "killed child reported success");
    assert!(
        result.elapsed < Duration::from_secs(7),
        "timeout did not bound the complete child lifecycle"
    );
    assert!(result.last_phase().contains("intentional-wait"), "{}", result.diagnostic());
    let pid = result
        .output
        .lines()
        .find_map(|line| line.strip_prefix("PTY_PROCESS start=")?.parse::<u32>().ok())
        .expect("child must report its real shell PID");
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_alive(pid),
        "PTY shell {pid} survived its test deadline: {}",
        process_detail(pid)
    );
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    // SAFETY: the test only waits on this queried process handle, then closes its owned handle.
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => {
                let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
                CloseHandle(handle).unwrap();
                running
            }
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => false,
            Err(error) => panic!("cannot inspect test shell {pid}: {error}"),
        }
    }
}

/// On Linux a killed shell counts as ended once it is a zombie: its test process is gone, and a container's PID 1
/// may never reap it.
#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    stat_shows_running(std::fs::read_to_string(format!("/proc/{pid}/stat")), pid)
}

/// Only a missing process, or a zombie or dead state, ends the shell; any other read error fails the check.
#[cfg(target_os = "linux")]
fn stat_shows_running(stat: std::io::Result<String>, pid: u32) -> bool {
    let stat = match stat {
        Ok(stat) => stat,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return false;
        }
        Err(error) => panic!("cannot inspect test shell {pid}: {error}"),
    };
    // The state letter follows the parenthesized command name, which can itself contain spaces or parentheses.
    let state = stat.rsplit_once(") ").and_then(|(_, rest)| rest.chars().next());
    !matches!(state, Some('Z' | 'X'))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_alive(pid: u32) -> bool {
    let result = {
        // SAFETY: signal zero queries the recorded shell PID without modifying a process.
        unsafe { libc::kill(pid as libc::pid_t, 0) }
    };
    if result == 0 {
        return true;
    }
    let error = std::io::Error::last_os_error();
    // Only a missing process counts as ended; any other error cannot prove termination.
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH), "cannot inspect test shell {pid}: {error}");
    false
}

/// A `/proc` read error other than a missing process must fail the deadline check instead of counting as an end.
#[cfg(target_os = "linux")]
#[test]
fn linux_shell_state_counts_only_absence_or_zombie_as_ended() {
    use std::io::{Error, ErrorKind};
    assert!(!stat_shows_running(Err(Error::from(ErrorKind::NotFound)), 7));
    assert!(!stat_shows_running(Err(Error::from_raw_os_error(libc::ESRCH)), 7));
    assert!(!stat_shows_running(Ok("7 (sh) Z 1 7".into()), 7));
    assert!(stat_shows_running(Ok("7 (a) b) S 1 7".into()), 7));
    let unreadable = std::panic::catch_unwind(|| {
        stat_shows_running(Err(Error::from_raw_os_error(libc::EMFILE)), 7)
    });
    assert!(unreadable.is_err(), "an unreadable stat file counted as an ended shell");
}

/// The shell's kernel state line on Linux, so a survival failure shows whether the process still runs.
#[cfg(target_os = "linux")]
fn process_detail(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_else(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
fn process_detail(_pid: u32) -> String {
    String::new()
}
