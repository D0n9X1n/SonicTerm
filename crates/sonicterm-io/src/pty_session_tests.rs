use super::*;

/// A descendant leaving during the last bounded pause must be observed before reporting failed termination.
#[test]
fn session_termination_observes_the_last_signal_before_failing() {
    let pauses = std::cell::Cell::new(0);
    let scans = std::cell::Cell::new(0);
    let mut signals = Vec::new();
    terminate_session_members(
        KillOutcome::Sent,
        || {
            scans.set(scans.get() + 1);
            Ok(if pauses.get() == 8 { vec![] } else { vec![42] })
        },
        |pid| {
            signals.push(pid);
            KillOutcome::Sent
        },
        || pauses.set(pauses.get() + 1),
    )
    .expect("the last signal terminated the only member");
    assert_eq!(signals, vec![42; 8]);
    assert_eq!(pauses.get(), 8);
    assert_eq!(scans.get(), 9);
}

/// A persistent descendant still fails after the existing budget and reports the final observed identity.
#[test]
fn session_termination_preserves_the_signal_budget_and_remaining_identity() {
    let mut pauses = 0;
    let mut signals = Vec::new();
    let error = terminate_session_members(
        KillOutcome::Sent,
        || Ok(vec![42]),
        |pid| {
            signals.push(pid);
            KillOutcome::Sent
        },
        || pauses += 1,
    )
    .expect_err("a remaining member is not success");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(error.to_string().contains("42"));
    assert_eq!(signals, vec![42; 8]);
    assert_eq!(pauses, 8);
}

/// Failure to read the final membership cannot authorize successful cleanup after the signal budget is exhausted.
#[test]
fn session_termination_propagates_final_confirmation_errors() {
    let scans = std::cell::Cell::new(0);
    let mut signals = Vec::new();
    let mut pauses = 0;
    let error = terminate_session_members(
        KillOutcome::Sent,
        || {
            scans.set(scans.get() + 1);
            if scans.get() == 9 {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                Ok(vec![42])
            }
        },
        |pid| {
            signals.push(pid);
            KillOutcome::Sent
        },
        || pauses += 1,
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(scans.get(), 9);
    assert_eq!(signals, vec![42; 8]);
    assert_eq!(pauses, 8);
}

/// Empty sessions finish without signalling; unreadable membership never becomes a successful empty session.
#[test]
fn session_termination_preserves_empty_and_error_results() {
    terminate_session_members(
        KillOutcome::Sent,
        || Ok(vec![]),
        |_| panic!("no member"),
        || panic!("no wait"),
    )
    .unwrap();
    let error = terminate_session_members(
        KillOutcome::Sent,
        || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        |_| panic!("unknown member"),
        || panic!("no wait"),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}

/// Kills and reaps a test child when dropped, so a failing assertion leaves no live child or zombie.
#[cfg(target_os = "macos")]
struct ReapOnDrop(std::process::Child);

#[cfg(target_os = "macos")]
impl ReapOnDrop {
    /// Spawns `command` under the guard.
    fn spawn(command: &mut std::process::Command) -> Self {
        Self(command.spawn().expect("spawn test child"))
    }

    /// The guarded child's pid.
    fn id(&self) -> u32 {
        self.0.id()
    }
}

#[cfg(target_os = "macos")]
// Lifecycle: dropping `ReapOnDrop` kills its child and reaps it within 5 s, reporting one it cannot reap.
impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        use std::io::Write as _;
        // The reap below decides; the kill result only explains a child that is never reaped.
        let killed = self.0.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                // Cleanup failures go to stderr and never panic: a panic while a test unwinds aborts the run.
                Ok(None) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "test child {} was not reaped within 5 s of SIGKILL (kill: {killed:?})",
                        self.0.id()
                    );
                    return;
                }
                Err(error) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "could not reap test child {}: {error} (kill: {killed:?})",
                        self.0.id()
                    );
                    return;
                }
            }
        }
    }
}

