use super::*;

fn identity(pid: u32) -> Identity {
    Identity { pid, seconds: 100, micros: 1 }
}

fn live(pid: u32) -> Observation {
    let id = identity(pid);
    Observation {
        state: 1,
        identity: id,
        recheck: id,
        ppid: 42,
        pgid: 42,
        sid: 42,
        sid_errno: 0,
        status: libc::SRUN,
        xstatus: 0,
        in_exit: false,
        errno: 0,
        command: *b"sleep\0\0\0\0\0\0\0\0\0\0\0",
    }
}

fn failure(members: &str) -> String {
    format!("PTY session still has live descendants after termination attempts: [{members}]; group_kill=ok")
}

#[test]
fn named_survivors_come_only_from_the_complete_error() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Only the exact bounded final-list grammar can nominate post-return identities.
    let text = failure("pid=43 ppid=42 pgid=42 state=SRUN in_exit=false comm=\"sleep\" kill=ok");
    assert_eq!(failed_member_pids(&text), Ok(vec![43]));
    let multiple = failure("pid=43 state=gone kill=ok, pid=44 state=SRUN kill=unlisted");
    assert_eq!(failed_member_pids(&multiple), Ok(vec![43, 44]));
    // Per-pass fields stay inside each survivor without becoming additional PID or group tokens.
    let history = failure("pid=43 state=gone kill=EPERM history=numeric-pid first_pass=1 last_pass=6 listed=2 sent=1 refused=1 skipped=0 passes=ok|-|-|-|-|EPERM|-|-, pid=44 state=SRUN kill=unlisted history=none");
    assert_eq!(failed_member_pids(&history), Ok(vec![43, 44]));
    let late = failure("pid=43 state=gone kill=ok history=numeric-pid first_pass=8 last_pass=8 listed=1 sent=1 refused=0 skipped=0 passes=-|-|-|-|-|-|-|ok");
    assert_eq!(failed_member_pids(&late), Ok(vec![43]));
    for invalid in [
        "pid=43".to_owned(),
        format!("prefix{text}"),
        text.replace("]; group_kill=ok", ""),
        format!("{text}\n"),
        failure(""),
        failure("pid=0 state=SRUN kill=ok"),
        failure("pid=2147483648 state=SRUN kill=ok"),
        failure("pid=43 state=SRUN kill=ok, pid=43 state=SRUN kill=ok"),
        failure("pid=43 state=SRUN"),
    ] {
        assert!(failed_member_pids(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn survivor_capacity_includes_the_retained_leader() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // The error parser reserves one identity for the unreaped leader instead of overflowing later capture.
    let members = (1..MAX_IDENTITIES)
        .map(|pid| format!("pid={pid} state=SRUN kill=ok"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(failed_member_pids(&failure(&members)).unwrap().len(), MAX_IDENTITIES - 1);
    assert!(
        failed_member_pids(&failure(&format!("{members}, pid=999 state=SRUN kill=ok"))).is_err()
    );
    assert!(failed_member_pids(&"x".repeat(MAX_BYTES + 1)).is_err());
}

#[test]
fn post_return_capture_keeps_leader_and_verified_member_births() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // A zombie leader still reserves its PID while the survivor must independently match the retained session.
    let text = failure("pid=43 ppid=42 pgid=42 state=SRUN kill=ok");
    let mut sampled = Vec::new();
    let captured = capture_after_failure(42, &text, |pid| {
        sampled.push(pid);
        if pid == 42 {
            Observation { status: libc::SZOMB, sid: -1, sid_errno: libc::ESRCH, ..live(pid) }
        } else {
            live(pid)
        }
    })
    .unwrap();
    assert_eq!(sampled, vec![42, 43]);
    assert_eq!(captured.named, sampled);
    assert_eq!(captured.expected, vec![identity(42), identity(43)]);
    assert_eq!(captured.initial.len(), 2);
    assert!(!captured.unknown);
}

#[test]
fn initially_missing_or_foreign_member_remains_unknown() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // A post-return missing PID has no pre-return birth proof, so it cannot become a settled identity.
    let text = failure("pid=43 state=SRUN kill=ok");
    for member in [
        Observation::absent(43, 0, libc::ESRCH),
        Observation::absent(43, 2, libc::EPERM),
        Observation { sid: 99, ..live(43) },
        Observation { recheck: Identity { seconds: 101, ..identity(43) }, ..live(43) },
        Observation { identity: identity(44), recheck: identity(44), ..live(43) },
        Observation { sid: -1, sid_errno: libc::ESRCH, in_exit: true, ..live(43) },
    ] {
        let captured =
            capture_after_failure(42, &text, |pid| if pid == 42 { live(pid) } else { member })
                .unwrap();
        assert!(captured.unknown, "{member:?}");
    }
    assert!(capture_after_failure(43, &text, live).is_err());
    assert!(capture_after_failure(0, &text, live).is_err());
}

#[test]
fn unknown_capture_does_not_claim_settlement_after_eof() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Closing output cannot restore the missing birth history of an initially unknown survivor.
    let text = failure("pid=43 state=SRUN kill=ok");
    let captured = capture_after_failure(42, &text, |pid| {
        if pid == 42 {
            live(pid)
        } else {
            Observation::absent(pid, 0, libc::ESRCH)
        }
    })
    .unwrap();
    let terminal = Observation { status: libc::SZOMB, ..live(42) };
    assert_eq!(classify(&captured.expected, &[terminal], true, captured.unknown), Verdict::Unknown);
}

#[test]
fn settlement_requires_terminal_births_and_disconnected_output() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Only terminal observations of admitted births together with EOF support eventual settlement.
    let id = identity(42);
    assert_eq!(classify(&[id], &[live(42)], true, false), Verdict::Persistent);
    let zombie = Observation { status: libc::SZOMB, sid: -1, sid_errno: libc::ESRCH, ..live(42) };
    assert_eq!(classify(&[id], &[zombie], false, false), Verdict::NoEof);
    assert_eq!(classify(&[id], &[zombie], true, false), Verdict::SettledLate);
    assert_eq!(
        classify(&[id], &[Observation::absent(42, 0, libc::ESRCH)], true, false),
        Verdict::SettledLate
    );
}

