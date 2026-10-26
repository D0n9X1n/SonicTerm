use super::*;
use crate::pty::pty_test_support::*;
use portable_pty::{ChildKiller, ExitStatus};

#[derive(Debug, Clone)]
struct MockChild {
    try_wait_calls: Arc<AtomicUsize>,
    kill_calls: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<&'static str>>>,
    running: bool,
    process_id: Option<u32>,
    kill_error: Option<std::io::ErrorKind>,
}

impl ChildKiller for MockChild {
    fn kill(&mut self) -> std::io::Result<()> {
        self.kill_calls.fetch_add(1, Ordering::Relaxed);
        self.events.lock().push("kill");
        match self.kill_error {
            Some(kind) => Err(std::io::Error::new(kind, "mock kill failed")),
            None => Ok(()),
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl Child for MockChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.try_wait_calls.fetch_add(1, Ordering::Relaxed);
        self.events.lock().push("wait");
        Ok((!self.running).then(|| ExitStatus::with_exit_code(0)))
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        Ok(ExitStatus::with_exit_code(0))
    }

    fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

#[cfg(any(unix, windows))]
#[test]
fn child_exit_probe_observes_short_lived_process() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    #[cfg(unix)]
    let command = "/usr/bin/true";
    #[cfg(windows)]
    let command = "whoami.exe";

    let pty = PtyHandle::spawn(command, 80, 24).expect("spawn short-lived process");
    let probe = pty.child_exit_probe();
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut output = Vec::new();
    while !probe.has_exited().expect("probe child") && std::time::Instant::now() < deadline {
        while let Ok(chunk) = pty.out_rx.try_recv() {
            output.extend_from_slice(&chunk);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(
        probe.has_exited().expect("final child probe"),
        "short-lived child remained active; output={output:?}"
    );
}

/// A clean exit and a failing one must be distinguishable.
///
/// The probe reporting only liveness is what left a pane unable to decide
/// whether to close: `exit` and a crash look identical through a bool, and a
/// close policy built on that would discard a crashed shell's output before
/// the user could read it.
///
/// Asserted as a pair. Checking only the clean case would pass against an
/// accessor hardcoded to `Some(true)`, which is exactly the failure that
/// would destroy scrollback.
#[test]
fn the_exit_probe_distinguishes_a_clean_exit_from_a_failing_one() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();

    // `spawn` takes a program path, not a command line — passing
    // "cmd.exe /c exit 0" makes Windows look for a binary with that literal
    // name. Arguments go through `spawn_with_args`.
    fn observe(command: &str, args: &[String]) -> Option<bool> {
        let pty =
            PtyHandle::spawn_with_args(command, args, 80, 24).expect("spawn short-lived process");
        let probe = pty.child_exit_probe();
        #[cfg(windows)]
        pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !probe.has_exited().expect("probe child") && std::time::Instant::now() < deadline {
            while pty.out_rx.try_recv().is_ok() {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(probe.has_exited().expect("final probe"), "{command} did not exit in time");
        probe.exit_was_clean()
    }

    #[cfg(unix)]
    let (clean, clean_args, failing, failing_args) =
        ("/usr/bin/true", Vec::<String>::new(), "/usr/bin/false", Vec::<String>::new());
    #[cfg(windows)]
    let (clean, clean_args, failing, failing_args) = (
        "cmd.exe",
        vec!["/c".to_string(), "exit".to_string(), "0".to_string()],
        "cmd.exe",
        vec!["/c".to_string(), "exit".to_string(), "1".to_string()],
    );

    assert_eq!(observe(clean, &clean_args), Some(true), "a zero exit must read as clean");
    assert_eq!(
        observe(failing, &failing_args),
        Some(false),
        "a nonzero exit must read as not clean"
    );
}

/// A clean child exit must become observable while the pane holds its handle.
///
/// This is the contract the pane close policy rests on. It is deliberately
/// stated as "observable", not "the channel disconnects", because the two
/// platforms satisfy it by different signals and an earlier version of this
/// test asserted the unix one as though it were universal:
///
/// * **unix** — the reader reaches EOF once the child's last slave fd closes,
///   so `out_rx` disconnects on its own and the loop needs no timer.
/// * **Windows** — the ConPTY master is held open by our `PtyHandle`, whose
///   `HPCON` is released only when that handle drops, which happens when the
///   pane closes. The channel therefore never disconnects while the pane
///   lives, and the loop must poll the exit probe instead.
///
/// Asserting the disconnect alone made this test fail on Windows for a true
/// reason and would have kept failing after the fix, because the fix does not
/// make the channel disconnect — it gives the loop a second way to notice.
///
/// The handle is held for the whole drain on purpose. Dropping it early tears
/// the pty down and proves nothing about the natural path, which is the only
/// case that matters here.
#[test]
fn a_clean_child_exit_becomes_observable_while_the_handle_lives() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    #[cfg(unix)]
    let (command, args) = ("/bin/sh", vec!["-c".to_string(), "exit 0".to_string()]);
    #[cfg(windows)]
    let (command, args) = ("cmd.exe", vec!["/c".to_string(), "exit".to_string(), "0".to_string()]);

    let pty = PtyHandle::spawn_with_args(command, &args, 80, 24).expect("spawn short-lived shell");
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    let probe = pty.child_exit_probe();

    // Drain as the VT worker does, checking both signals it can act on, and
    // keep the handle alive throughout — `pty` outlives this loop.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut signal: Option<&'static str> = None;
    let mut bytes = 0usize;
    while std::time::Instant::now() < deadline {
        match pty.out_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => bytes += chunk.len(),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                signal = Some("channel disconnect");
                break;
            }
        }
        if probe.has_exited().expect("probe the child") {
            signal = Some("exit probe");
            break;
        }
    }

    let signal = signal.unwrap_or_else(|| {
        panic!(
            "a clean child exit was still unobservable 10s later ({bytes} bytes read) with the \
             handle held: neither the output channel nor the exit probe reported it, so a pane \
             waiting on either would never learn its shell had gone"
        )
    });

    // Record the status the way production does. `exit_was_clean` reports
    // what a prior `has_exited` observed — it is an accessor, not a probe — so
    // reading it straight after a channel disconnect returns `None` for a
    // shell that exited perfectly well. The VT worker runs this same bounded
    // poll before classifying, because EOF and the child becoming reapable are
    // unordered.
    let status_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !probe.has_exited().expect("probe the child")
        && std::time::Instant::now() < status_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }

    // The status must survive the observation, whichever signal produced it —
    // the close policy reads it to decide whether the pane may close at all.
    assert_eq!(
        probe.exit_was_clean(),
        Some(true),
        "the exit was observed via {signal} but its status did not survive; a pane cannot \
         distinguish `exit` from a crash without it"
    );
    drop(pty);
}

#[cfg(unix)]
#[test]
fn observed_shell_exit_still_kills_background_process_group() {
    // Protect teardown from leaving a runnable descendant behind when the shell leader exits first.
    let args = vec!["-c".to_string(), "trap '' HUP; sleep 30 & echo $!; exit 0".to_string()];
    let pty = PtyHandle::spawn_with_args("/bin/sh", &args, 80, 24).expect("spawn shell");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut output = Vec::new();
    while !output.contains(&b'\n') && std::time::Instant::now() < deadline {
        match pty.out_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("PTY output closed before background pid")
            }
        }
    }
    let background_pid = String::from_utf8_lossy(&output)
        .trim()
        .parse::<libc::pid_t>()
        .expect("numeric background pid");
    let probe = pty.child_exit_probe();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !probe.has_exited().expect("probe shell") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(probe.has_exited().expect("shell exited"));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while unix_process_is_active(background_pid as u32).expect("probe background process")
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !unix_process_is_active(background_pid as u32).expect("background process terminated"),
        "the background process remained runnable after its shell leader exited"
    );
    drop(pty);
}

#[test]
fn explicit_kill_returns_termination_failure() {
    let try_wait_calls = Arc::new(AtomicUsize::new(0));
    let kill_calls = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let child = MockChild {
        try_wait_calls: try_wait_calls.clone(),
        kill_calls: kill_calls.clone(),
        events: events.clone(),
        running: true,
        process_id: None,
        kill_error: Some(std::io::ErrorKind::PermissionDenied),
    };
    let mut child = ChildState::new(Box::new(child), None);

    let error = terminate_child_for_platform(&mut child)
        .expect_err("explicit kill must surface termination failure");

    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "mock kill failed");
    assert_eq!(try_wait_calls.load(Ordering::Relaxed), 1);
    assert_eq!(kill_calls.load(Ordering::Relaxed), 1);
    assert_eq!(*events.lock(), ["wait", "kill"]);
}