/// Observes a child's exit without reaping it, polling until `deadline`; returns whether it exited.
#[cfg(target_os = "macos")]
fn exited_without_reaping(pid: u32, deadline: std::time::Instant) -> std::io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t =
            // SAFETY: zeroed `siginfo_t` is valid writable storage for `waitid` to initialize.
            unsafe { std::mem::zeroed() };
        let waited =
            // SAFETY: `info` is writable and `pid` is our child; `WNOWAIT` leaves an exited child unreaped.
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
        if waited == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                // EINTR retries within the same deadline; any other error ends the observation.
                return Err(error);
            }
        } else {
            let exited_pid =
                // SAFETY: `info` was zeroed first, so `si_pid` is readable whether or not `waitid` wrote a status.
                unsafe { info.si_pid() };
            if exited_pid != 0 {
                return Ok(true);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Spawns a child that has exited but is deliberately left unreaped, so it stays a zombie until its guard drops.
#[cfg(target_os = "macos")]
fn spawn_unreaped_exit_child() -> ReapOnDrop {
    let child = ReapOnDrop::spawn(&mut std::process::Command::new("/usr/bin/true"));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let exited = exited_without_reaping(child.id(), deadline).expect("observe the child's exit");
    assert!(exited, "the exiting child did not exit within 10 s");
    child
}

/// Spawns a child that stays alive until its guard drops.
#[cfg(target_os = "macos")]
fn spawn_live_child() -> ReapOnDrop {
    ReapOnDrop::spawn(std::process::Command::new("/bin/sleep").arg("30"))
}

/// A real exited-but-unreaped macOS child leaves the session scan while its live sibling stays listed.
#[cfg(target_os = "macos")]
#[test]
fn macos_session_scan_excludes_an_unreaped_exited_child_and_keeps_a_live_one() {
    // Both children inherit this test process's session, so one scan sees a live and a zombie member.
    let live = spawn_live_child();
    let exited = spawn_unreaped_exit_child();
    let session_id =
        // SAFETY: `getsid(0)` only reads the calling process's own session id.
        unsafe { libc::getsid(0) };
    assert!(session_id > 0, "read this process's session id");
    let members = unix_session_pids(session_id as u32).expect("scan this session");
    assert!(members.contains(&live.id()), "live member missing from {members:?}");
    assert!(!members.contains(&exited.id()), "unreaped exited child listed in {members:?}");
}

/// macOS liveness treats a real exited-but-unreaped child as terminated and a running child as active.
#[cfg(target_os = "macos")]
#[test]
fn macos_liveness_treats_an_unreaped_exited_child_as_terminated() {
    let live = spawn_live_child();
    let exited = spawn_unreaped_exit_child();
    assert!(
        unix_process_is_active(live.id()).expect("probe live child"),
        "a running child must stay a live session member"
    );
    assert!(
        !unix_process_is_active(exited.id()).expect("probe exited child"),
        "an exited, unreaped child is a zombie, not a live session member"
    );
}

/// A member that accepts SIGKILL but outlives the budget is reported with its ids, state, command, and `kill=ok`.
#[cfg(target_os = "macos")]
#[test]
fn macos_session_survivor_error_names_state_and_command() {
    let member = spawn_live_child();
    let pid = member.id();
    let group =
        // SAFETY: `getpgrp` only reads the calling process's own process group id.
        unsafe { libc::getpgrp() };
    // The scan keeps reporting the live child and the injected signal succeeds without effect, so the budget runs out.
    let error = terminate_session_members(
        KillOutcome::Sent,
        || Ok(vec![pid]),
        |_| KillOutcome::Sent,
        || {},
    )
    .expect_err("a member that never leaves exhausts the budget");
    let message = error.to_string();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    // The child is ours and inherits this process's group.
    let ids = format!("pid={pid} ppid={} pgid={group} state=", std::process::id());
    assert!(message.contains(&ids), "{message}");
    assert!(message.contains("in_exit=false comm=\"sleep\" kill=ok"), "{message}");
    assert!(!message.contains("state=gone"), "{message}");
    assert!(!message.contains("state=unreadable"), "{message}");
}

/// A refused signal stays visible: a survivor reports its latest kill outcome, not an earlier success.
#[cfg(target_os = "macos")]
#[test]
fn macos_session_survivor_error_reports_a_refused_signal() {
    let member = spawn_live_child();
    let pid = member.id();
    let mut attempts = 0;
    // The first seven injected signals succeed and the eighth is refused, so only `EPERM` may appear.
    let error = terminate_session_members(
        KillOutcome::Sent,
        || Ok(vec![pid]),
        |_| {
            attempts += 1;
            if attempts < 8 {
                KillOutcome::Sent
            } else {
                KillOutcome::Refused(Some(libc::EPERM))
            }
        },
        || {},
    )
    .expect_err("a member that never leaves exhausts the budget");
    let message = error.to_string();
    assert_eq!(attempts, 8);
    assert!(message.contains(&format!("pid={pid} ppid=")), "{message}");
    assert!(message.contains("comm=\"sleep\" kill=EPERM"), "{message}");
    // A leading space keeps `group_kill=ok` from matching the member's own outcome.
    assert!(!message.contains(" kill=ok"), "{message}");
}

/// A member whose latest pass skipped the recheck reads `kill=skipped-recheck`, even after earlier successes.
#[cfg(target_os = "macos")]
#[test]
fn macos_session_survivor_error_marks_a_member_the_recheck_skipped() {
    let member = spawn_live_child();
    let pid = member.id();
    let mut attempts = 0;
    // Seven injected signals succeed; on the eighth pass the recheck no longer matches, so no signal is sent.
    let error = terminate_session_members(
        KillOutcome::Sent,
        || Ok(vec![pid]),
        |_| {
            attempts += 1;
            if attempts < 8 {
                KillOutcome::Sent
            } else {
                KillOutcome::SkippedRecheck
            }
        },
        || {},
    )
    .expect_err("a member that never leaves exhausts the budget");
    let message = error.to_string();
    assert_eq!(attempts, 8);
    assert!(message.contains("comm=\"sleep\" kill=skipped-recheck"), "{message}");
    // The latest pass decides: the earlier successes must not show, and the member was listed, not late.
    assert!(!message.contains(" kill=ok"), "{message}");
    assert!(!message.contains("kill=unlisted"), "{message}");
}

/// A member first listed by the final scan was never signalled, so it reads `kill=unlisted`.
#[cfg(target_os = "macos")]
#[test]
fn macos_session_survivor_error_marks_a_member_seen_only_in_the_final_scan() {
    let early = spawn_live_child();
    let late = spawn_live_child();
    let (early_pid, late_pid) = (early.id(), late.id());
    let scans = std::cell::Cell::new(0);
    let mut signalled = Vec::new();
    // Eight scans list one member; the ninth, after the last pause, also lists the late one.
    let error = terminate_session_members(
        KillOutcome::Sent,
        || {
            scans.set(scans.get() + 1);
            Ok(if scans.get() <= 8 { vec![early_pid] } else { vec![early_pid, late_pid] })
        },
        |pid| {
            signalled.push(pid);
            KillOutcome::Sent
        },
        || {},
    )
    .expect_err("both members outlive the budget");
    let message = error.to_string();
    // Survivors sit between the brackets, and each starts with its own pid, so a `ppid=` field cannot match.
    let list = message
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(list, _)| list.to_owned())
        .unwrap_or_else(|| panic!("no survivor list in {message}"));
    let survivor = |pid: u32| {
        list.split(", ")
            .find(|part| part.starts_with(&format!("pid={pid} ")))
            .unwrap_or_else(|| panic!("pid {pid} missing from {message}"))
            .to_owned()
    };
    assert_eq!(scans.get(), 9);
    assert_eq!(signalled, vec![early_pid; 8]);
    assert!(survivor(early_pid).contains("kill=ok"), "{message}");
    assert!(survivor(late_pid).contains("kill=unlisted"), "{message}");
}

/// The group SIGKILL's own result is reported once, after the survivor list.
#[cfg(target_os = "macos")]
#[test]
fn macos_session_survivor_error_reports_the_group_kill_result() {
    let member = spawn_live_child();
    let pid = member.id();
    // The injected group kill was refused while the member's own signals were accepted.
    let error = terminate_session_members(
        KillOutcome::Refused(Some(libc::EPERM)),
        || Ok(vec![pid]),
        |_| KillOutcome::Sent,
        || {},
    )
    .expect_err("a member that never leaves exhausts the budget");
    let message = error.to_string();
    assert!(message.ends_with("]; group_kill=EPERM"), "{message}");
    assert_eq!(message.matches("group_kill=").count(), 1, "{message}");
    assert!(message.contains("comm=\"sleep\" kill=ok"), "{message}");
}

/// Survivor text names ids, every kill outcome, and each state, and never shows missing history as errno 0.
#[cfg(target_os = "macos")]
#[test]
fn macos_survivor_text_names_each_kill_outcome() {
    let running = MacosProcessState::Read {
        ppid: 1,
        pgid: 5,
        status: libc::SRUN,
        in_exit: false,
        command: "sh".to_owned(),
    };
    let text =
        |state: &MacosProcessState, signal: KillOutcome| format_session_survivor(7, state, signal);
    let prefix = "pid=7 ppid=1 pgid=5 state=SRUN in_exit=false comm=\"sh\"";
    assert_eq!(text(&running, KillOutcome::Sent), format!("{prefix} kill=ok"));
    assert_eq!(
        text(&running, KillOutcome::Refused(Some(libc::EPERM))),
        format!("{prefix} kill=EPERM")
    );
    assert_eq!(text(&running, KillOutcome::Refused(None)), format!("{prefix} kill=refused"));
    assert_eq!(
        text(&running, KillOutcome::SkippedRecheck),
        format!("{prefix} kill=skipped-recheck")
    );
    assert_eq!(text(&running, KillOutcome::Unlisted), format!("{prefix} kill=unlisted"));
    assert_eq!(text(&MacosProcessState::Gone, KillOutcome::Sent), "pid=7 state=gone kill=ok");
    assert_eq!(
        text(&MacosProcessState::Unreadable { errno: 0 }, KillOutcome::Sent),
        "pid=7 state=unreadable(short record) kill=ok"
    );
    assert_eq!(
        text(&MacosProcessState::Unreadable { errno: libc::EPERM }, KillOutcome::Unlisted),
        "pid=7 state=unreadable(EPERM) kill=unlisted"
    );
    // The group kill is reported once, in the same form as a member's outcome.
    assert_eq!(describe_group_kill(KillOutcome::Sent), "; group_kill=ok");
    assert_eq!(describe_group_kill(KillOutcome::Refused(Some(libc::EPERM))), "; group_kill=EPERM");
    assert_eq!(describe_group_kill(KillOutcome::Refused(None)), "; group_kill=refused");
}

/// The macOS classifier drops only zombies and reaped pids; it keeps unreadable and in-exit members.
#[cfg(target_os = "macos")]
#[test]
fn macos_state_classifier_keeps_unreadable_and_exiting_members() {
    let read = |status: u32, in_exit: bool| MacosProcessState::Read {
        ppid: 1,
        pgid: 1,
        status,
        in_exit,
        command: "sh".to_owned(),
    };
    assert!(!macos_state_is_active(&MacosProcessState::Gone));
    assert!(!macos_state_is_active(&read(libc::SZOMB, true)));
    assert!(macos_state_is_active(&read(libc::SRUN, false)));
    // The classifier keeps a member flagged in exit, which can still hold the PTY slave; `getsid` may drop it first.
    assert!(macos_state_is_active(&read(libc::SRUN, true)));
    // Unreadable state is conservative: cleanup must not report success it could not observe.
    assert!(macos_state_is_active(&MacosProcessState::Unreadable { errno: libc::EPERM }));
    assert!(macos_state_is_active(&MacosProcessState::Unreadable { errno: 0 }));
}

/// Reading state sees an unreaped exited child as a zombie, and needs no same-user access for a root-owned pid.
#[cfg(target_os = "macos")]
#[test]
fn macos_process_state_reads_zombies_and_root_owned_members() {
    let exited = spawn_unreaped_exit_child();
    let state = macos_process_state(exited.id());
    // The zombie's parent stays this process until it is reaped; its group is not asserted here.
    assert!(
        matches!(
            &state,
            MacosProcessState::Read { ppid, status: libc::SZOMB, in_exit: true, command, .. }
                if *ppid == std::process::id() && command.as_str() == "true"
        ),
        "{state:?}"
    );
    // `launchd` is owned by root; the full BSD-info flavor refuses it with `EPERM`, this one does not.
    assert!(matches!(
        macos_process_state(1),
        MacosProcessState::Read { status, .. } if status != libc::SZOMB
    ));
}

#[test]
fn linux_proc_stat_treats_zombie_and_dead_members_as_terminated() {
    // Protect session cleanup from waiting on non-runnable descendants that container PID 1 has not reaped.
    assert_eq!(linux_proc_stat_is_active("41 (sleep worker) R 1 2 3"), Some(true));
    assert_eq!(linux_proc_stat_is_active("42 (sleep worker) Z 1 2 3"), Some(false));
    assert_eq!(linux_proc_stat_is_active("43 (name with ) inside) X 1 2 3"), Some(false));
    assert_eq!(linux_proc_stat_is_active("44 (name with ) inside) x 1 2 3"), Some(false));
    assert_eq!(linux_proc_stat_is_active("malformed"), None);
}