#[test]
fn admitted_birth_stays_live_through_in_exit_session_loss() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Once admitted, an unchanged birth with in-exit ESRCH remains live until a real terminal state.
    let exiting = Observation { sid: -1, sid_errno: libc::ESRCH, in_exit: true, ..live(42) };
    for disconnected in [false, true] {
        assert_eq!(classify(&[identity(42)], &[exiting], disconnected, false), Verdict::Persistent);
    }
    assert_eq!(
        classify(&[identity(42)], &[Observation { status: libc::SZOMB, ..exiting }], true, false),
        Verdict::SettledLate
    );
    assert_eq!(
        classify(&[identity(42)], &[Observation { in_exit: false, ..exiting }], true, false),
        Verdict::Unknown
    );
}

#[test]
fn pid_reuse_or_unreadable_state_never_proves_settlement() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // A replacement birth or ambiguous native read cannot settle an earlier observed process.
    let id = identity(42);
    for value in [
        Observation::absent(42, 2, libc::EPERM),
        Observation::absent(42, 3, 0),
        Observation::absent(42, 0, 0),
        Observation { identity: Identity { seconds: 101, ..id }, ..live(42) },
        Observation {
            state: 0,
            identity: Identity { seconds: 101, ..id },
            errno: libc::ESRCH,
            ..live(42)
        },
        Observation { sid: -1, sid_errno: libc::EPERM, ..live(42) },
    ] {
        assert_eq!(classify(&[id], &[value], true, false), Verdict::Unknown);
    }
}

#[test]
fn incomplete_duplicate_or_excessive_identity_sets_are_unknown() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Partial or conflicting populations never index past their observations or claim completeness.
    assert_eq!(classify(&[], &[], true, false), Verdict::Unknown);
    assert_eq!(classify(&[identity(42), identity(43)], &[live(42)], true, false), Verdict::Unknown);
    assert_eq!(
        classify(&[identity(42), identity(42)], &[live(42), live(42)], true, false),
        Verdict::Unknown
    );
    let expected: Vec<_> = (1..=MAX_IDENTITIES as u32 + 1).map(identity).collect();
    let current: Vec<_> =
        expected.iter().map(|id| Observation { status: libc::SZOMB, ..live(id.pid) }).collect();
    assert_eq!(classify(&expected, &current, true, false), Verdict::Unknown);
    assert_eq!(
        classify(&expected[..MAX_IDENTITIES], &current[..MAX_IDENTITIES], true, false),
        Verdict::SettledLate
    );
}

