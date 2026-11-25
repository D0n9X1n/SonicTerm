use super::*;
use std::sync::mpsc;

fn secs(count: u64) -> Duration {
    Duration::from_secs(count)
}

/// A case's end is fixed at its first section: a second section of the same case, opened late in
/// that window, does not extend it, so an end sentinel past the first opening's budget is refused.
#[test]
fn a_later_section_never_extends_its_case() {
    let start = Instant::now();
    let mut clock = CaseClock::new(start, start + secs(60));
    assert_eq!(clock.opened("case/out", start + secs(1)), Arrival::InTime);
    assert_eq!(clock.closed(start + secs(3)), Arrival::InTime);
    assert_eq!(clock.opened("case/in", start + secs(5)), Arrival::InTime);
    // The case opened at 1 s, so its end is 6 s whatever the second section did.
    assert_eq!(clock.deadline(), start + secs(1) + CASE_BUDGET);
    assert_eq!(clock.closed(start + secs(6)), Arrival::Late);
}

/// An end sentinel whose chunk arrived at or past the case's end is refused, while one just
/// inside it counts: the cutoff is the arrival time, not when the host got round to scanning.
#[test]
fn a_late_end_sentinel_is_refused() {
    let start = Instant::now();
    let open_case = || {
        let mut clock = CaseClock::new(start, start + secs(60));
        assert_eq!(clock.opened("late/out", start), Arrival::InTime);
        clock
    };
    assert_eq!(open_case().closed(start + CASE_BUDGET), Arrival::Late);
    let just_inside = start + CASE_BUDGET - Duration::from_millis(1);
    assert_eq!(open_case().closed(just_inside), Arrival::InTime);
}

/// Reopening a case whose end has passed is refused, even after silence that would admit a new
/// case: a case cannot restart its own clock by sending a fresh opening.
#[test]
fn a_reopened_case_keeps_its_first_end() {
    let start = Instant::now();
    let mut clock = CaseClock::new(start, start + secs(60));
    assert_eq!(clock.opened("again/out", start), Arrival::InTime);
    assert_eq!(clock.closed(start + secs(4)), Arrival::InTime);
    // Inside the silence bound after the close, but past the case's own 5 s end.
    assert_eq!(clock.opened("again/in", start + secs(6)), Arrival::Late);
    // A different case opened at the same moment has its own fresh end.
    assert_eq!(clock.opened("next/out", start + secs(6)), Arrival::InTime);
    assert_eq!(clock.deadline(), start + secs(6) + CASE_BUDGET);
}

/// While no section is open the silence bound runs from the last sentinel, so a host waits no
/// longer than that for the next case; an abandoned section also counts as that sentinel.
#[test]
fn silence_between_cases_is_bounded_from_the_last_sentinel() {
    let start = Instant::now();
    let mut clock = CaseClock::new(start, start + secs(60));
    assert_eq!(clock.deadline(), start + IDLE_BUDGET);
    assert_eq!(clock.opened("one/out", start + secs(1)), Arrival::InTime);
    clock.abandoned(start + secs(2));
    assert_eq!(clock.deadline(), start + secs(2) + IDLE_BUDGET);
    assert_eq!(clock.opened("two/out", start + secs(2) + IDLE_BUDGET), Arrival::Late);
}

/// The host's own end caps every case: a case opened near it ends with the host, not 5 s later.
#[test]
fn the_host_deadline_caps_every_case() {
    let start = Instant::now();
    let mut clock = CaseClock::new(start, start + secs(10));
    assert_eq!(clock.opened("first/out", start + secs(3)), Arrival::InTime);
    assert_eq!(clock.closed(start + secs(4)), Arrival::InTime);
    assert_eq!(clock.opened("edge/out", start + secs(8)), Arrival::InTime);
    assert_eq!(clock.deadline(), start + secs(10));
    assert_eq!(clock.closed(start + secs(10)), Arrival::Late);
}