#[test]
fn termination_signals_group_before_reaping_exited_leader() {
    let try_wait_calls = Arc::new(AtomicUsize::new(0));
    let kill_calls = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let child = MockChild {
        try_wait_calls: try_wait_calls.clone(),
        kill_calls: kill_calls.clone(),
        events: events.clone(),
        running: false,
        process_id: Some(4242),
        kill_error: None,
    };
    let child = Arc::new(Mutex::new(ChildState::new(Box::new(child), Some(4242))));

    let mut signalled_groups = Vec::new();
    let mut signalled_pids = Vec::new();
    terminate_child(
        &mut child.lock(),
        |pgid| {
            events.lock().push("group");
            signalled_groups.push(pgid);
            Ok(())
        },
        |pid| signalled_pids.push(pid),
    )
    .unwrap();
    terminate_child(
        &mut child.lock(),
        |pgid| {
            signalled_groups.push(pgid);
            Ok(())
        },
        |pid| signalled_pids.push(pid),
    )
    .unwrap();

    assert_eq!(signalled_groups, [4242]);
    assert!(signalled_pids.is_empty());
    assert_eq!(kill_calls.load(Ordering::Relaxed), 0);
    assert_eq!(try_wait_calls.load(Ordering::Relaxed), 1);
    assert_eq!(*events.lock(), ["group", "wait"]);
}

#[test]
fn failed_session_cleanup_remains_retryable() {
    let child = MockChild {
        try_wait_calls: Arc::new(AtomicUsize::new(0)),
        kill_calls: Arc::new(AtomicUsize::new(0)),
        events: Arc::new(Mutex::new(Vec::new())),
        running: false,
        process_id: Some(4242),
        kill_error: None,
    };
    let mut child = ChildState::new(Box::new(child), Some(4242));

    let first = signal_process_group(&mut child, |_| {
        Err(std::io::Error::other("process table unavailable"))
    });
    assert!(first.is_err());
    assert!(!child.process_group_signalled);

    signal_process_group(&mut child, |_| Ok(())).unwrap();
    assert!(child.process_group_signalled);
}

/// A mock leader that reports `running` and identifies as pid 4242, with fresh counters.
fn mock_leader(running: bool, process_id: Option<u32>) -> (MockChild, Arc<AtomicUsize>) {
    let try_wait_calls = Arc::new(AtomicUsize::new(0));
    let child = MockChild {
        try_wait_calls: try_wait_calls.clone(),
        kill_calls: Arc::new(AtomicUsize::new(0)),
        events: Arc::new(Mutex::new(Vec::new())),
        running,
        process_id,
        kill_error: None,
    };
    (child, try_wait_calls)
}

/// A pending peek publishes exit before the consuming wait runs: another thread that looks
/// while the wait is in progress already sees `exit_observed`, so no reader can mistake a
/// released identity for a live child.
#[test]
fn pending_peek_publishes_exit_before_the_consuming_wait() {
    let (leader, try_wait_calls) = mock_leader(false, Some(4242));
    let mut child = ChildState::new(Box::new(leader), None);
    let flag = child.exit_observed.clone();
    let mut seen_during_wait = None;

    let exited = child
        .has_exited_with(
            |_| Ok(Some(true)),
            |native| {
                // Pause inside the consuming wait and observe from another thread.
                seen_during_wait = Some(
                    std::thread::scope(|scope| scope.spawn(|| flag.load(Ordering::Acquire)).join())
                        .unwrap(),
                );
                native.try_wait()
            },
        )
        .unwrap();

    assert!(exited);
    assert_eq!(seen_during_wait, Some(true), "exit was published before the wait");
    assert_eq!(try_wait_calls.load(Ordering::Relaxed), 1);
    assert_eq!(child.exit_status, Some(PaneExitStatus::Code(0)));
}

/// A live child's peek reports nothing pending, so no consuming wait runs and exit stays
/// unpublished: a live child is never marked exited.
#[test]
fn live_peek_never_waits_or_publishes() {
    let (leader, try_wait_calls) = mock_leader(true, Some(4242));
    let mut child = ChildState::new(Box::new(leader), None);

    let exited = child
        .has_exited_with(|_| Ok(Some(false)), |_| panic!("a live child must not be waited"))
        .unwrap();

    assert!(!exited);
    assert!(!child.exit_observed.load(Ordering::Acquire));
    assert_eq!(try_wait_calls.load(Ordering::Relaxed), 0);
}