fn exit_event(pid: u32, token: u64, status: i64) -> libc::kevent64_s {
    libc::kevent64_s {
        ident: u64::from(pid),
        filter: libc::EVFILT_PROC,
        flags: libc::EV_EOF,
        fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
        data: status,
        udata: token,
        ..empty_exit_event()
    }
}

fn registered_exit_record(pid: u32) -> ExitRecord {
    ExitRecord { registered: true, ..ExitRecord::pending(identity(pid)) }
}

#[test]
fn exit_status_distinguishes_normal_sigkill_and_other_signals() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Native wait payloads distinguish natural exit from each signal without attributing who delivered it.
    for (status, outcome) in [
        (7 << 8, ExitOutcome::Exited(7)),
        (libc::SIGKILL, ExitOutcome::Signalled(libc::SIGKILL)),
        (libc::SIGTERM, ExitOutcome::Signalled(libc::SIGTERM)),
        (libc::SIGABRT | 0x80, ExitOutcome::Signalled(libc::SIGABRT)),
    ] {
        let mut records = [registered_exit_record(42)];
        assert_eq!(apply_exit_event(&mut records, &exit_event(42, 1, i64::from(status))), Ok(()));
        assert_eq!(records[0].outcome, outcome);
        assert_eq!(records[0].event, Some(ExitPayload::of(&exit_event(42, 1, i64::from(status)))));
        assert_eq!(records[0].identity, identity(42));
    }
    assert_eq!(registered_exit_record(42).outcome, ExitOutcome::Pending);
    let mut unfinished = [registered_exit_record(42)];
    finish_exit_records(&mut unfinished);
    assert_eq!(unfinished[0].outcome, ExitOutcome::Unknown(libc::ENODATA));
    assert!(unfinished[0].event.is_none());
}

#[test]
fn observations_retain_raw_exit_status_without_requiring_a_registered_edge() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // The native record must retain terminal status even when a process exits before event registration.
    let mut native: libc::proc_bsdinfo =
        // SAFETY: proc_bsdinfo contains only integer and byte-array fields, all valid when zeroed.
        unsafe { std::mem::zeroed() };
    native.pbi_pid = 42;
    native.pbi_start_tvsec = 100;
    native.pbi_start_tvusec = 1;
    native.pbi_status = libc::SZOMB;
    native.pbi_xstatus = 7 << 8;
    let observed = from_native(42, &native);
    assert_eq!(observed.identity, identity(42));
    assert!(format!("{observed:?}").contains("xstatus: 1792"));
}

#[test]
fn zombie_status_requires_an_admitted_birth_and_terminal_native_state() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Live zero, changed births and partial reads cannot masquerade as a zombie's natural exit.
    let expected = identity(42);
    for (raw, outcome) in [
        (0, ExitOutcome::Exited(0)),
        (7 << 8, ExitOutcome::Exited(7)),
        (libc::SIGKILL as u32, ExitOutcome::Signalled(libc::SIGKILL)),
        ((libc::SIGABRT | 0x80) as u32, ExitOutcome::Signalled(libc::SIGABRT)),
    ] {
        let zombie = Observation {
            status: libc::SZOMB,
            xstatus: raw,
            sid: -1,
            sid_errno: libc::ESRCH,
            ..live(42)
        };
        assert_eq!(zombie_status(expected, zombie), outcome);
        assert_eq!(zombie_status(expected, Observation { in_exit: true, ..zombie }), outcome);
        for status in [0, libc::SIDL, libc::SRUN, libc::SSLEEP, libc::SSTOP] {
            assert_eq!(
                zombie_status(expected, Observation { status, ..zombie }),
                ExitOutcome::Unknown(libc::ENODATA)
            );
        }
        for invalid in [
            Observation::absent(42, 0, libc::ESRCH),
            Observation::absent(42, 2, libc::EPERM),
            Observation { state: 2, ..zombie },
            Observation { state: 3, ..zombie },
            Observation { errno: libc::EIO, ..zombie },
            Observation { identity: identity(43), ..zombie },
            Observation { identity: Identity { seconds: 101, ..expected }, ..zombie },
            Observation { recheck: Identity { micros: 2, ..expected }, ..zombie },
        ] {
            assert_eq!(zombie_status(expected, invalid), ExitOutcome::Unknown(libc::ENODATA));
        }
    }
    for expected in [
        identity(0),
        identity(i32::MAX as u32 + 1),
        Identity { seconds: 0, ..expected },
        Identity { micros: 1_000_000, ..expected },
    ] {
        let invalid =
            Observation { identity: expected, recheck: expected, status: libc::SZOMB, ..live(42) };
        assert_eq!(zombie_status(expected, invalid), ExitOutcome::Unknown(libc::ENODATA));
    }
}

