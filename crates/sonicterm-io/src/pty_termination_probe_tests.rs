use super::*;

fn identity() -> Identity {
    Identity { pid: 42, seconds: 100, micros: 1 }
}

fn live() -> Observation {
    let id = identity();
    Observation {
        state: 1,
        identity: id,
        recheck: id,
        ppid: 1,
        pgid: 42,
        sid: 42,
        sid_errno: 0,
        status: libc::SRUN,
        in_exit: false,
        errno: 0,
        command: *b"sleep\0\0\0\0\0\0\0\0\0\0\0",
    }
}

fn record(kind: u8, elapsed: u128, observation: Observation) -> Record {
    Record {
        kind,
        elapsed,
        result: 0,
        included: false,
        observation,
        raw_sid: -1,
        raw_sid_errno: 0,
        active: -1,
        active_errno: 0,
        enumerated: 0,
        skipped: 0,
    }
}

fn trace() -> Trace {
    Trace {
        process: 7,
        session: 42,
        sequence: 9,
        wall_start: 1_000,
        group_before_wall: 1_010,
        group_before_elapsed: 10,
        group_after_wall: 1_020,
        group_after_elapsed: 20,
        records: vec![record(0, 0, live()), record(1, 30, live())],
        overflow: false,
        result: 1,
    }
}

fn encoded(trace: &Trace) -> Vec<u8> {
    let mut bytes = Vec::new();
    writeln!(
        &mut bytes,
        "PTYTERM\t2\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        trace.process,
        trace.session,
        trace.sequence,
        trace.wall_start,
        trace.group_before_wall,
        trace.group_before_elapsed,
        trace.group_after_wall,
        trace.group_after_elapsed
    )
    .unwrap();
    for record in &trace.records {
        encode_record(&mut bytes, record).unwrap();
    }
    writeln!(
        &mut bytes,
        "END\t{}\t{}\t{}",
        trace.records.len(),
        u8::from(trace.overflow),
        trace.result
    )
    .unwrap();
    bytes
}

#[test]
fn settlement_requires_terminal_identity_and_disconnection() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Live and in-exit identities remain persistent; zombie/gone needs EOF as independent proof.
    let id = identity();
    let current = live();
    assert_eq!(classify(&[id], &[current], true, false), Verdict::Persistent);
    assert_eq!(
        classify(&[id], &[Observation { in_exit: true, ..current }], true, false),
        Verdict::Persistent
    );
    let zombie = Observation { status: libc::SZOMB, sid: -1, sid_errno: libc::ESRCH, ..current };
    assert_eq!(classify(&[id], &[zombie], false, false), Verdict::NoEof);
    assert_eq!(classify(&[id], &[zombie], true, false), Verdict::SettledLate);
    assert_eq!(
        classify(&[id], &[Observation::absent(42, 0, libc::ESRCH)], true, false),
        Verdict::SettledLate
    );
    assert_eq!(
        classify(&[id], &[Observation { state: 0, errno: libc::ESRCH, ..current }], true, false),
        Verdict::SettledLate
    );
}

#[test]
fn exiting_identity_without_session_remains_live_until_terminal() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Darwin can lose getsid during exit while the same birth still holds a nonterminal process entry.
    let exiting = Observation { sid: -1, sid_errno: libc::ESRCH, in_exit: true, ..live() };
    let mut captured = trace();
    captured.records.push(record(3, 31, exiting));
    assert_eq!(identities(&captured), (vec![identity()], false));
    for disconnected in [false, true] {
        assert_eq!(classify(&[identity()], &[exiting], disconnected, false), Verdict::Persistent);
    }
    let terminal = Observation { status: libc::SZOMB, ..exiting };
    assert_eq!(classify(&[identity()], &[terminal], true, false), Verdict::SettledLate);
    for unsupported in [
        Observation { in_exit: false, ..exiting },
        Observation { sid_errno: libc::EPERM, ..exiting },
    ] {
        captured.records.pop();
        captured.records.push(record(3, 31, unsupported));
        assert!(identities(&captured).1);
        assert_eq!(classify(&[identity()], &[unsupported], true, false), Verdict::Unknown);
    }
}