/// Where no peek exists (the Windows shape, where a wait releases nothing), the wait decides:
/// a running child stays unpublished and a reported exit is published before returning.
#[test]
fn unpeekable_wait_publishes_only_on_a_reported_exit() {
    for running in [true, false] {
        let (leader, try_wait_calls) = mock_leader(running, Some(4242));
        let mut child = ChildState::new(Box::new(leader), None);

        let exited = child.has_exited_with(|_| Ok(None), |native| native.try_wait()).unwrap();

        assert_eq!(exited, !running);
        assert_eq!(child.exit_observed.load(Ordering::Acquire), !running);
        assert_eq!(try_wait_calls.load(Ordering::Relaxed), 1);
    }
}

/// Explicit termination publishes exit at entry, before the group signal, the direct-pid
/// signal and the wait; the reap-free terminator publishes too.
#[test]
fn termination_publishes_exit_before_signalling_or_waiting() {
    let (leader, _) = mock_leader(true, Some(4242));
    let mut child = ChildState::new(Box::new(leader), Some(4242));
    let flag = child.exit_observed.clone();
    let (mut at_group, mut at_pid) = (Vec::new(), Vec::new());

    terminate_child(
        &mut child,
        |_| {
            at_group.push(flag.load(Ordering::Acquire));
            Ok(())
        },
        |_| at_pid.push(flag.load(Ordering::Acquire)),
    )
    .unwrap();

    assert_eq!((at_group, at_pid), (vec![true], vec![true]), "both signals follow publication");

    let (leader, _) = mock_leader(true, None);
    let mut child = ChildState::new(Box::new(leader), None);
    // The Windows mock has no retained handle, so only the publication is under test here.
    let _ = terminate_child_without_reap(&mut child);
    assert!(child.exit_observed.load(Ordering::Acquire));
}

/// A long-lived shell for the publication tests below.
#[cfg(any(unix, windows))]
fn spawn_live_shell() -> PtyHandle {
    #[cfg(unix)]
    return PtyHandle::spawn_with_args("/bin/sh", &["-s".into()], 80, 24).expect("spawn shell");
    #[cfg(windows)]
    return PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24)
        .expect("spawn shell");
}

/// A running shell is not exited; `kill`, `into_teardown` and a direct drop each publish
/// exit before the child's identity can be released.
#[cfg(any(unix, windows))]
#[test]
fn kill_teardown_and_drop_publish_exit_for_a_live_child() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    for path in ["kill", "teardown", "drop"] {
        let pty = spawn_live_shell();
        let observed = pty.exit_observed();
        assert!(!observed.is_exited(), "{path}: a running shell is not exited");
        match path {
            "kill" => {
                pty.kill().expect("kill the shell");
                assert!(observed.is_exited(), "kill publishes");
                drop(pty);
            }
            "teardown" => {
                let teardown = pty.into_teardown();
                assert!(observed.is_exited(), "into_teardown publishes before custody moves");
                drop(teardown);
            }
            _ => {
                drop(pty);
                assert!(observed.is_exited(), "drop publishes");
            }
        }
    }
}

/// The exit probe publishes a natural exit when it observes it, before signalling the group.
#[cfg(any(unix, windows))]
#[test]
fn exit_probe_publishes_a_natural_exit() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    #[cfg(unix)]
    let command = "/usr/bin/true";
    #[cfg(windows)]
    let command = "whoami.exe";
    let pty = PtyHandle::spawn(command, 80, 24).expect("spawn short-lived process");
    let observed = pty.exit_observed();
    let probe = pty.child_exit_probe();
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !probe.has_exited().expect("probe child") && std::time::Instant::now() < deadline {
        while pty.out_rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(probe.has_exited().expect("final probe"), "the short-lived child exited");
    assert!(observed.is_exited(), "the probe published the exit it observed");
}