#[test]
fn malformed_zombie_status_preserves_raw_bits_without_terminal_inference() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // High bits and nonterminal wait encodings remain visible instead of being truncated into a valid status.
    for raw in [0x7f, 0x80, 32, 0x109, 0xffff, 65_536, u32::MAX] {
        let observed = Observation { status: libc::SZOMB, xstatus: raw, ..live(42) };
        assert_eq!(zombie_status(identity(42), observed), ExitOutcome::Unknown(libc::EPROTO));
        assert_eq!(observed.xstatus, raw);
    }
    assert_eq!(decode_wait_status(-1), ExitOutcome::Unknown(libc::EPROTO));
}

#[test]
fn malformed_or_duplicate_exit_events_fail_closed() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Malformed delivery, receipt errors and repeated events cannot create or replace terminal evidence.
    let valid = exit_event(42, 1, 7 << 8);
    let mut invalid = Vec::new();
    for flags in [0, libc::EV_ERROR, libc::EV_EOF | libc::EV_ERROR] {
        invalid.push(libc::kevent64_s { flags, ..valid });
    }
    for fflags in [0, libc::NOTE_EXIT, libc::NOTE_EXITSTATUS] {
        invalid.push(libc::kevent64_s { fflags, ..valid });
    }
    for data in [-1, 65_536, 0x7f, 0xffff, 0x80, 32, 0x109] {
        invalid.push(libc::kevent64_s { data, ..valid });
    }
    for udata in [0, 2, u64::MAX, (1_u64 << 32) | 1] {
        invalid.push(libc::kevent64_s { udata, ..valid });
    }
    invalid.push(libc::kevent64_s { filter: libc::EVFILT_READ, ..valid });
    invalid.push(libc::kevent64_s { ident: 43, ..valid });
    for event in invalid {
        let mut records = [registered_exit_record(42)];
        assert_eq!(apply_exit_event(&mut records, &event), Err(libc::EPROTO));
        assert_eq!(records[0].outcome, ExitOutcome::Unknown(libc::EPROTO));
        assert!(records[0].receipt.is_none());
        if event.udata != 1 || event.ident != 42 || event.filter != libc::EVFILT_PROC {
            assert!(records[0].event.is_none());
        }
    }
    let refused =
        ExitPayload { flags: 0x4045, fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS, data: 0 };
    let mut unbound = [ExitRecord {
        registration_errno: Some(libc::EAGAIN),
        receipt: Some(refused),
        outcome: ExitOutcome::Unknown(libc::EAGAIN),
        ..ExitRecord::pending(identity(42))
    }];
    assert!(apply_exit_event(&mut unbound, &valid).is_err());
    assert_eq!(unbound[0].receipt, Some(refused));
    assert_eq!(unbound[0].registration_errno, Some(libc::EAGAIN));
    assert!(unbound[0].event.is_none());
    let mut records = [registered_exit_record(42), registered_exit_record(43)];
    assert_eq!(apply_exit_event(&mut records, &valid), Ok(()));
    assert!(apply_exit_event(&mut records, &valid).is_err());
    assert_eq!(records[0].outcome, ExitOutcome::Unknown(libc::EPROTO));
    assert_eq!(records[0].event, Some(ExitPayload::of(&valid)));
    assert!(apply_exit_event(&mut records, &libc::kevent64_s { udata: 2, ..valid }).is_err());
    assert!(records.iter().all(|record| record.outcome == ExitOutcome::Unknown(libc::EPROTO)));
    assert!(records[1].event.is_none());
    let receipt = libc::kevent64_s {
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_RECEIPT | libc::EV_ERROR,
        data: 0,
        ..valid
    };
    assert_eq!(exit_receipt(&receipt, identity(42), 1), Ok(()));
    for bad in [
        valid,
        libc::kevent64_s { data: i64::from(libc::EPERM), ..receipt },
        libc::kevent64_s { flags: libc::EV_ERROR, ..receipt },
        libc::kevent64_s { flags: receipt.flags | libc::EV_EOF, ..receipt },
        libc::kevent64_s { fflags: libc::NOTE_EXIT, ..receipt },
        libc::kevent64_s { ident: 43, ..receipt },
        libc::kevent64_s { udata: 0, ..receipt },
        libc::kevent64_s { filter: libc::EVFILT_READ, ..receipt },
    ] {
        assert!(exit_receipt(&bad, identity(42), 1).is_err());
    }
}

