//! Pins the phase boundaries: each phase's counter delta covers exactly its own span, and work
//! between phases (the gap, checkpoints, progress writes) is credited to no phase.

use std::cell::Cell;

use super::*;
use crate::counters::{FieldValue, Section, SourceValue};

/// Totals whose only supported field is `attempts`, standing in for an App snapshot.
fn totals_with_attempts(attempts: u64) -> CounterTotals {
    let mut totals = CounterTotals::unsupported();
    let read = |name: &str| {
        if name == "attempts" {
            SourceValue::Count(attempts)
        } else {
            // When: any other field is asked for, this source does not supply it.
            SourceValue::Absent
        }
    };
    totals.add_record(&[Section::Window], read).expect("a count field");
    totals
}

/// The attempts a finished phase was credited with.
fn credited(record: &PhaseRecord) -> u64 {
    let counters = record.frame_counters.as_ref().expect("a counting phase");
    match counters.get("attempts") {
        Some(FieldValue::Count(attempts)) => *attempts,
        other => panic!("attempts reads {other:?}"),
    }
}

#[test]
fn a_publication_inside_a_phase_is_credited_to_that_phase() {
    // The meter snapshots at its start and its finish; what is published between them is the
    // phase's, however the counter stood before it began.
    let published = Cell::new(4_u64);
    let meter = PhaseMeter::start(
        "typing",
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        Some(totals_with_attempts(published.get())),
        None,
    );
    published.set(published.get() + 3);
    let record = meter.finish(
        Some(totals_with_attempts(published.get())),
        crate::transition::Completion::default(),
    );
    assert_eq!(credited(&record), 3);
}

#[test]
fn a_publication_in_the_gap_between_phases_is_credited_to_neither() {
    // Checkpoints and the progress write run between one phase's finish snapshot and the next
    // phase's start snapshot, so whatever they publish appears in no phase's delta.
    let published = Cell::new(0_u64);
    let first = PhaseMeter::start(
        "first",
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        Some(totals_with_attempts(published.get())),
        None,
    );
    published.set(published.get() + 2);
    let first = first.finish(
        Some(totals_with_attempts(published.get())),
        crate::transition::Completion::default(),
    );
    published.set(published.get() + 5);
    let second = PhaseMeter::start(
        "second",
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        Some(totals_with_attempts(published.get())),
        None,
    );
    published.set(published.get() + 1);
    let second = second.finish(
        Some(totals_with_attempts(published.get())),
        crate::transition::Completion::default(),
    );
    assert_eq!((credited(&first), credited(&second)), (2, 1));
    assert_eq!(published.get() - credited(&first) - credited(&second), 5, "the gap's work");
}

/// The body of `source`'s method `name`, up to the next method.
fn method_in(source: &str, name: &str) -> String {
    // A CRLF checkout is read as LF, so method ends match either way.
    let source = source.replace("\r\n", "\n");
    // A generic method's name is followed by its parameters' `<`, an ordinary one's by `(`.
    let start = ["(", "<"]
        .iter()
        .find_map(|opener| source.find(&format!("    fn {name}{opener}")))
        .unwrap_or_else(|| panic!("{name}"));
    let rest = &source[start + 4..];
    rest[..rest.find("\n    fn ").unwrap_or(rest.len())].to_owned()
}

/// The body of probe.rs's method `name`, up to the next method.
fn method(name: &str) -> String {
    method_in(include_str!("probe.rs"), name)
}

#[test]
fn checkpoint_and_progress_work_falls_between_phase_snapshots() {
    // A phase's finish snapshot is taken before its progress write, a checkpoint records
    // progress with no phase open, and a phase's start snapshot is the last thing before its
    // meter opens, after the GO writes.
    for ending in ["end_phase", "end_startup"] {
        let body = method(ending);
        let finish = body.find("self.finish_meter();").unwrap_or_else(|| panic!("{ending}"));
        let progress = body.find("self.record_progress();").unwrap_or_else(|| panic!("{ending}"));
        assert!(finish < progress, "{ending} writes progress inside its phase");
    }
    let finish = method("finish_meter");
    let snapshot = finish.find("let counters_end = self.counter_totals();").expect("end snapshot");
    assert!(snapshot < finish.find("meter.finish(counters_end, completion)").expect("finish"));
    let begin = method("begin_phase");
    let go_write = begin.find("go/{role}").expect("GO write");
    let snapshot =
        begin.find("let counters_start = self.counter_totals();").expect("start snapshot");
    let opened = begin.find("self.meter = Some(PhaseMeter::start(").expect("meter opens");
    assert!(go_write < snapshot && snapshot < opened);
    assert!(!begin[snapshot..opened].contains("record_progress"));
    let advance = method("advance_steps");
    let checkpoint = advance.find("Step::Checkpoint(label) => {").expect("checkpoint step");
    let progress =
        checkpoint + advance[checkpoint..].find("self.record_progress();").expect("progress");
    assert!(!advance[checkpoint..progress].contains("PhaseMeter::start"));
}

#[test]
fn boundary_scans_read_a_crlf_checkout_as_they_read_an_lf_one() {
    // Windows CI checks sources out with CRLF line ends; the method bodies the boundary scan
    // reads must be the same either way.
    let lf_source = include_str!("probe.rs").replace("\r\n", "\n");
    let crlf_source = lf_source.replace('\n', "\r\n");
    for name in ["end_phase", "end_startup", "finish_meter", "begin_phase", "advance_steps"] {
        assert_eq!(method_in(&crlf_source, name), method_in(&lf_source, name), "{name}");
    }
}

#[test]
fn a_pending_barrier_and_an_anchored_hold_set_the_phase_deadline() {
    // The probe's wake includes each pending barrier's bound, and a hold anchored to an earlier phase
    // ends from that phase's end; a met barrier adds no deadline.
    let act = Instant::now();
    let barrier = FrameBarrier::new(act, 0, waits::MEDIA_FREE_WAIT, false);
    assert_eq!(
        barrier_phase_deadline(&PhaseEnd::MediaFree, Some(&barrier), act, None),
        Some(act + waits::MEDIA_FREE_WAIT)
    );
    let mut met = barrier;
    met.observe(act, 1, 0);
    assert_eq!(barrier_phase_deadline(&PhaseEnd::MediaFree, Some(&met), act, None), None);
    let hold = PhaseEnd::HoldFrom { anchor: "media-free", hold_ms: 65_000 };
    let late_start = act + Duration::from_secs(40);
    assert_eq!(
        barrier_phase_deadline(&hold, None, late_start, Some(act)),
        Some(act + Duration::from_secs(65))
    );
    assert_eq!(
        barrier_phase_deadline(&PhaseEnd::Hold(1), None, act, None),
        None,
        "not a barrier phase"
    );
}

#[test]
fn the_end_checkpoint_carries_the_frame_texture_only_with_the_feature() {
    // With `perf-frame-texture` the `end` checkpoint reads width x height x 4 from the renderer's
    // frame texture; any other checkpoint, and a build without the feature, records nothing.
    let expected = cfg!(feature = "perf-frame-texture").then_some(1920 * 1080 * 4);
    assert_eq!(checkpoint_frame_texture_bytes("end", Some((1920, 1080))), expected);
    assert_eq!(checkpoint_frame_texture_bytes("released", Some((1920, 1080))), None);
    assert_eq!(checkpoint_frame_texture_bytes("end", None), None);
}