/// A host is admitted with a loop that leaves REPORT_MARGIN before its deadline and never runs
/// past the work deadline, which itself keeps one teardown allowance before the total.
#[test]
fn a_launch_fits_its_loop_report_and_teardown_in_the_total() {
    let start = Instant::now();
    let run = RunClock::new(start);
    assert_eq!(run.work_deadline() + TEARDOWN_BUDGET, start + TOTAL_BUDGET);
    let first = run.launch(start).expect("a fresh run admits a host");
    assert_eq!(first.host_deadline, start + HOST_BUDGET);
    assert_eq!(first.loop_budget, HOST_BUDGET - REPORT_MARGIN);
    // Late staging leaves less than a whole host budget: the host gets only what is left.
    let late = start + TOTAL_BUDGET - TEARDOWN_BUDGET - secs(20);
    let squeezed = run.launch(late).expect("20 s still covers a loop and its report");
    assert_eq!(squeezed.host_deadline, run.work_deadline());
    assert_eq!(squeezed.loop_budget, secs(20) - REPORT_MARGIN);
}

/// No host launches once the work deadline passed, or when what is left cannot cover a loop
/// beyond the report margin; slow staging or a slow first host can never start a late one.
#[test]
fn an_expired_run_refuses_to_launch() {
    let start = Instant::now();
    let run = RunClock::new(start);
    assert_eq!(run.launch(run.work_deadline()), None);
    assert_eq!(run.launch(run.work_deadline() + secs(1)), None);
    assert_eq!(run.launch(run.work_deadline() - REPORT_MARGIN), None);
}

/// A scripted host and job: whether each call succeeds, and what teardown asked of them.
#[derive(Default)]
struct FakeTeardown {
    host_exits: bool,
    exits_after_terminate: bool,
    job_empties: bool,
    terminated: bool,
}

impl Teardown for FakeTeardown {
    fn host_exited(&mut self) -> bool {
        self.host_exits || self.terminated && self.exits_after_terminate
    }

    fn terminate_job(&mut self) {
        self.terminated = true;
    }

    fn job_empty(&mut self) -> bool {
        self.job_empties
    }
}

/// A host that exits with an empty job is reaped without ending the job.
#[test]
fn a_clean_host_is_reaped_without_termination() {
    let mut steps = FakeTeardown { host_exits: true, job_empties: true, ..FakeTeardown::default() };
    assert!(reap(&mut steps, Instant::now() + Duration::from_millis(100)));
    assert!(!steps.terminated);
}

/// A host that never exits ends its job, and a job end that reaps it counts as reaped.
#[test]
fn a_wedged_host_is_terminated_and_then_reaped() {
    let mut steps =
        FakeTeardown { exits_after_terminate: true, job_empties: true, ..FakeTeardown::default() };
    assert!(reap(&mut steps, Instant::now() + Duration::from_millis(100)));
    assert!(steps.terminated);
}

/// A failed cleanup is detected, never assumed: a host that survives the job end, or a job that
/// still holds a process after the host exits, reports unreaped within the one deadline.
#[test]
fn a_failed_teardown_reports_unreaped_within_its_deadline() {
    let deadline = Instant::now() + Duration::from_millis(100);
    let mut survivor = FakeTeardown { job_empties: true, ..FakeTeardown::default() };
    assert!(!reap(&mut survivor, deadline));
    assert!(survivor.terminated);
    let mut leftover = FakeTeardown { host_exits: true, ..FakeTeardown::default() };
    assert!(!reap(&mut leftover, deadline));
    // Both share one deadline, so the whole teardown ends shortly after it.
    assert!(Instant::now() < deadline + Duration::from_millis(200));
}

/// A scripted host outside its job: whether the kill call and the reap succeed.
struct FakeUnjobbed {
    kill_works: bool,
    exits: bool,
}

impl Unjobbed for FakeUnjobbed {
    fn kill(&mut self) -> bool {
        self.kill_works
    }

    fn exited(&mut self) -> bool {
        self.exits
    }
}

/// A host that never joined its job reports a failed kill and a failed reap separately, so an
/// escaped host is named rather than hidden behind the assignment error.
#[test]
fn an_unjobbed_host_reports_its_kill_and_reap() {
    let deadline = Instant::now() + Duration::from_millis(60);
    assert_eq!(
        abandon(&mut FakeUnjobbed { kill_works: true, exits: true }, deadline),
        (true, true)
    );
    assert_eq!(
        abandon(&mut FakeUnjobbed { kill_works: false, exits: false }, deadline),
        (false, false)
    );
}