/// The spawn captures identity once from the retained child: macOS and Windows report the
/// shell's pid with a nonzero start token (Windows from the retained handle, never a pid
/// reopen); other platforms report none, so they start no foreground worker.
#[cfg(any(unix, windows))]
#[test]
fn spawn_captures_process_identity_from_the_retained_child() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let pty = spawn_live_shell();
    let identity = pty.process_identity();
    #[cfg(any(target_os = "macos", windows))]
    {
        let identity = identity.expect("identity captured at spawn");
        assert_eq!(Some(identity.pid), pty.pid());
        assert_ne!(identity.start, 0);
        #[cfg(target_os = "macos")]
        assert_eq!(crate::proc_info::process_start_token(identity.pid), Some(identity.start));
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    assert!(identity.is_none());
}

/// In every path that can release the child's identity, `publish_exit` comes first in the
/// source, and the only consuming wait in `pty.rs` is the one `has_exited` hands its helper.
#[test]
fn exit_publication_precedes_every_identity_release_in_source() {
    let source = PTY_SOURCE.replace("\r\n", "\n");
    for (signature, release) in [
        ("fn has_exited_with<", "wait(native)"),
        (
            "pub fn has_exited(&self) -> Result<bool>",
            "signal_process_group_for_platform(&mut child)",
        ),
        ("fn terminate_child<G, P>(", "signal_process_group(child"),
        ("fn terminate_child_without_reap(", "child.exited"),
        ("pub fn kill(&self)", "terminate_child_for_platform("),
        ("pub fn into_teardown(", "closing.store(true"),
        ("impl Drop for PtyHandle", "drop(teardown)"),
    ] {
        let start = source.find(signature).unwrap_or_else(|| panic!("missing {signature}"));
        let body = &source[start..];
        let publish = body.find("publish_exit").unwrap_or_else(|| panic!("{signature} publishes"));
        let released = body.find(release).unwrap_or_else(|| panic!("{signature}: {release}"));
        assert!(publish < released, "{signature}: publish_exit must precede {release}");
    }
    let code = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(code.matches(".try_wait()").count(), 1, "one consuming wait site");
    assert!(!code.contains(".wait()"), "no blocking wait releases the child");
    let only_wait = code.find(".try_wait()").unwrap();
    let owner = code[..only_wait].rfind("fn ").unwrap();
    assert!(code[owner..].starts_with("fn has_exited(&mut self)"));
}

/// Spawn `command` with `args`, wait for it to exit, and return its typed status.
fn observe_exit_status(command: &str, args: &[String]) -> Option<PaneExitStatus> {
    let pty = PtyHandle::spawn_with_args(command, args, 80, 24).expect("spawn short-lived process");
    let probe = pty.child_exit_probe();
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !probe.has_exited().expect("probe child") && std::time::Instant::now() < deadline {
        while pty.out_rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(probe.has_exited().expect("final probe"), "{command} did not exit in time");
    probe.exit_status()
}

/// A nonzero exit code reaches the probe as that exact code, not just "unclean".
#[test]
fn the_exit_probe_reports_the_exit_code() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-c".to_string(), "exit 3".to_string()]);
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/c".to_string(), "exit".to_string(), "3".to_string()]);
    assert_eq!(observe_exit_status(program, &args), Some(PaneExitStatus::Code(3)));
}

/// A signal death reaches the probe as the signal's conventional name.
#[cfg(unix)]
#[test]
fn the_exit_probe_reports_the_killing_signal() {
    let args = vec!["-c".to_string(), "kill -KILL $$".to_string()];
    assert_eq!(
        observe_exit_status("/bin/sh", &args),
        Some(PaneExitStatus::Signal("SIGKILL".to_string()))
    );
}

/// The first observed status wins, so a later consuming wait cannot rewrite the `waitid` record.
#[test]
fn a_recorded_exit_status_is_not_overwritten() {
    let (leader, _calls) = mock_leader(false, Some(4242));
    let mut child = ChildState::new(Box::new(leader), None);
    child.record_exit_status(Some(PaneExitStatus::Signal("SIGTERM".to_string())));
    child.record_exit_status(Some(PaneExitStatus::Code(0)));
    assert_eq!(child.exit_status, Some(PaneExitStatus::Signal("SIGTERM".to_string())));
}

/// Unlisted Unix signals keep their number instead of borrowing a wrong name.
#[cfg(unix)]
#[test]
fn unlisted_signals_are_named_by_number() {
    assert_eq!(unix_signal_name(libc::SIGKILL), "SIGKILL");
    assert_eq!(unix_signal_name(250), "signal 250");
}