/// A headless probe running S11/release's phase `name`, its meter open and its barrier started at `act`,
/// with the renderer readings stubbed at `frames` presented and an empty image atlas.
fn release_probe(name: &str, act: Instant, frames: u64) -> (Probe, PhaseSpec) {
    let plan = scenarios::plan_for("S11", "release", false, Host::Posix).expect("S11/release");
    let (index, phase) = plan
        .steps
        .iter()
        .enumerate()
        .find_map(|(index, step)| match step {
            Step::Phase(phase) if phase.name == name => Some((index, phase.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("S11/release has no {name} phase"));
    let request = RunArgs {
        scenario: "S11",
        variant: "release",
        managed: false,
        short: false,
        laps: false,
        counters: false,
        harness_hash: None,
        scratch: String::from("unused"),
        capture_delivery: false,
    };
    let app = App::new(Theme::default(), Config::default(), Keymap::default());
    let mut probe =
        Probe::new(app, plan, request, PathBuf::from("unused"), act + Duration::from_secs(3600));
    probe.test_readings = Some((frames, ResourceAmount::default()));
    probe.stage = Stage::Steps(index);
    probe.meter = Some(PhaseMeter::start(
        phase.name,
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        None,
        None,
    ));
    probe.start_barrier(&phase.end, act);
    (probe, phase)
}

/// The image atlas reading of a promoted atlas: its full capacity, holding `items` images.
fn promoted_atlas(items: usize) -> ResourceAmount {
    ResourceAmount { bytes: 2048 * 2048 * 4, items }
}

#[test]
fn the_media_free_barrier_baselines_the_renderers_frame_count_at_its_act() {
    // The barrier's baseline is the renderer's presented-frame count when the act runs: frames that
    // presented before it (7 here) never end the phase; the first frame the renderer reports after it does.
    let act = Instant::now();
    let (mut probe, phase) = release_probe("media-free", act, 7);
    probe.observe_barrier_frame(act + Duration::from_millis(10));
    assert!(!probe.phase_done(&phase, act + Duration::from_millis(10)), "no frame after the act");
    probe.test_readings = Some((8, ResourceAmount::default()));
    probe.observe_barrier_frame(act + Duration::from_millis(20));
    assert!(probe.phase_done(&phase, act + Duration::from_millis(20)), "the next frame ends it");
}

#[test]
fn the_reshow_barrier_reads_image_atlas_items_never_its_capacity() {
    // A re-show frame whose image atlas has its capacity but holds no image does not end the phase; the
    // first frame whose atlas holds an item does.
    let act = Instant::now();
    let (mut probe, phase) = release_probe("reshow", act, 40);
    probe.test_readings = Some((41, promoted_atlas(0)));
    probe.observe_barrier_frame(act + Duration::from_millis(16));
    assert!(!probe.phase_done(&phase, act + Duration::from_millis(16)), "capacity alone");
    probe.test_readings = Some((42, promoted_atlas(1)));
    probe.observe_barrier_frame(act + Duration::from_millis(33));
    assert!(probe.phase_done(&phase, act + Duration::from_millis(33)), "an item ends it");
}

#[test]
fn an_expired_barrier_invalidates_the_run_even_with_a_late_frame() {
    // With no qualifying frame by the bound the step loop gets an invalid reason, and a frame the
    // renderer reports exactly at the bound neither ends the phase nor clears the reason.
    let act = Instant::now();
    for (name, bound) in [("media-free", waits::MEDIA_FREE_WAIT), ("reshow", waits::RESHOW_WAIT)] {
        let (mut probe, phase) = release_probe(name, act, 0);
        let just_before = act + bound - Duration::from_millis(1);
        assert_eq!(probe.expired_barrier_reason(&phase, just_before), None, "{name}");
        probe.test_readings = Some((1, promoted_atlas(1)));
        probe.observe_barrier_frame(act + bound);
        assert!(!probe.phase_done(&phase, act + bound), "{name}: a late frame is not counted");
        let reason = probe.expired_barrier_reason(&phase, act + bound).expect("expired");
        assert!(reason.contains(&format!("{} s", bound.as_secs())), "{name}: {reason}");
    }
    // The step loop checks expiry, and invalidates with its reason, before asking whether the phase ended.
    let advance = method("advance_steps");
    let expiry = advance.find("self.expired_barrier_reason(phase,").expect("expiry check");
    let invalid =
        expiry + advance[expiry..].find("self.invalidate(event_loop, reason)").expect("invalid");
    assert!(
        invalid < advance.find("if !self.phase_done(phase, Instant::now())").expect("done check")
    );
}

#[test]
fn a_pending_barrier_is_the_probes_next_wake_and_a_met_one_is_not() {
    // The barrier's bound joins the probe's own wake, so an expiry is noticed without other events; once
    // a qualifying frame has presented the wake falls back to the run deadline.
    let act = Instant::now();
    let (mut probe, _phase) = release_probe("media-free", act, 3);
    assert_eq!(probe.next_deadline(act), Some(act + waits::MEDIA_FREE_WAIT));
    probe.test_readings = Some((4, ResourceAmount::default()));
    probe.observe_barrier_frame(act + Duration::from_millis(5));
    assert_eq!(probe.next_deadline(act + Duration::from_millis(5)), Some(probe.run_deadline));
}

#[test]
fn dispatch_and_phase_start_drive_the_barrier() {
    // Every presenting dispatch reports to the barrier, and a phase starts its barrier before its act.
    let forward = method("forward");
    let advanced = forward.find("if advanced {").expect("advanced branch");
    assert!(forward[advanced..].contains("self.observe_barrier_frame(ended);"));
    let begin = method("begin_phase");
    let start =
        begin.find("self.start_barrier(&phase.end, Instant::now());").expect("barrier start");
    assert!(start < begin.find("for act in &phase.enter {").expect("acts"));
}

/// The run deadline the checkpoint fixture's wakes are bounded by; far past every checkpoint wake.
const FIXTURE_RUN_DEADLINE: Duration = Duration::from_secs(60);

/// One checkpoint driven through the probe's production checkpoint path on a fake clock: a private
/// scratch directory for its request and footprint files, the pending state and records the probe
/// keeps, the plan step the probe would be on, and the number of times the sampler was called.
///
/// Each turn runs `checkpoint_turn` with an injected clock and file check, then moves the plan step
/// on exactly when `plan_moves_on` says so, as `Probe::advance_checkpoint` and the step loop do.
/// Its wake is `steps_wake`, the function `Probe::next_deadline` uses while steps run.
struct CheckpointRun {
    scratch: PathBuf,
    start: Instant,
    managed: bool,
    sampling: bool,
    /// The fake clock every decision reads; shared, so a sampler reads the time a decision saw.
    clock: std::rc::Rc<Cell<Instant>>,
    /// When set, the next `.done` existence check moves the clock here, as slow I/O would.
    jump_on_done: Cell<Option<Instant>>,
    pending: Option<PendingCheckpoint>,
    records: Vec<CheckpointRecord>,
    /// The plan step the probe is on: 0 until the checkpoint moves the plan on.
    plan_step: usize,
    sampler_calls: u32,
}

impl CheckpointRun {
    fn new(name: &str, managed: bool) -> Self {
        let scratch = std::env::temp_dir().join(format!(
            "sonicterm-checkpoint-{name}-{}-{}",
            std::process::id(),
            nonce_seed()
        ));
        std::fs::create_dir_all(scratch.join("checkpoints")).expect("scratch");
        let start = Instant::now();
        CheckpointRun {
            scratch,
            start,
            managed,
            sampling: true,
            clock: std::rc::Rc::new(Cell::new(start)),
            jump_on_done: Cell::new(None),
            pending: None,
            records: Vec::new(),
            plan_step: 0,
            sampler_calls: 0,
        }
    }

    fn at(&self, millis: u64) -> Instant {
        self.start + Duration::from_millis(millis)
    }

    /// The fake clock's time, in ms from the start.
    fn now_ms(&self) -> u64 {
        self.clock.get().saturating_duration_since(self.start).as_millis() as u64
    }

    /// One turn at checkpoint `ordinal` (`label`) starting at `millis`, with `sample` answering
    /// each attempt (index, label, attempt) with whether it measured every pane.
    fn turn_with(
        &mut self,
        ordinal: usize,
        label: &'static str,
        millis: u64,
        mut sample: impl FnMut(usize, &str, u32) -> bool,
    ) -> CheckpointOutcome {
        self.clock.set(self.at(millis));
        let (clock, jump) = (&*self.clock, &self.jump_on_done);
        let read_clock = || clock.get();
        let exists = |path: &Path| {
            if path.extension().is_some_and(|extension| extension == "done") {
                if let Some(later) = jump.take() {
                    clock.set(later);
                }
            }
            path.exists()
        };
        let site = CheckpointSite {
            scratch: &self.scratch,
            managed: self.managed,
            sampling: self.sampling,
            footprint_wait: Duration::from_millis(1_000),
            clock: &read_clock,
            exists: &exists,
        };
        let step = CheckpointStep {
            ordinal,
            label,
            unix_s: 1_000.0 + millis as f64 / 1_000.0,
            fresh_after_unix_s: None,
            frame_texture_bytes: None,
        };
        let calls = &mut self.sampler_calls;
        let outcome = checkpoint_turn(
            &mut self.pending,
            &mut self.records,
            &site,
            &step,
            |index, label, attempt| {
                *calls += 1;
                sample(index, label, attempt)
            },
        );
        if plan_moves_on(&outcome) {
            self.plan_step += 1;
        }
        outcome
    }

    /// One turn at checkpoint `ordinal` (`label`) at `millis`; `complete` answers each attempt from
    /// the clock's time, in ms, when the attempt is taken.
    fn turn_at(
        &mut self,
        ordinal: usize,
        label: &'static str,
        millis: u64,
        complete: impl Fn(u64) -> bool,
    ) -> CheckpointOutcome {
        let clock = std::rc::Rc::clone(&self.clock);
        let start = self.start;
        self.turn_with(ordinal, label, millis, move |_, _, _| {
            complete(clock.get().saturating_duration_since(start).as_millis() as u64)
        })
    }

    fn turn(&mut self, millis: u64, complete: impl Fn(u64) -> bool) -> CheckpointOutcome {
        self.turn_at(0, "end", millis, complete)
    }

    /// The probe's next wake at `millis` while it runs its steps, with no phase wake of its own.
    fn wake(&self, millis: u64) -> Instant {
        steps_wake(self.pending.as_ref(), self.at(millis), [], self.start + FIXTURE_RUN_DEADLINE)
    }

    /// Answer the footprint request as the comparison script does: the JSON, then `.done`.
    fn answer_footprint(&self, stem: &str) {
        std::fs::write(self.scratch.join(format!("checkpoints/{stem}.json")), b"{}").expect("json");
        std::fs::write(self.scratch.join(format!("checkpoints/{stem}.done")), b"").expect("done");
    }

    /// Request files written so far.
    fn requests(&self) -> usize {
        std::fs::read_dir(self.scratch.join("checkpoints"))
            .expect("scratch")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".request"))
            })
            .count()
    }

    /// The request's text, to show a re-entry did not rewrite it.
    fn request_text(&self, stem: &str) -> String {
        std::fs::read_to_string(self.scratch.join(format!("checkpoints/{stem}.request")))
            .expect("request")
    }
}

