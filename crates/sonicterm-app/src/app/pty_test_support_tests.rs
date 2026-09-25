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
        #[cfg(unix)]
        {
            // The shell is a separate PTY session; reap its exit before the timeout kills this test's group.
            let probe = pty.child_exit_probe();
            std::thread::spawn(move || {
                while !probe.has_exited().unwrap() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }
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
    assert!(!process_alive(pid), "PTY shell {pid} survived its test deadline");
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

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal zero queries the recorded shell PID without modifying a process.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