#[test]
fn uncertainty_reuse_and_fake_gone_never_prove_settlement() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A missing session is not a gone process, and arbitrary state-zero data cannot forge ESRCH.
    let id = identity();
    for value in [
        Observation::absent(42, 2, libc::EPERM),
        Observation::absent(42, 3, 0),
        Observation::absent(42, 0, 0),
        Observation::absent(42, 0, libc::EPERM),
        Observation { identity: Identity { seconds: 101, ..id }, ..live() },
        Observation {
            state: 0,
            identity: Identity { seconds: 101, ..id },
            errno: libc::ESRCH,
            ..live()
        },
        Observation { sid: -1, sid_errno: libc::ESRCH, ..live() },
    ] {
        assert_eq!(classify(&[id], &[value], true, false), Verdict::Unknown);
    }
    assert_eq!(classify(&[id], &[live()], true, true), Verdict::Unknown);
    assert_eq!(classify(&[], &[], true, false), Verdict::Unknown);
    assert_eq!(
        classify(&[Identity { seconds: 0, ..id }], &[live()], true, false),
        Verdict::Unknown
    );
    assert_eq!(classify(&[id, id], &[live(), live()], true, false), Verdict::Unknown);
}

#[test]
fn incomplete_and_oversized_observation_sets_are_unknown() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A partial observation loop cannot index past its data or accept an oversized identity population.
    let second = Identity { pid: 43, ..identity() };
    for unknown in [false, true] {
        assert_eq!(classify(&[identity(), second], &[live()], true, unknown), Verdict::Unknown);
    }
    let expected: Vec<_> = (0..=MAX_IDENTITIES)
        .map(|index| Identity { pid: 42 + index as u32, ..identity() })
        .collect();
    let observations: Vec<_> = expected
        .iter()
        .map(|id| Observation { identity: *id, recheck: *id, status: libc::SZOMB, ..live() })
        .collect();
    assert_eq!(
        classify(&expected[..MAX_IDENTITIES], &observations[..MAX_IDENTITIES], true, false),
        Verdict::SettledLate
    );
    assert_eq!(classify(&expected, &observations, true, false), Verdict::Unknown);
}

#[test]
fn holder_admission_preserves_capacity_and_byte_bounds() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Both independent limits apply before retaining a chunk, including arithmetic overflow.
    assert!(admits_chunk(255, 256, MAX_PAYLOAD - 1, 1));
    assert!(!admits_chunk(256, 256, 0, 1));
    assert!(!admits_chunk(1, 1, 0, 1));
    assert!(!admits_chunk(0, 256, MAX_PAYLOAD, 1));
    assert!(!admits_chunk(0, 256, usize::MAX, 1));
}

#[test]
fn parser_retains_calibration_and_command_bytes() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Clock domains remain separate fields and arbitrary fixed command bytes roundtrip without text escaping.
    let mut original = trace();
    original.records[0].observation.command =
        [0xff, 0, b'\t', b'\n', 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    let parsed = parse(&encoded(&original)).unwrap();
    assert_eq!((parsed.process, parsed.session, parsed.sequence, parsed.result), (7, 42, 9, 1));
    assert_eq!(
        (
            parsed.wall_start,
            parsed.group_before_wall,
            parsed.group_before_elapsed,
            parsed.group_after_wall,
            parsed.group_after_elapsed
        ),
        (1_000, 1_010, 10, 1_020, 20)
    );
    assert_eq!(parsed.records[0].observation.command, original.records[0].observation.command);
    assert_eq!(identities(&parsed), (vec![identity()], false));
}

#[test]
fn parser_rejects_incomplete_duplicate_and_unordered_framing() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Each invalid case begins with a complete valid trace so malformed positives cannot hide a missing guard.
    let bytes = encoded(&trace());
    assert!(parse(&bytes).is_ok());
    assert!(parse(&bytes[..bytes.len() - 1]).is_err());
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(b"extra\n");
    assert!(parse(&trailing).is_err());
    let text = String::from_utf8(bytes).unwrap();
    assert!(parse(text.replace("END\t2", "END\t3").as_bytes()).is_err());
    assert!(parse(text.replace("PTYTERM\t2", "PTYTERM\t1").as_bytes()).is_err());
    let mut backwards = trace();
    backwards.records.push(record(3, 29, live()));
    assert!(parse(&encoded(&backwards)).is_err());
    let mut duplicate = trace();
    duplicate.records.push(record(0, 31, live()));
    assert!(parse(&encoded(&duplicate)).is_err());
}

#[test]
fn group_brackets_must_enclose_the_native_signal_records() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Monotonic boundaries and wall-clock calibration are validated separately, never compared to birth in Instant units.
    for case in 0..5 {
        let mut value = trace();
        match case {
            0 => value.group_before_elapsed = 21,
            1 => value.group_after_wall = 1_009,
            2 => value.wall_start = 1_011,
            3 => value.records[0].elapsed = 11,
            _ => value.records[1].elapsed = 19,
        }
        assert!(parse(&encoded(&value)).is_err());
    }
    assert!(decode_command("gg000000000000000000000000000000").is_err());
    assert!(decode_command("00").is_err());
    assert!(decode_command("FF000000000000000000000000000000").is_err());
    assert_eq!(decode_command("ff000000000000000000000000000000").unwrap()[0], 255);
}