impl Drop for CheckpointRun {
    // Lifecycle: the scratch directory belongs to this test run alone; removing it is best effort.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

#[test]
fn an_early_footprint_waits_for_a_complete_sample() {
    // The footprint is answered at 10 ms while the first sample was partial: the step holds, wakes
    // by the retry at 50 ms rather than at the run deadline, and advances at 50 ms with the
    // footprint recorded. One record and one request however many turns.
    let mut run = CheckpointRun::new("early-footprint", true);
    let partial_until_50 = |millis: u64| millis >= 50;
    assert_eq!(run.turn(0, partial_until_50), CheckpointOutcome::Wait);
    let request = run.request_text("0-end");
    run.answer_footprint("0-end");
    assert_eq!(run.turn(10, partial_until_50), CheckpointOutcome::Wait);
    assert!(run.wake(10) <= run.at(50), "wakes for the retry, not at the run deadline");
    assert_eq!(run.plan_step, 0, "the plan holds while the sample is partial");
    assert_eq!(run.turn(50, partial_until_50), CheckpointOutcome::Advance);
    assert_eq!(run.plan_step, 1, "the plan moves on once, at 50 ms");
    assert_eq!(run.records.len(), 1);
    assert_eq!(run.records[0].footprint_file.as_deref(), Some("checkpoints/0-end.json"));
    assert_eq!(run.records[0].sampling, Some("complete"));
    assert_eq!(run.records[0].attempts, Some(2));
    assert_eq!((run.requests(), run.request_text("0-end")), (1, request));
}

#[test]
fn an_unmanaged_checkpoint_waits_for_its_sampling() {
    // No footprint: the step still waits for a complete sample, waking within 50 ms each turn, and
    // advances at 100 ms when the third attempt is complete.
    let mut run = CheckpointRun::new("unmanaged", false);
    let complete_from_100 = |millis: u64| millis >= 100;
    for millis in [0, 50] {
        assert_eq!(run.turn(millis, complete_from_100), CheckpointOutcome::Wait);
        assert!(run.wake(millis) <= run.at(millis + 50), "wakes within 50 ms at {millis} ms");
        assert_eq!(run.plan_step, 0, "the plan holds at {millis} ms");
    }
    assert_eq!(run.turn(100, complete_from_100), CheckpointOutcome::Advance);
    assert_eq!(run.plan_step, 1);
    assert_eq!((run.records.len(), run.requests()), (1, 1));
    assert_eq!(run.records[0].footprint_file, None);
}

#[test]
fn an_unmanaged_checkpoint_advances_when_ten_attempts_run_out() {
    // A sampler that never completes and turns every 50 ms: the tenth attempt, at 450 ms, exhausts
    // sampling and the step advances in that same turn, before the 500 ms deadline.
    let mut run = CheckpointRun::new("count-limit", false);
    for millis in (0..450).step_by(50) {
        assert_eq!(run.turn(millis, |_| false), CheckpointOutcome::Wait, "{millis} ms");
        assert!(run.wake(millis) <= run.at(millis + 50), "wakes within 50 ms at {millis} ms");
    }
    assert_eq!(run.plan_step, 0);
    assert_eq!(run.turn(450, |_| false), CheckpointOutcome::Advance);
    assert_eq!(run.plan_step, 1, "the plan moves on at 450 ms, not at the run deadline");
    assert_eq!(run.sampler_calls, 10);
    assert_eq!(run.records[0].sampling, Some("exhausted"));
    assert_eq!(run.records[0].last_attempt_complete, Some(false));
}

#[test]
fn a_complete_sample_waits_for_the_footprint() {
    // The first sample is complete; the footprint arrives at 300 ms. The step polls every 50 ms,
    // takes no further sample, and advances at the first poll that sees the answer.
    let mut run = CheckpointRun::new("sample-first", true);
    for millis in (0..300).step_by(50) {
        assert_eq!(run.turn(millis, |_| true), CheckpointOutcome::Wait, "{millis} ms");
        assert_eq!(run.wake(millis), run.at(millis + 50), "polls every 50 ms");
    }
    run.answer_footprint("0-end");
    assert_eq!(run.turn(300, |_| true), CheckpointOutcome::Advance);
    assert_eq!(run.sampler_calls, 1);
    assert_eq!(run.requests(), 1);
}

#[test]
fn exhausted_sampling_with_an_answered_footprint_advances_and_an_expired_one_invalidates() {
    // Exhausted sampling does not hold an answered checkpoint; a footprint that never answers
    // within its wait ends the run invalid, as before sampling existed.
    let mut answered = CheckpointRun::new("exhausted-answered", true);
    answered.answer_footprint("0-end");
    for millis in (0..450).step_by(50) {
        assert_eq!(answered.turn(millis, |_| false), CheckpointOutcome::Wait);
    }
    assert_eq!(answered.turn(450, |_| false), CheckpointOutcome::Advance);
    assert_eq!(answered.records[0].sampling, Some("exhausted"));

    let mut expired = CheckpointRun::new("expired", true);
    assert_eq!(expired.turn(0, |_| true), CheckpointOutcome::Wait);
    match expired.turn(1_000, |_| true) {
        CheckpointOutcome::Invalid(reason) => assert!(reason.contains("no .done"), "{reason}"),
        other => panic!("an expired footprint must invalidate, got {other:?}"),
    }
}

#[test]
fn a_pending_checkpoint_of_another_identity_invalidates_and_the_next_gets_its_own_record() {
    // A checkpoint pending for index 0 when the plan reaches index 1 would mix two checkpoints, so
    // the run is invalid. A fresh run's two checkpoints each get their own record and request.
    let mut run = CheckpointRun::new("identity", false);
    assert_eq!(run.turn_at(0, "settled", 0, |_| false), CheckpointOutcome::Wait);
    match run.turn_at(1, "end", 10, |_| false) {
        CheckpointOutcome::Invalid(reason) => {
            assert!(reason.contains("checkpoint identity changed"), "{reason}")
        }
        other => panic!("a changed identity must invalidate, got {other:?}"),
    }

    let mut fresh = CheckpointRun::new("identity-fresh", false);
    assert_eq!(fresh.turn_at(0, "settled", 0, |_| true), CheckpointOutcome::Advance);
    assert_eq!(fresh.turn_at(1, "end", 100, |_| true), CheckpointOutcome::Advance);
    let indices: Vec<_> = fresh.records.iter().map(|record| (record.index, record.label)).collect();
    assert_eq!(indices, [(0, "settled"), (1, "end")]);
    assert_eq!(fresh.requests(), 2);
}

#[test]
fn delayed_turns_exhaust_sampling_at_its_deadline_without_a_late_sample() {
    // Turns delayed to 0, 120, 240, 360 and 480 ms take five partial attempts; the wake after the
    // fifth is the 500 ms deadline, not the 530 ms retry. A turn at the deadline, or a late one at
    // 610 ms, exhausts sampling without a sixth sample and advances; the run deadline is not involved.
    for last_turn in [500, 610] {
        let mut run = CheckpointRun::new(&format!("deadline-{last_turn}"), false);
        for millis in [0, 120, 240, 360, 480] {
            assert_eq!(run.turn(millis, |_| false), CheckpointOutcome::Wait);
        }
        assert_eq!(run.wake(480), run.at(500));
        assert_eq!(run.plan_step, 0);
        assert_eq!(run.turn(last_turn, |_| true), CheckpointOutcome::Advance, "at {last_turn} ms");
        assert_eq!(run.plan_step, 1);
        assert_eq!(run.sampler_calls, 5, "no sixth sample at {last_turn} ms");
        assert_eq!(run.records[0].sampling, Some("exhausted"));
        assert_eq!(run.records[0].attempts, Some(5));
        assert_eq!(run.records[0].last_attempt_complete, Some(false));
    }
}

#[test]
fn a_build_without_the_hook_advances_without_sampling() {
    // With no hook the checkpoint has no sampling: an unmanaged step advances in its first turn and
    // its record carries none of the sampling fields. The constant follows the build's feature.
    let scratch = CheckpointRun::new("unsupported", false);
    let read_clock = || scratch.start;
    let exists = |path: &Path| path.exists();
    let site = CheckpointSite {
        scratch: &scratch.scratch,
        managed: false,
        sampling: false,
        footprint_wait: Duration::from_secs(1),
        clock: &read_clock,
        exists: &exists,
    };
    let step = CheckpointStep {
        ordinal: 0,
        label: "end",
        unix_s: 1_000.0,
        fresh_after_unix_s: None,
        frame_texture_bytes: None,
    };
    let (mut pending, mut records) = (None, Vec::new());
    let outcome = checkpoint_turn(&mut pending, &mut records, &site, &step, |_, _, _| {
        panic!("a build without the hook never samples")
    });
    assert_eq!(outcome, CheckpointOutcome::Advance);
    assert_eq!((records[0].sampling, records[0].attempts), (None, None));
    assert_eq!(CHECKPOINT_MEMORY, cfg!(feature = "perf-hook-checkpoint-memory"));
}

#[test]
fn a_turn_whose_footprint_check_passes_the_deadline_takes_no_sample() {
    // The decision to sample reads the clock after the turn's file checks. Attempt 1 at 0 ms is
    // partial; a re-entry starts at 490 ms, inside the window, but its `.done` check takes until
    // 610 ms. The sampler (which would answer complete) is not called: sampling is exhausted with
    // one attempt. With `.done` present that same turn advances; without it the step waits and
    // advances once the footprint is answered.
    for answered_in_time in [true, false] {
        let mut run = CheckpointRun::new(&format!("late-check-{answered_in_time}"), true);
        assert_eq!(run.turn(0, |_| false), CheckpointOutcome::Wait);
        if answered_in_time {
            run.answer_footprint("0-end");
        }
        run.jump_on_done.set(Some(run.at(610)));
        let outcome = run.turn(490, |_| true);
        assert_eq!(run.now_ms(), 610, "precondition: the `.done` check moved the clock");
        assert_eq!(run.sampler_calls, 1, "no sample after the window, answered {answered_in_time}");
        assert_eq!(run.records[0].sampling, Some("exhausted"));
        assert_eq!(run.records[0].attempts, Some(1));
        if answered_in_time {
            assert_eq!(outcome, CheckpointOutcome::Advance);
        } else {
            assert_eq!(outcome, CheckpointOutcome::Wait);
            assert_eq!(run.plan_step, 0);
            run.answer_footprint("0-end");
            assert_eq!(run.turn(650, |_| true), CheckpointOutcome::Advance);
            assert_eq!(run.sampler_calls, 1);
        }
        assert_eq!(run.plan_step, 1);
    }
}

#[test]
fn a_first_sample_after_slow_checkpoint_io_opens_its_window_at_that_sample() {
    // The sampling window opens at its first attempt, not when the turn began. The first turn
    // starts at 0 ms, but its `.done` check takes until 610 ms, so the first sample is taken at
    // 610 ms and the deadline is 610 + 500 ms. Every sample, partial or complete, is taken before
    // the deadline in force; none is authorized against a deadline that had already passed.
    for complete in [true, false] {
        let mut run = CheckpointRun::new(&format!("late-first-{complete}"), true);
        run.jump_on_done.set(Some(run.at(610)));
        let taken = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let turn = |run: &mut CheckpointRun, millis: u64| {
            let taken = std::rc::Rc::clone(&taken);
            let outcome = run.turn(millis, move |sampled_ms| {
                taken.borrow_mut().push(sampled_ms);
                complete
            });
            if let Some(sampling) = run.pending.as_ref().and_then(|pending| pending.sampling) {
                assert_eq!(sampling.deadline, run.at(1_110), "the window opens at 610 ms");
            }
            outcome
        };
        assert_eq!(turn(&mut run, 0), CheckpointOutcome::Wait);
        assert_eq!(
            taken.borrow().first(),
            Some(&610),
            "the first sample is taken at the fresh time"
        );
        // The footprint is answered before its own 1 s wait ends, so only sampling holds the step.
        run.answer_footprint("0-end");
        if complete {
            assert_eq!(turn(&mut run, 650), CheckpointOutcome::Advance);
            assert_eq!(run.records[0].sampling, Some("complete"));
            assert_eq!(run.records[0].attempts, Some(1));
        } else {
            // Retries every 50 ms from 660 ms; the tenth attempt, at 1060 ms, exhausts sampling.
            for millis in (660..=1_010).step_by(50) {
                assert_eq!(turn(&mut run, millis), CheckpointOutcome::Wait, "{millis} ms");
            }
            assert_eq!(turn(&mut run, 1_060), CheckpointOutcome::Advance);
            assert_eq!(run.records[0].sampling, Some("exhausted"));
            assert_eq!(run.records[0].attempts, Some(10));
        }
        assert!(
            taken.borrow().iter().all(|&sampled_ms| sampled_ms < 1_110),
            "every sample precedes its deadline: {:?}",
            taken.borrow()
        );
        assert_eq!(run.sampler_calls as usize, taken.borrow().len());
    }
}

#[test]
fn with_nothing_pending_the_steps_wake_is_the_soonest_phase_wake() {
    // `steps_wake` is what `Probe::next_deadline` uses during steps: with no pending checkpoint it is
    // the soonest phase wake, or the run deadline when there is none, never later than that deadline.
    let start = Instant::now();
    let deadline = start + FIXTURE_RUN_DEADLINE;
    let at = |millis: u64| start + Duration::from_millis(millis);
    assert_eq!(steps_wake(None, start, [Some(at(300)), None, Some(at(120))], deadline), at(120));
    assert_eq!(steps_wake(None, start, [None, None], deadline), deadline);
    assert_eq!(
        steps_wake(None, start, [Some(deadline + Duration::from_secs(1))], deadline),
        deadline
    );
}

#[test]
fn only_an_advanced_checkpoint_moves_the_plan_on() {
    // The step loop starts the next phase only when `plan_moves_on` holds: a waiting or invalid
    // checkpoint keeps the plan where it is.
    assert!(plan_moves_on(&CheckpointOutcome::Advance));
    assert!(!plan_moves_on(&CheckpointOutcome::Wait));
    assert!(!plan_moves_on(&CheckpointOutcome::Invalid("stopped".into())));
}

/// The golden runs `scripts/perf-compare_tests.py` reads: each case's `result.json` from the
/// production serializer and the `memory` log lines the App's hook emitted, under the build's key.
const CHECKPOINT_FIXTURE: &str = "../../scripts/perf-compare_checkpoint_fixture.json";

/// A `tracing` writer that keeps everything written to it, so a test can read the log lines the
/// production format layer produced.
#[derive(Clone, Default)]
struct LogSink(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogSink {
    type Writer = LogSink;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// One fixture case: its turns as (ms, expected outcome is Advance), and what its record must say.
struct FixtureCase {
    name: &'static str,
    managed: bool,
    turns: &'static [(u64, bool)],
    /// When set, the `.done` check of the turn at this ms moves the clock to the second time.
    jump: Option<(u64, u64)>,
    /// Answer the footprint before the turn at this ms.
    answer_before: Option<u64>,
    attempts: Option<u32>,
}

/// The cases a build with the hook writes: late exhaustion by count of turns, by a stale clock, by a
/// single turn at 600 ms, and at or after the deadline after five partial attempts.
#[cfg(feature = "perf-hook-checkpoint-memory")]
const FIXTURE_CASES: &[FixtureCase] = &[
    FixtureCase {
        name: "eight-attempts",
        managed: false,
        turns: &[
            (0, false),
            (60, false),
            (120, false),
            (180, false),
            (240, false),
            (300, false),
            (360, false),
            (420, false),
            (500, true),
        ],
        jump: None,
        answer_before: None,
        attempts: Some(8),
    },
    FixtureCase {
        name: "stale-clock",
        managed: true,
        turns: &[(0, false), (490, true)],
        jump: Some((490, 610)),
        answer_before: Some(490),
        attempts: Some(1),
    },
    FixtureCase {
        name: "single-at-600",
        managed: false,
        turns: &[(0, false), (600, true)],
        jump: None,
        answer_before: None,
        attempts: Some(1),
    },
    FixtureCase {
        name: "deadline-500",
        managed: false,
        turns: &[(0, false), (120, false), (240, false), (360, false), (480, false), (500, true)],
        jump: None,
        answer_before: None,
        attempts: Some(5),
    },
    FixtureCase {
        name: "deadline-610",
        managed: false,
        turns: &[(0, false), (120, false), (240, false), (360, false), (480, false), (610, true)],
        jump: None,
        answer_before: None,
        attempts: Some(5),
    },
];

/// The case a build without the hook writes: its checkpoint advances at once, unsampled.
#[cfg(not(feature = "perf-hook-checkpoint-memory"))]
const FIXTURE_CASES: &[FixtureCase] = &[FixtureCase {
    name: "unsupported",
    managed: false,
    turns: &[(0, true)],
    jump: None,
    answer_before: None,
    attempts: None,
}];

/// `result.json` for one fixture run: a valid S1 run with one phase and the probe's checkpoints.
fn fixture_result(checkpoints: Vec<CheckpointRecord>) -> RunResult {
    RunResult {
        harness_hash: Some("ab".repeat(32)),
        scenario: "S1",
        variant: "default",
        managed: true,
        short: true,
        laps: false,
        alloc_counting: false,
        status: Status::Valid,
        invalid_reason: None,
        harness_pid: 4242,
        grid: Some((80, 24)),
        monitor: Some(MonitorInfo {
            name: Some("Built-in Display".into()),
            refresh_rate_millihertz: Some(60_000),
            scale_factor: 2.0,
        }),
        window_path: "production",
        synthetic_occlusion: false,
        trim_hook: TrimHookOutcome::NotReached,
        trim_experiment: None,
        trim_seq_after_hook: None,
        atlas_recovery: None,
        native_focus_events_dropped: 0,
        native_cursor_rest_events_dropped: 0,
        finish_session_settled: true,
        // Fixed, so every build of the harness writes the same fixture whatever its counter feature.
        frame_counters: CountersMode::Unsupported,
        presenter: None,
        phases: vec![PhaseRecord {
            name: "workload",
            kind: "sustained",
            endpoint: None,
            completion_ms: None,
            completion_missing: None,
            start_unix_s: 990.0,
            end_unix_s: 1_000.0,
            cpu_user_s: 1.5,
            cpu_system_s: 0.5,
            presented_frames: 120,
            redraw_requested: 130,
            first_present_ms: Some(16.6),
            last_present_ms: Some(9_990.0),
            first_present_seq: Some(1),
            last_present_seq: Some(120),
            present_missing: None,
            nonpresenting_redraws: 10,
            dispatch_ms: vec![1.0, 2.0],
            slow_dispatches: Vec::new(),
            dispatch_count: 2,
            present_interval_ms: vec![16.6],
            allocations_per_frame: None,
            frame_counters: None,
            updates: None,
        }],
        latency: None,
        throughput: None,
        uncover_ms: None,
        scrollback_rows_retained: None,
        checkpoints,
        notes: Vec::new(),
    }
}

/// Run `case` through the probe's checkpoint path with the real App hook as the sampler, one of
/// two panes' parser held so every sample is partial. Returns `result.json` and the log lines.
fn run_fixture_case(case: &FixtureCase) -> (serde_json::Value, Vec<String>) {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer, Registry};
    let sink = LogSink::default();
    // The production file layer's format: no ANSI, RFC 3339 UTC stamps, `target: fields`.
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(sink.clone())
        .with_filter(EnvFilter::new("memory=info"));
    let subscriber = Registry::default().with(layer);
    let mut run = CheckpointRun::new(&format!("fixture-{}", case.name), case.managed);
    run.sampling = CHECKPOINT_MEMORY;
    sonicterm_logging::test_capture::with_default(subscriber, || {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["one", "two"]);
        let pane_ids = app.__test_child_pane_ids(window).expect("child exists");
        // An untagged periodic sample first: the comparison must never read it for the checkpoint.
        assert!(app.__test_sample_pane_retention_now(), "the periodic sample ran");
        let parser = app.__test_child_pane_parser(window, pane_ids[0]).expect("pane exists");
        let held = parser.lock();
        for &(millis, advances) in case.turns {
            if case.answer_before == Some(millis) {
                run.answer_footprint("0-end");
            }
            if let Some((_, to_ms)) = case.jump.filter(|&(at_ms, _)| at_ms == millis) {
                run.jump_on_done.set(Some(run.at(to_ms)));
            }
            let outcome = run.turn_with(0, "end", millis, |index, label, attempt| {
                sample_checkpoint_memory(&mut app, index, label, attempt)
            });
            let expected =
                if advances { CheckpointOutcome::Advance } else { CheckpointOutcome::Wait };
            assert_eq!(outcome, expected, "{} at {millis} ms", case.name);
        }
        drop(held);
    });
    assert_eq!(run.plan_step, 1, "{} moves the plan on once", case.name);
    let record = &run.records[0];
    assert_eq!(record.attempts, case.attempts, "{}", case.name);
    assert_eq!(run.sampler_calls, case.attempts.unwrap_or(0), "{}", case.name);
    if case.attempts.is_some() {
        assert_eq!(record.sampling, Some("exhausted"), "{}", case.name);
        assert_eq!(record.last_attempt_complete, Some(false), "{}", case.name);
    }
    let text = String::from_utf8(sink.0.lock().clone()).expect("log lines are UTF-8");
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    (fixture_result(run.records.clone()).to_json(), lines)
}

/// The message marker `parse_memory_line` looks for, read from the comparison script itself, so a
/// rename on either side changes what the fixture test compares.
fn memory_marker() -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/perf-compare.py");
    let source = std::fs::read_to_string(&script).expect("perf-compare.py").replace("\r\n", "\n");
    let line = source
        .lines()
        .find(|line| line.starts_with("MEMORY_MARKER = \""))
        .expect("perf-compare.py defines MEMORY_MARKER");
    line.trim_start_matches("MEMORY_MARKER = \"").trim_end_matches('"').to_owned()
}