/// Only an exact `go` line confirms the job: EOF, a read error, a missing newline, CRLF or
/// extra bytes all leave the host spawning nothing.
#[test]
fn only_an_exact_go_line_confirms_the_job() {
    assert!(confirmed(&Ok(3), "go\n"));
    assert!(!confirmed(&Ok(0), ""));
    assert!(!confirmed(&Err(std::io::Error::other("closed")), "go\n"));
    assert!(!confirmed(&Ok(2), "go"));
    assert!(!confirmed(&Ok(4), "go\r\n"));
    assert!(!confirmed(&Ok(6), "go\ngo\n"));
}

/// A leftover burst is discarded and then a quiet window settles the input.
#[test]
fn a_leftover_burst_settles() {
    let (sender, receiver) = mpsc::sync_channel(16);
    sender.send(vec![b'x'; 100]).expect("queue a leftover");
    assert!(settle_input(&receiver));
    assert!(receiver.try_recv().is_err());
}

/// A sustained flood never settles: the drain stops at its byte or time bound instead of
/// consuming a refilled queue forever, and the input after the flood ends settles again.
#[test]
fn a_flood_stops_the_settle_and_a_later_case_settles() {
    let (sender, receiver) = mpsc::sync_channel(16);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let flooder = std::thread::spawn(move || {
        while stop_rx.try_recv().is_err() {
            if sender.send(vec![b'f'; 4096]).is_err() {
                // When: send fails, the receiver is gone and the flood ends.
                return;
            }
        }
    });
    let started = Instant::now();
    assert!(!settle_input(&receiver));
    assert!(started.elapsed() <= SETTLE_DEADLINE + Duration::from_millis(200));
    stop_tx.send(()).expect("stop the flood");
    // Drain what the flooder queued before it stopped, then the next case's input settles.
    while receiver.recv_timeout(Duration::from_millis(50)).is_ok() {}
    flooder.join().expect("flooder ends");
    assert!(settle_input(&receiver));
}

/// A settle on a queue whose reader ended is settled: nothing more can arrive.
#[test]
fn a_closed_reader_is_settled() {
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(1);
    drop(sender);
    assert!(settle_input(&receiver));
}

/// A reply up to MAX_READ_BYTES is kept whole; one byte more is an overflow, never a cut reply
/// that could read as a short one.
#[test]
fn a_reply_past_the_cap_is_an_overflow() {
    let (sender, receiver) = mpsc::sync_channel(32);
    for _ in 0..16 {
        sender.send(vec![b'r'; 4096]).expect("queue a whole reply");
    }
    assert_eq!(collect_input(&receiver), Input::Bytes(vec![b'r'; MAX_READ_BYTES]));
    for _ in 0..16 {
        sender.send(vec![b'r'; 4096]).expect("queue a full reply");
    }
    sender.send(vec![b'r']).expect("queue one byte past it");
    assert_eq!(collect_input(&receiver), Input::Overflow);
}

/// A reply ends READ_QUIET after its last byte, well before READ_DEADLINE.
#[test]
fn a_reply_ends_after_its_quiet_window() {
    let (sender, receiver) = mpsc::sync_channel(4);
    sender.send(b"reply".to_vec()).expect("queue a reply");
    let started = Instant::now();
    assert_eq!(collect_input(&receiver), Input::Bytes(b"reply".to_vec()));
    assert!(started.elapsed() < READ_DEADLINE);
    drop(sender);
}

/// A leftover larger than MAX_SETTLE_BYTES is a flood even when it stops at once: the byte bound
/// refuses it on its own, before any quiet window could make it look settled.
#[test]
fn a_leftover_past_the_byte_bound_never_settles() {
    let chunk_count = MAX_SETTLE_BYTES / 4096 + 1;
    let (sender, receiver) = mpsc::sync_channel(chunk_count);
    for _ in 0..chunk_count {
        sender.send(vec![b'l'; 4096]).expect("queue a leftover past the bound");
    }
    drop(sender);
    assert!(!settle_input(&receiver));
}
