//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::workload_tests::*;
use super::*;

#[test]
pub(super) fn run_steps_writes_nothing_between_ready_and_go() {
    // The probe writes GO only after it finds READY, so nothing may follow READY until GO exists.
    let flood = plan("S3", "default", true).unwrap();
    let waiting = program_dir("ready-waiting", &flood, false);
    let mut host = FakeHost::new(20);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_steps(&waiting, &mut host, &mut out, &mut err),
        1,
        "the fake host ends the wait"
    );
    assert_eq!(out, b"READY 0\n");
    assert_eq!(host.sleeps, vec![Duration::from_millis(50); 20], "go/0 is polled every 50 ms");
    // The session record names the role and this process, as the comparison script reads it.
    let record = std::fs::read_to_string(waiting.join("sessions/0.json")).unwrap();
    let record: serde_json::Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["role"], 0);
    assert_eq!(record["program_pid"], std::process::id());
    std::fs::remove_dir_all(&waiting).unwrap();
    // Once GO exists, the workload follows READY directly.
    let started = program_dir("ready-go", &flood, true);
    let mut host = FakeHost::new(usize::MAX);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&started, &mut host, &mut out, &mut err), 0);
    assert!(out.starts_with(b"READY 0\ny\ny\n"));
    assert!(out == expected_role_output(&flood, 0), "S3 output differs");
    std::fs::remove_dir_all(&started).unwrap();
}