/// The shape of a leading log stamp: `utc` for the fmt layer's `YYYY-MM-DDTHH:MM:SS[.frac]Z`, which
/// `parse_utc_stamp` reads, else `other`.
fn stamp_shape(line: &str) -> &'static str {
    let stamp = line.split_whitespace().next().unwrap_or("");
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let utc = stamp.strip_suffix('Z').is_some_and(|body| {
        let (whole, fraction) = body.split_once('.').unwrap_or((body, "0"));
        let bytes = whole.as_bytes();
        whole.len() == 19
            && [4, 7].iter().all(|&index| bytes[index] == b'-')
            && bytes[10] == b'T'
            && [13, 16].iter().all(|&index| bytes[index] == b':')
            && [
                &whole[0..4],
                &whole[5..7],
                &whole[8..10],
                &whole[11..13],
                &whole[14..16],
                &whole[17..19],
            ]
            .iter()
            .all(|part| digits(part))
            && digits(fraction)
    });
    if utc {
        "utc"
    } else {
        // When: the stamp is not the fmt layer's UTC form, the parser would drop the line.
        "other"
    }
}

/// The comparable part of a log line: what `parse_memory_line` needs to keep it (the message
/// marker, a UTC stamp, and the shape of each required total), and its checkpoint tags and pane
/// counts. Byte figures and times vary by host, so only their shapes are compared.
fn log_projection(line: &str, marker: &str) -> Vec<String> {
    let field = |name: &str| {
        let prefix = format!("{name}=");
        line.split_whitespace()
            .find_map(|field| field.strip_prefix(prefix.as_str()))
            .map(str::to_owned)
    };
    let shape = |name: &str| {
        let value = field(name);
        let kind = match value.as_deref() {
            None => "missing",
            Some(text) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => {
                "count"
            }
            Some("unsupported") => "unsupported",
            Some(_) => "other",
        };
        format!("{name}:{kind}")
    };
    let mut projection = vec![
        format!("marker:{}", line.contains(marker)),
        format!("stamp:{}", stamp_shape(line)),
        shape("renderer_total_bytes"),
        shape("session_total_bytes"),
        shape("process_resident_bytes"),
    ];
    let names = [
        "checkpoint_index",
        "checkpoint_label",
        "checkpoint_attempt",
        "checkpoint_complete",
        "panes_total",
        "panes_sampled",
        "panes_contended",
    ];
    projection.extend(
        names.iter().map(|name| format!("{name}={}", field(name).as_deref().unwrap_or("-"))),
    );
    projection
}