#[test]
fn exit_registration_requires_the_same_live_birth() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Birth validation rejects reuse and uncertainty; only the post-registration read may observe a same-birth zombie.
    let expected = identity(42);
    for observed in [
        live(43),
        Observation { identity: Identity { seconds: 101, ..expected }, ..live(42) },
        Observation { recheck: Identity { seconds: 101, ..expected }, ..live(42) },
        Observation::absent(42, 2, libc::EPERM),
        Observation::absent(42, 0, libc::ESRCH),
        Observation { state: 3, ..live(42) },
        Observation { status: 0, ..live(42) },
    ] {
        assert!(exit_birth(expected, observed, true).is_err());
        assert!(exit_birth(expected, observed, false).is_err());
    }
    for observed in
        [Observation { in_exit: true, ..live(42) }, Observation { status: libc::SZOMB, ..live(42) }]
    {
        assert!(exit_birth(expected, observed, true).is_err());
        assert_eq!(exit_birth(expected, observed, false), Ok(()));
    }
    assert_eq!(exit_birth(expected, live(42), true), Ok(()));
    assert_eq!(exit_birth(expected, live(42), false), Ok(()));
    for expected in [Identity { seconds: 0, ..expected }, identity(0), identity(u32::MAX)] {
        assert!(exit_birth(expected, live(42), true).is_err());
    }
    assert_eq!(exit_deadline(Instant::now()), Err(libc::ETIMEDOUT));
}