#[test]
fn limits_reject_complete_oversize_and_excess_record_payloads() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Maximal typed numeric fields exceed the byte cap without relying on malformed syntax.
    let mut value = trace();
    let maximal = Observation {
        identity: Identity { pid: i32::MAX as u32, seconds: u64::MAX, micros: 999_999 },
        recheck: Identity { pid: i32::MAX as u32, seconds: u64::MAX, micros: 999_999 },
        ppid: u32::MAX,
        pgid: u32::MAX,
        sid: i32::MAX,
        sid_errno: i32::MAX,
        status: u32::MAX,
        errno: i32::MAX,
        ..live()
    };
    for _ in 2..MAX_RECORDS {
        let mut r = record(3, u128::MAX, maximal);
        r.result = i32::MAX;
        r.raw_sid = i32::MAX;
        r.raw_sid_errno = i32::MAX;
        r.active_errno = i32::MAX;
        r.enumerated = u32::MAX;
        r.skipped = u32::MAX;
        value.records.push(r);
    }
    let payload = encoded(&value);
    assert!(
        payload.len() > MAX_BYTES,
        "fixture must exceed the byte cap with typed numeric records"
    );
    assert!(parse(&payload).is_err());
    value.records = vec![record(0, 0, live()), record(1, 30, live())];
    for _ in 2..=MAX_RECORDS {
        value.records.push(record(3, 31, live()));
    }
    let payload = encoded(&value);
    assert!(payload.len() < MAX_BYTES);
    assert!(parse(&payload).is_err());
}

#[test]
fn recording_and_output_storage_never_grow_past_their_caps() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Full record and byte buffers preserve their allocation and refuse more data without a native observation.
    let mut probe = Probe {
        directory: PathBuf::new(),
        directory_identity: None,
        session: 42,
        sequence: 0,
        origin: Instant::now(),
        wall_start: 1_000,
        group_before_wall: 1_010,
        group_before_elapsed: 10,
        group_after_wall: 1_020,
        group_after_elapsed: 20,
        records: Vec::with_capacity(MAX_RECORDS),
        seen: Vec::with_capacity(MAX_IDENTITIES),
        overflow: false,
    };
    let capacity = probe.records.capacity();
    for _ in 0..MAX_RECORDS {
        probe.push(record(3, 31, live()));
    }
    probe.push(record(3, 32, live()));
    assert!(probe.overflow);
    assert_eq!((probe.records.len(), probe.records.capacity()), (MAX_RECORDS, capacity));
    let mut bytes = LimitedOutput::new(4);
    let capacity = bytes.bytes.capacity();
    bytes.write_all(b"1234").unwrap();
    assert!(bytes.write_all(b"5").is_err());
    assert_eq!((bytes.bytes.as_slice(), bytes.bytes.capacity()), (&b"1234"[..], capacity));
}

#[test]
fn union_keeps_birth_before_gone_and_reports_reuse_or_overflow() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Terminal and reused records retain their known births rather than disappearing with session membership.
    let mut value = trace();
    let extra = Identity { pid: 43, ..identity() };
    value.records.push(record(
        3,
        31,
        Observation { state: 0, identity: extra, recheck: extra, errno: libc::ESRCH, ..live() },
    ));
    assert_eq!(identities(&value), (vec![identity(), extra], false));
    let replacement = Identity { seconds: 101, ..extra };
    value.records.push(record(
        3,
        32,
        Observation { state: 3, identity: extra, recheck: replacement, ..live() },
    ));
    let (union, unknown) = identities(&value);
    assert_eq!(union, vec![identity(), extra, replacement]);
    assert!(unknown);
    value = trace();
    value.records.push(record(3, 31, Observation::absent(43, 2, libc::EPERM)));
    assert!(identities(&value).1);
    value = trace();
    value.overflow = true;
    assert!(identities(&value).1);
    value = trace();
    for pid in 43..43 + MAX_IDENTITIES as u32 {
        let id = Identity { pid, ..identity() };
        value.records.push(record(3, 31, Observation { identity: id, recheck: id, ..live() }));
    }
    let (union, unknown) = identities(&value);
    assert_eq!(union.len(), MAX_IDENTITIES);
    assert!(unknown);
}