/// The golden checkpoint runs the comparison script reads come from this build's own code: the
/// production checkpoint path, the App's hook (one pane held, so samples are partial), the
/// production log format and the production `result.json` serializer. Each late-exhaustion case
/// ends exhausted with its attempt count and a partial last attempt; a build without the hook
/// reports `checkpoint_memory = "unsupported"`. Every line written keeps what `parse_memory_line`
/// needs (the script's own message marker, a UTC stamp, the required totals), and the comparison
/// with the committed fixture includes that structure, so a renamed message or a dropped total
/// fails here. `SONICTERM_WRITE_CHECKPOINT_FIXTURE=1` rewrites this build's half of the fixture;
/// otherwise the committed fixture must match what the build produces.
#[test]
fn the_checkpoint_fixture_matches_what_this_build_writes() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(CHECKPOINT_FIXTURE);
    let build_key = crate::record::checkpoint_memory_support();
    let mut produced = serde_json::Map::new();
    for case in FIXTURE_CASES {
        let (result, logs) = run_fixture_case(case);
        assert_eq!(result["checkpoint_memory"], build_key, "{}", case.name);
        // Every line this build writes must be one the comparison's parser keeps.
        let marker = memory_marker();
        for line in &logs {
            let projection = log_projection(line, &marker);
            assert_eq!(
                &projection[..4],
                [
                    "marker:true",
                    "stamp:utc",
                    "renderer_total_bytes:count",
                    "session_total_bytes:count"
                ],
                "{}: a memory line the parser would drop: {line}",
                case.name
            );
        }
        let tagged = logs.iter().filter(|line| line.contains("checkpoint_index=")).count();
        assert_eq!(tagged, case.attempts.unwrap_or(0) as usize, "one tagged line per attempt");
        produced.insert(case.name.to_owned(), serde_json::json!({"result": result, "logs": logs}));
    }
    let mut fixture: serde_json::Map<String, serde_json::Value> =
        std::fs::read_to_string(&fixture_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
    if std::env::var_os("SONICTERM_WRITE_CHECKPOINT_FIXTURE").is_some() {
        fixture.insert(build_key.to_owned(), serde_json::Value::Object(produced));
        let text = serde_json::to_string_pretty(&fixture).expect("fixture serializes") + "\n";
        std::fs::write(&fixture_path, text).expect("fixture written");
        return;
    }
    let committed = fixture
        .get(build_key)
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("no `{build_key}` runs in {}", fixture_path.display()));
    assert_eq!(
        committed.keys().collect::<Vec<_>>(),
        produced.keys().collect::<Vec<_>>(),
        "the fixture's cases"
    );
    let marker = memory_marker();
    for (name, run) in &produced {
        assert_eq!(committed[name]["result"], run["result"], "{name}: result.json");
        let lines = |value: &serde_json::Value| -> Vec<Vec<String>> {
            value["logs"]
                .as_array()
                .expect("logs")
                .iter()
                .map(|line| log_projection(line.as_str().expect("line"), &marker))
                .collect()
        };
        assert_eq!(lines(&committed[name]), lines(run), "{name}: memory lines");
    }
}