#[test]
pub(super) fn run_steps_plays_each_fixture_in_order() {
    // Every role prints what its POSIX script prints, each fixture once, in order, then its sentinel.
    let plans = all_plans_on(BUILD_HOST).into_iter().filter(|plan| {
        plan.short && !fixtures(plan).is_empty() && !plan.roles.contains(&Workload::DateLoop)
    });
    for plan in plans {
        let key = plan_key(&plan);
        let dir = program_dir(&format!("play-{}", key.replace(' ', "-")), &plan, true);
        for role in 0..plan.roles.len() {
            let expected = expected_role_output(&plan, role);
            let mut host = FakeHost::new(usize::MAX);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_steps(&dir, &mut host, &mut out, &mut err);
            assert_eq!(code, 0, "{key} role {role}: {}", String::from_utf8_lossy(&err));
            // A writer that repeated a fixture would print more than one copy of each.
            assert_eq!(out.len(), expected.len(), "{key} role {role} length");
            assert!(out == expected, "{key} role {role} bytes differ");
            let finished = plan.roles[role] != Workload::IdleShell;
            assert_eq!(dir.join(format!("done/{role}")).exists(), finished, "{key} role {role}");
            if let Workload::PrintThenSleep(_) = plan.roles[role] {
                let bound = Duration::from_secs(plan.timeout_s + ANCHOR_MARGIN_S);
                assert_eq!(host.sleeps.last(), Some(&bound), "{key} role {role} sleeps the bound");
                assert_eq!(host.shells, 0);
            } else {
                assert_eq!(host.shells, 1, "{key} role {role} ends in the idle shell");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
pub(super) fn run_steps_writes_the_frames_of_both_s10_variants() {
    // S10 plays its frame files in order, each once and paced 16 ms; only `sync` brackets them.
    for (variant, synchronized) in [("default", false), ("sync", true)] {
        let stream = plan("S10", variant, true).unwrap();
        let dir = program_dir(&format!("frames-{variant}"), &stream, true);
        let mut host = FakeHost::new(usize::MAX);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 0);
        let mut rest = out.strip_prefix(b"READY 0\n".as_slice()).expect("READY comes first");
        let frames = fixtures(&stream);
        assert_eq!(frames.len(), 300);
        for (index, frame) in frames.iter().enumerate() {
            let FixtureBody::Bytes(bytes) = &frame.body else { panic!("frames are bytes") };
            assert_eq!(frame.relative_path, format!("frames/{index}"));
            assert!(rest.len() >= bytes.len(), "{variant} output ends before frame {index}");
            let (head, tail) = rest.split_at(bytes.len());
            assert!(head == bytes.as_slice(), "{variant} frame {index} differs");
            let bracketed = head.starts_with(b"\x1b[?2026h") && head.ends_with(b"\x1b[?2026l");
            assert_eq!(bracketed, synchronized, "{variant} frame {index} brackets");
            rest = tail;
        }
        assert_eq!(rest, format!("{}\n", sentinel_line(0, NONCE)).as_bytes());
        assert_eq!(host.sleeps, vec![Duration::from_millis(16); 300]);
        assert_eq!(host.shells, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
pub(super) fn run_steps_date_loop_ends_each_line() {
    // S4 streams one `date` line every 10 ms, each ending in LF; the fake host ends the loop.
    let stream = plan("S4", "default", true).unwrap();
    let dir = program_dir("date-loop", &stream, true);
    let mut host = FakeHost::new(3);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 1, "the loop never ends by itself");
    let line = date_line(FAKE_START_S);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text, format!("READY 0\n{line}\n{line}\n{line}\n{line}\n"));
    assert_eq!(host.sleeps, vec![Duration::from_millis(10); 3]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
pub(super) fn run_steps_unplanned_role_only_sleeps_the_bound() {
    // An extra pane follows the handshake like the script's `*)` branch, then prints nothing more.
    let idle = plan("S1", "default", true).unwrap();
    let dir = program_dir("unplanned", &idle, true);
    std::fs::create_dir(dir.join("roles/0")).unwrap();
    std::fs::write(dir.join("acks/1"), "").unwrap();
    std::fs::write(dir.join("go/1"), "").unwrap();
    let mut host = FakeHost::new(usize::MAX);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 0);
    assert_eq!(out, b"READY 1\n");
    assert_eq!(host.sleeps, vec![Duration::from_secs(idle.timeout_s + ANCHOR_MARGIN_S)]);
    assert_eq!(host.shells, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
pub(super) fn program_returns_1_on_a_write_error() {
    // A program that cannot write stops at once with one line naming the error, ending its role.
    let idle = plan("S1", "default", true).unwrap();
    let dir = program_dir("write-error", &idle, true);
    let mut host = FakeHost::new(usize::MAX);
    let mut err = Vec::new();
    assert_eq!(run_steps(&dir, &mut host, &mut FailingWriter, &mut err), 1);
    let text = String::from_utf8(err).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.ends_with('\n') && text.contains("console closed"), "{text}");
    assert_eq!(host.shells, 0, "nothing runs after the failed write");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
pub(super) fn role_exit_program_exits_1_after_go() {
    // S1/role-exit's program exits 1 as soon as GO exists, printing nothing after READY, not even an error.
    let exiting = plan("S1", "role-exit", true).unwrap();
    let dir = program_dir("role-exit", &exiting, true);
    let mut host = FakeHost::new(usize::MAX);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_steps(&dir, &mut host, &mut out, &mut err), 1);
    assert_eq!(out, b"READY 0\n");
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    assert_eq!((host.shells, host.sleeps.len()), (0, 0));
    assert!(!dir.join("done/0").exists());
    assert_eq!(program_steps(Workload::ExitAfterGo), [ProgramStep::ExitAfterGo]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The Windows program plays S6/rest as the POSIX script does: it creates the rest target, prints
/// the URL and the target's absolute path above the sentinel, then sets the scroll region and
/// streams date lines inside it until the host stops it.
#[test]
pub(super) fn run_steps_prints_the_rest_fixture_then_streams_inside_its_region() {
    let rest = plan("S6", "rest", true).unwrap();
    let dir = program_dir("rest-stream", &rest, true);
    let mut host = FakeHost::new(2);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_ne!(run_steps(&dir, &mut host, &mut out, &mut err), 0, "the fake host ends the stream");
    let target = dir.join("workload").join(REST_TARGET_NAME);
    assert!(target.exists(), "the rest target file is created before its path is printed");
    let fixture = format!(
        "READY 0\n\x1b[H\x1b[2J{REST_URL}\n{}\n\n{}\n\x1b[{}r\x1b[999;1H",
        target.display(),
        sentinel_line(0, NONCE),
        crate::scenarios::REST_REGION_TOP
    );
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with(&fixture), "{text:?}");
    let streamed = &text[fixture.len()..];
    assert_eq!(
        streamed.lines().count(),
        3,
        "one date line before each granted sleep and the failed one"
    );
    assert!(streamed.lines().all(|line| line.ends_with("UTC 1970") || line.contains("UTC")));
    std::fs::remove_dir_all(&dir).unwrap();
}