#[test]
fn native_exit_watch_observes_status_without_reaping() {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Pipe-gated children and grandchildren exercise both permission paths without letting the observer reap them.
    for (ending, expected, grandchild) in [
        ("exit 7", ExitOutcome::Exited(7), false),
        ("kill -KILL $$", ExitOutcome::Signalled(libc::SIGKILL), false),
        ("exit 7", ExitOutcome::Exited(7), true),
        ("kill -KILL $$", ExitOutcome::Signalled(libc::SIGKILL), true),
    ] {
        let directory = tempfile::tempdir().expect("private control identity directory");
        let pid_path = directory.path().join("pid");
        let payload = format!("printf '%s\\n' \"$$\" > \"$1\"; IFS= read -r -t 3 gate || exit 23; [ \"$gate\" = go ] || exit 24; {ending}");
        let script = if grandchild {
            format!(
                "/bin/bash -c '{}' control \"$1\" <&0 & worker=$!; wait \"$worker\"; exit $?",
                payload.replace('\'', "'\\''")
            )
        } else {
            payload
        };
        let mut child = Command::new("/bin/bash")
            .args(["-c", &script, "control"])
            .arg(&pid_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bounded exit-status control");
        let observed = (|| -> io::Result<ExitOutcome> {
            let deadline = Instant::now() + Duration::from_secs(2);
            let pid = loop {
                if let Ok(text) = std::fs::read_to_string(&pid_path) {
                    if let Ok(pid) = text.trim().parse::<u32>() {
                        break pid;
                    }
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "control identity not published",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let initial = observe(pid);
            exit_birth(initial.identity, initial, true).map_err(io::Error::from_raw_os_error)?;
            if (grandchild && (pid == child.id() || initial.ppid != child.id()))
                || (!grandchild && pid != child.id())
            {
                return Err(io::Error::other("control process ancestry differs"));
            }
            let mut watch = ExitWatch::new(&[initial.identity], deadline)?;
            watch.poll();
            if !watch.records()[0].registered || watch.records()[0].outcome != ExitOutcome::Pending
            {
                return Err(io::Error::other(format!(
                    "native registration refused: {:?}",
                    watch.records()
                )));
            }
            child
                .stdin
                .as_mut()
                .ok_or_else(|| io::Error::other("control pipe missing"))?
                .write_all(b"go\n")?;
            drop(child.stdin.take());
            while Instant::now() < deadline {
                watch.poll();
                if let Some(errno) = watch.error() {
                    return Err(io::Error::from_raw_os_error(errno));
                }
                if watch.records()[0].outcome != ExitOutcome::Pending {
                    return Ok(watch.records()[0].outcome);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(io::Error::new(io::ErrorKind::TimedOut, "native exit event absent"))
        })();
        drop(child.stdin.take());
        // EOF releases either gate on failure; the parent reaps a grandchild before it exits.
        let status = child.wait().expect("the observer must leave its child waitable");
        assert_eq!(observed.expect("complete passive exit observation"), expected);
        match expected {
            ExitOutcome::Exited(code) => assert_eq!(status.code(), Some(code)),
            ExitOutcome::Signalled(signal) if grandchild => {
                assert_eq!(status.code(), Some(128 + signal))
            }
            ExitOutcome::Signalled(signal) => assert_eq!(status.signal(), Some(signal)),
            _ => unreachable!(),
        }
    }
}

#[test]
fn native_zombie_status_survives_exit_before_registration() {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // A pipe-gated birth exits before registration; libproc must expose its status without consuming ordinary waitability.
    for (ending, expected, raw) in [
        ("exit 0", ExitOutcome::Exited(0), 0),
        ("exit 7", ExitOutcome::Exited(7), 7 << 8),
        ("kill -KILL $$", ExitOutcome::Signalled(libc::SIGKILL), libc::SIGKILL as u32),
    ] {
        let script =
            format!("IFS= read -r -t 3 gate || exit 23; [ \"$gate\" = go ] || exit 24; {ending}");
        let mut child = Command::new("/bin/bash")
            .args(["-c", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bounded zombie-status control");
        let observed = (|| -> io::Result<(Observation, ExitOutcome)> {
            let deadline = Instant::now() + Duration::from_secs(2);
            let initial = observe(child.id());
            exit_birth(initial.identity, initial, true).map_err(io::Error::from_raw_os_error)?;
            if zombie_status(initial.identity, initial) != ExitOutcome::Unknown(libc::ENODATA) {
                return Err(io::Error::other("live process supplied terminal status"));
            }
            child
                .stdin
                .as_mut()
                .ok_or_else(|| io::Error::other("control pipe missing"))?
                .write_all(b"go\n")?;
            drop(child.stdin.take());
            while Instant::now() < deadline {
                let sample = observe(child.id());
                if sample.state != 1
                    || sample.identity != initial.identity
                    || sample.recheck != initial.identity
                {
                    return Err(io::Error::other("control zombie birth became unknown"));
                }
                if sample.status == libc::SZOMB {
                    let mut late = ExitWatch::new(&[initial.identity], deadline)?;
                    late.poll();
                    late.finish();
                    let record = &late.records()[0];
                    if record.registered
                        || record.event.is_some()
                        || record.outcome != ExitOutcome::Unknown(libc::EAGAIN)
                    {
                        return Err(io::Error::other("late registration supplied an exit event"));
                    }
                    return Ok((sample, zombie_status(initial.identity, sample)));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(io::Error::new(io::ErrorKind::TimedOut, "control zombie not observed"))
        })();
        drop(child.stdin.take());
        // Closing the pipe releases a failed gate before ordinary wait consumes the retained child.
        let status = child.wait().expect("zombie observation must leave the child waitable");
        let (sample, decoded) = observed.expect("complete passive zombie-status observation");
        assert_eq!(sample.xstatus, raw);
        assert_eq!(decoded, expected);
        match expected {
            ExitOutcome::Exited(code) => assert_eq!(status.code(), Some(code)),
            ExitOutcome::Signalled(signal) => assert_eq!(status.signal(), Some(signal)),
            _ => unreachable!(),
        }
        eprintln!("PTY_ZOMBIE_CONTROL expected={expected:?} observation={sample:?} decoded={decoded:?} late_event=false wait_status={status:?}");
    }
}

#[test]
fn holder_admission_preserves_capacity_and_payload_limits() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // Passive draining stays within the holder's original allocation and an independent byte cap.
    assert!(admits_chunk(255, 256, MAX_PAYLOAD - 1, 1));
    assert!(!admits_chunk(256, 256, 0, 1));
    assert!(!admits_chunk(1, 1, 0, 1));
    assert!(!admits_chunk(0, 256, MAX_PAYLOAD, 1));
    assert!(!admits_chunk(0, 256, usize::MAX, 1));
}