#[test]
fn startup_records_the_presenter_on_every_host() {
    // macOS results need the presenter to show whether a run stayed on the hardware path.
    let source = include_str!("probe.rs").replace("\r\n", "\n");
    let start = source.find("fn end_startup(").expect("end_startup exists");
    let body = &source[start..start + source[start..].find("\n    }\n").unwrap()];
    assert!(body.contains("self.presenter = self.presenter_record();"), "{body}");
    assert!(!body.contains("cfg!(windows)"), "no host gate: {body}");
    let start = source.find("fn presenter_record(&self)").expect("presenter_record exists");
    let body = &source[start..start + source[start..].find("\n    }\n").unwrap()];
    assert!(body.contains("presenter_record_for("), "{body}");
}

#[test]
fn atlas_readings_join_the_latest_record_of_their_checkpoint_in_attempt_order() {
    // Each sampling attempt's atlas reading belongs to the checkpoint that attempt sampled, and only to
    // its latest record, so perf-compare can pair a memory sample with the reading of the same attempt.
    let reading = |attempt| AtlasReading {
        attempt,
        main_window: None,
        counted_glyph_atlas_growths: None,
        closed_glyph_atlas_growths: None,
    };
    let mut records = vec![
        CheckpointRecord { index: 0, label: "settled", ..CheckpointRecord::default() },
        CheckpointRecord { index: 1, label: "end", ..CheckpointRecord::default() },
    ];
    attach_atlas_readings(&mut records, vec![(1, reading(1)), (1, reading(2))]);
    assert!(records[0].atlas_readings.is_empty());
    let attempts: Vec<u32> = records[1].atlas_readings.iter().map(|found| found.attempt).collect();
    assert_eq!(attempts, [1, 2]);
}

