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