fn transport_fixture(root: &Path) -> (Ticket, PathBuf, Trace) {
    let directory = root.join("trace");
    fs::create_dir(&directory).unwrap();
    let mut value = trace();
    value.process = std::process::id();
    let path = directory
        .join(format!("ptyterm-{}-{}-{}.tsv", value.process, value.session, value.sequence));
    let ticket = Ticket {
        root_identity: directory_identity(&directory).unwrap(),
        directory,
        session: value.session,
        leader: live(),
        before: Vec::new(),
        captured_wall: 1_000,
    };
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap()
        .write_all(&encoded(&value))
        .unwrap();
    (ticket, path, value)
}

#[test]
fn ticket_load_binds_one_fresh_framed_failure_to_its_leader() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Every rejection starts with a real successful load, so malformed fixture setup cannot satisfy it.
    for case in [
        "duplicate",
        "filename-alias",
        "stale-header",
        "success-result",
        "wrong-birth",
        "known-file",
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let (mut ticket, path, mut value) = transport_fixture(scratch.path());
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.nlink(), 1);
        let loaded = ticket.load().unwrap_or_else(|error| panic!("{case}: positive load: {error}"));
        assert_eq!(
            (loaded.process, loaded.session, loaded.sequence, loaded.result),
            (std::process::id(), 42, 9, 1)
        );
        assert_eq!(loaded.records[0].observation.identity, ticket.leader.identity);
        assert_eq!(loaded.wall_start, ticket.captured_wall);
        match case {
            "duplicate" => {
                value.sequence += 1;
                let second = ticket.directory.join(format!(
                    "ptyterm-{}-{}-{}.tsv",
                    value.process, value.session, value.sequence
                ));
                fs::write(second, encoded(&value)).unwrap();
            }
            "filename-alias" => {
                let alias = ticket
                    .directory
                    .join(format!("ptyterm-{}-{}-0009.tsv", value.process, value.session));
                fs::rename(&path, alias).unwrap();
            }
            "stale-header" => {
                value.wall_start = ticket.captured_wall - 1;
                let bytes = encoded(&value);
                assert!(parse(&bytes).is_ok());
                fs::write(&path, bytes).unwrap();
            }
            "success-result" => {
                value.result = 0;
                let bytes = encoded(&value);
                assert!(parse(&bytes).is_ok());
                fs::write(&path, bytes).unwrap();
            }
            "wrong-birth" => {
                for record in &mut value.records {
                    record.observation.identity.seconds += 1;
                    record.observation.recheck.seconds += 1;
                }
                let bytes = encoded(&value);
                assert!(parse(&bytes).is_ok());
                fs::write(&path, bytes).unwrap();
            }
            "known-file" => ticket.before.push(path),
            _ => unreachable!(),
        }
        assert!(ticket.load().is_err(), "{case} must not authorize this call's evidence");
    }
}

#[test]
fn ticket_load_rejects_aliases_nonregular_files_and_replaced_roots() {
    let _serialised = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Metadata refusal precedes opening a FIFO; all files and aliases belong exclusively to this fresh fixture.
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{symlink, FileTypeExt, PermissionsExt},
    };
    for case in ["oversized", "symlink", "hardlink", "changed-root", "changed-mode", "fifo"] {
        let scratch = tempfile::tempdir().unwrap();
        let (ticket, path, value) = transport_fixture(scratch.path());
        assert_eq!(ticket.load().unwrap().result, 1, "{case}: positive load");
        match case {
            "oversized" => {
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len((MAX_BYTES + 1) as u64)
                    .unwrap();
                assert_eq!(fs::symlink_metadata(&path).unwrap().len(), (MAX_BYTES + 1) as u64);
            }
            "symlink" => {
                let target = scratch.path().join("target.tsv");
                fs::write(&target, encoded(&value)).unwrap();
                fs::remove_file(&path).unwrap();
                symlink(target, &path).unwrap();
                assert!(fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
            }
            "hardlink" => {
                fs::hard_link(&path, scratch.path().join("alias.tsv")).unwrap();
                assert_eq!(fs::symlink_metadata(&path).unwrap().nlink(), 2);
            }
            "changed-root" => {
                fs::rename(&ticket.directory, scratch.path().join("old-root")).unwrap();
                fs::create_dir(&ticket.directory).unwrap();
                fs::write(&path, encoded(&value)).unwrap();
            }
            "changed-mode" => {
                let mode = fs::metadata(&ticket.directory).unwrap().permissions().mode();
                fs::set_permissions(&ticket.directory, fs::Permissions::from_mode(mode ^ 0o010))
                    .unwrap();
            }
            "fifo" => {
                fs::remove_file(&path).unwrap();
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(
                    // SAFETY: name is NUL-terminated and names only this fixture's absent scratch path.
                    unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
                    0
                );
                assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());
            }
            _ => unreachable!(),
        }
        assert!(ticket.load().is_err(), "{case} must fail before reading unsafe transport");
    }
}