/// A phase meter hands the update count it started with to the record it finishes.
#[test]
fn a_phase_meter_records_the_update_count_it_started_with() {
    assert_eq!(
        PhaseMeter::start(
            "stream",
            crate::scenarios::PhaseKind::Sustained,
            None,
            false,
            None,
            Some(300)
        )
        .finish(None, crate::transition::Completion::default())
        .updates,
        Some(300)
    );
    assert_eq!(
        PhaseMeter::start("idle", crate::scenarios::PhaseKind::Sustained, None, false, None, None)
            .finish(None, crate::transition::Completion::default())
            .updates,
        None
    );
}

/// A probe for `scenario`/`variant` whose App forces the counter gate on and holds one counting
/// pane, typing into that pane from row 0, column 0 under the grid's current identity.
fn typing_probe(scenario: &'static str, variant: &'static str) -> (Probe, u64) {
    use crate::record::prompt_identity;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window or pane yet");
    let pane = app.__test_seed_counting_tab("s2");
    let identity = app
        .main_panes()
        .and_then(|panes| panes.get(&pane))
        .map(|state| prompt_identity(state.parser.lock().grid()))
        .expect("seeded pane");
    let plan = scenarios::plan_for(scenario, variant, false, Host::Posix).expect("listed plan");
    let request = RunArgs {
        scenario,
        variant,
        managed: false,
        short: false,
        laps: false,
        counters: true,
        harness_hash: None,
        scratch: String::from("unused"),
        capture_delivery: false,
    };
    let now = Instant::now();
    let mut probe =
        Probe::new(app, plan, request, PathBuf::from("unused"), now + Duration::from_secs(3600));
    probe.driver = DriverState::Typing(Typing {
        role: 0,
        pane,
        chars: 3,
        interval: Duration::from_millis(100),
        settle: Duration::from_millis(100),
        origin: Some((0, 0)),
        identity,
        cols: 80,
        first_at: Some(now),
        typed: 0,
        done_at: None,
    });
    (probe, pane)
}

/// Settle the open sample from a presenting dispatch observed now, with snapshots read now.
fn credit_now(probe: &mut Probe) {
    let snapshot = probe.echo_snapshot();
    probe.attribute(
        &DispatchObservation { before: snapshot, after: probe.echo_snapshot(), advanced: true },
        Instant::now(),
    );
}

/// Closing a sample that no frame was credited with takes its armed watch, so the worker stops
/// paying for it: a later take of the same token finds it already taken.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn closing_an_unattributed_sample_disarms_its_watch() {
    use sonicterm_app::app::TakeOutcome;
    let (mut probe, pane) = typing_probe("S2", "default");
    assert_eq!(probe.open_typing_sample(Instant::now()), Some('a'));
    let ArmState::Armed(token) = probe.open_sample.as_ref().expect("open").arm else {
        panic!("precondition: S2/default arms its sample");
    };
    probe.close_sample(UnattributedReason::NoCandidate);
    assert_eq!(probe.app.take_echo_watch(pane, token), TakeOutcome::AlreadyTaken);
    assert_eq!(probe.samples.last().map(|sample| sample.split_reason), Some("not-credited"));
}

/// With the feature, only S2/default arms its samples; S2/flood's are out of scope and never armed.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn open_typing_sample_arms_only_for_s2_default() {
    let (mut default, _) = typing_probe("S2", "default");
    default.open_typing_sample(Instant::now());
    assert!(matches!(default.open_sample.as_ref().expect("open").arm, ArmState::Armed(_)));
    let (mut flood, _) = typing_probe("S2", "flood");
    flood.open_typing_sample(Instant::now());
    assert_eq!(flood.open_sample.as_ref().expect("open").arm, ArmState::OutOfScope);
}

/// S2/flood still credits its samples exactly as before, and every credited one reads
/// `unsupported` because it is outside the split's scope, in every build.
#[test]
fn s2_flood_attributes_as_before_and_reads_unsupported() {
    let (mut probe, pane) = typing_probe("S2", "flood");
    assert_eq!(probe.open_typing_sample(Instant::now()), Some('a'));
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe.app.__test_publish_pane_output(main, pane, b"a");
    credit_now(&mut probe);
    let sample = probe.samples.last().expect("a credited sample");
    assert_eq!(sample.reason, crate::record::CREDITED);
    assert!(sample.latency_ms.is_some());
    assert_eq!((sample.split, sample.split_reason), (None, "unsupported"));
}

/// A resize on the event loop after the echo appeared, which the worker never sees, changes the
/// grid's size generation; the harness's own snapshots catch it and the split is refused.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn a_harness_snapshot_identity_change_reads_row_identity_changed() {
    let (mut probe, pane) = typing_probe("S2", "default");
    probe.open_typing_sample(Instant::now());
    let main = sonicterm_app::app::synthetic_main_window_id();
    probe.app.__test_publish_pane_output(main, pane, b"a");
    let generation = |probe: &Probe| {
        let panes = probe.app.main_panes().expect("main");
        panes[&pane].parser.lock().grid().size_generation()
    };
    let before = generation(&probe);
    // 80 x 30 cells: more rows, the same columns, so row 0 keeps the echo.
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 600.0);
    assert!(probe.app.__test_set_main_pane_viewport(outer, 10.0, 20.0));
    probe.app.__test_resize_visible_panes();
    assert_ne!(generation(&probe), before, "precondition: the resize changed the grid's size");
    credit_now(&mut probe);
    let sample = probe.samples.last().expect("a credited sample");
    assert_eq!(sample.reason, crate::record::CREDITED, "the attribution itself is unchanged");
    assert_eq!(sample.split_reason, "row-identity-changed");
}

/// Without the feature the adapter reports `Unsupported` itself, so the run is an untrimmed
/// baseline; with it the App's real rules apply, and an uncovered window is skipped with no trim
/// number.
#[test]
fn the_trim_adapter_reports_what_this_build_can_do() {
    let (mut probe, _pane) = typing_probe("S12", "default");
    let main = sonicterm_app::app::synthetic_main_window_id();
    let expected = if cfg!(feature = "perf-hook-trim") {
        TrimHookOutcome::Skipped
    } else {
        TrimHookOutcome::Unsupported
    };
    assert_eq!(trim_covered(&mut probe.app, main), (expected, None));
}

/// A requested trim waits while the App is not covered, lapses as `not-reached` when the hold
/// ends first, and is called exactly once on the first turn the App holds `Occluded(true)`.
#[test]
fn a_requested_trim_is_called_once_after_the_cover_is_delivered() {
    let (mut probe, _pane) = typing_probe("S12", "default");
    probe.main_id = Some(sonicterm_app::app::synthetic_main_window_id());

    probe.trim_pending = true;
    probe.service_trim(false);
    assert!(probe.trim_pending, "no occlusion delivered yet, so the trim waits");
    probe.service_trim(true);
    assert_eq!((probe.trim_pending, probe.trim_hook), (false, TrimHookOutcome::NotReached));

    probe.trim_pending = true;
    probe.occlusion.delivered = Some(true);
    probe.service_trim(false);
    assert!(!probe.trim_pending, "the hook was called");
    assert_ne!(probe.trim_hook, TrimHookOutcome::NotReached);
    let called = probe.trim_hook;
    probe.service_trim(true);
    assert_eq!(probe.trim_hook, called, "a second turn calls nothing");
}

/// The cover act only opens the cover; the trim act asks for the trim, the turn handler services
/// it after any overdue occlusion is delivered, and the end of a phase lapses one still waiting.
/// The trim step never touches the plan's stage.
#[test]
fn the_trim_act_is_serviced_after_delivery_and_lapses_with_its_phase() {
    let perform = method("perform");
    assert!(perform.contains("Act::Cover => self.cover(event_loop),"), "{perform}");
    assert!(perform.contains("Act::TrimCovered => self.request_trim(event_loop),"));
    let turn = method("about_to_wait");
    let overdue = turn.find("self.deliver_overdue_occlusion(").expect("overdue delivery");
    let service = turn.find("self.service_trim(false);").expect("trim service");
    assert!(overdue < service, "the trim sees an occlusion delivered on the same turn");
    assert!(method("end_phase").contains("self.service_trim(true);"));
    assert!(!method("service_trim").contains("stage"), "the trim never changes the plan's steps");
}

/// Only a main-window redraw feeds a phase's presentation trace: a native dispatch that presents still
/// counts as a presented frame, but neither starts the trace nor counts as a nonpresenting redraw.
#[test]
fn only_main_window_redraws_feed_the_presentation_trace() {
    let mut meter =
        PhaseMeter::start("open", crate::scenarios::PhaseKind::Transition, None, false, None, None);
    let start = meter.started;
    let at = |offset_ms: u64| start + std::time::Duration::from_millis(offset_ms);
    meter.observe_dispatch(Dispatch::Native, at(1), at(2), (0, 1), None);
    meter.observe_dispatch(Dispatch::Harness, at(2), at(3), (1, 1), None);
    meter.observe_dispatch(Dispatch::Redraw, at(4), at(5), (1, 1), None);
    meter.observe_dispatch(Dispatch::Redraw, at(6), at(8), (1, 2), None);
    meter.observe_dispatch(Dispatch::Redraw, at(9), at(10), (2, 2), None);
    meter.observe_dispatch(Dispatch::Redraw, at(11), at(12), (2, 4), None);
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!((record.presented_frames, record.redraw_requested), (4, 4));
    assert_eq!((record.first_present_ms, record.last_present_ms), (Some(8.0), Some(12.0)));
    assert_eq!((record.first_present_seq, record.last_present_seq), (Some(2), Some(4)));
    assert_eq!((record.present_missing, record.nonpresenting_redraws), (None, 2));
}

/// A phase whose redraws never presented records `no-presentation` and every redraw as nonpresenting.
#[test]
fn a_phase_without_a_presenting_redraw_records_no_presentation() {
    let mut meter =
        PhaseMeter::start("hold", crate::scenarios::PhaseKind::Hold, None, false, None, None);
    let start = meter.started;
    meter.observe_dispatch(Dispatch::Redraw, start, start, (3, 3), None);
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!(record.present_missing, Some("no-presentation"));
    assert_eq!((record.first_present_ms, record.first_present_seq), (None, None));
    assert_eq!(record.nonpresenting_redraws, 1);
    let written = serde_json::to_value(&record).expect("a phase record serializes");
    assert!(written.get("first_present_ms").is_none(), "{written}");
    assert_eq!(written["present_missing"], "no-presentation");
}

/// `forward` feeds the meter from its own readings around the dispatch: the dispatch kind, its span
/// and the presented counts before and after it.
#[test]
fn forward_feeds_the_meter_from_its_dispatch_readings() {
    let forward = method("forward");
    // Compare without whitespace, so rustfmt's line wrapping does not decide the result.
    let compact: String = forward.chars().filter(|glyph| !glyph.is_whitespace()).collect();
    let adapter = compact.find("dispatch(&mutself.app,event_loop);").expect("forward dispatches");
    let fed = compact
        .find("meter.observe_dispatch(kind,started,ended,(frames_before,frames_after),allocations")
        .expect("forward feeds the meter");
    assert!(adapter < fed);
}

/// The real `forward` feeds its meter: a main-window redraw that presents nothing counts as a redraw and a
/// nonpresenting redraw, so the phase records `no-presentation`. The loop is generic, so a unit stands in for it.
#[test]
fn a_forwarded_redraw_reaches_the_phases_presentation_trace() {
    let act = Instant::now();
    let (mut probe, _phase) = release_probe("media-free", act, 7);
    probe.forward(&(), Dispatch::Redraw, |_app, _loop| {});
    let record = probe
        .meter
        .take()
        .expect("the phase is open")
        .finish(None, crate::transition::Completion::default());
    assert_eq!((record.redraw_requested, record.nonpresenting_redraws), (1, 1));
    assert_eq!(record.present_missing, Some("no-presentation"));
}

/// A transition that presents exactly one frame records that frame as both its first and last presentation.
#[test]
fn a_one_frame_transition_records_one_presentation_as_first_and_last() {
    let mut meter = PhaseMeter::start(
        "image",
        crate::scenarios::PhaseKind::Transition,
        Some(crate::scenarios::Endpoint::ImageRegisteredThenPresented),
        false,
        None,
        None,
    );
    let start = meter.started;
    let at = |offset_ms: u64| start + std::time::Duration::from_millis(offset_ms);
    meter.observe_dispatch(Dispatch::Redraw, at(30), at(40), (5, 6), None);
    let completion = crate::transition::Completion { elapsed_ms: Some(40.0), missing: None };
    let record = meter.finish(None, completion);
    assert_eq!((record.first_present_ms, record.last_present_ms), (Some(40.0), Some(40.0)));
    assert_eq!((record.first_present_seq, record.last_present_seq), (Some(6), Some(6)));
    assert_eq!((record.presented_frames, record.nonpresenting_redraws), (1, 0));
    assert_eq!(record.completion_ms, Some(40.0));
}

/// A sentinel transition completes on its parsed sentinel even when no redraw presented: completion and
/// `no-presentation` are recorded together, so completion never implies a presented frame.
#[test]
fn a_sentinel_completion_without_a_presentation_records_both() {
    let mut meter = PhaseMeter::start(
        "print",
        crate::scenarios::PhaseKind::Transition,
        Some(crate::scenarios::Endpoint::SentinelParsed),
        false,
        None,
        None,
    );
    let start = meter.started;
    meter.observe_dispatch(Dispatch::Native, start, start, (2, 2), None);
    let completion = crate::transition::Completion { elapsed_ms: Some(12.5), missing: None };
    let record = meter.finish(None, completion);
    assert_eq!((record.completion_ms, record.completion_missing), (Some(12.5), None));
    assert_eq!((record.present_missing, record.presented_frames), (Some("no-presentation"), 0));
}

/// A hold with no dispatch at all records no presentation and no nonpresenting redraw.
#[test]
fn a_no_draw_hold_records_no_presentation_and_no_redraws() {
    let meter =
        PhaseMeter::start("idle", crate::scenarios::PhaseKind::Hold, None, false, None, None);
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!(
        (record.present_missing, record.redraw_requested, record.nonpresenting_redraws),
        (Some("no-presentation"), 0, 0)
    );
    assert_eq!((record.completion_ms, record.completion_missing), (None, None));
}

/// The presentation trace is kept directly, not derived from the bounded slow-dispatch list: with more redraws
/// than SLOW_DISPATCH_LIMIT, the first and last presentations are still the phase's first and last.
#[test]
fn the_trace_outlives_the_slow_dispatch_limit() {
    let mut meter = PhaseMeter::start(
        "stream",
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        None,
        None,
    );
    let start = meter.started;
    let redraw_count = crate::record::SLOW_DISPATCH_LIMIT as u64 * 3;
    for index in 0..redraw_count {
        let ended = start + std::time::Duration::from_millis(index * 10 + 5);
        meter.observe_dispatch(Dispatch::Redraw, ended, ended, (index, index + 1), None);
    }
    let record = meter.finish(None, crate::transition::Completion::default());
    assert_eq!(record.slow_dispatches.len(), crate::record::SLOW_DISPATCH_LIMIT);
    assert_eq!(record.dispatch_count, redraw_count);
    let last_ms = ((redraw_count - 1) * 10 + 5) as f64;
    assert_eq!((record.first_present_ms, record.last_present_ms), (Some(5.0), Some(last_ms)));
    assert_eq!((record.first_present_seq, record.last_present_seq), (Some(1), Some(redraw_count)));
}
