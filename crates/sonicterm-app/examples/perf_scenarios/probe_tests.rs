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
    let meter = PhaseMeter::start("typing", false, Some(totals_with_attempts(published.get())));
    published.set(published.get() + 3);
    let record = meter.finish(Some(totals_with_attempts(published.get())));
    assert_eq!(credited(&record), 3);
}

#[test]
fn a_publication_in_the_gap_between_phases_is_credited_to_neither() {
    // Checkpoints and the progress write run between one phase's finish snapshot and the next
    // phase's start snapshot, so whatever they publish appears in no phase's delta.
    let published = Cell::new(0_u64);
    let first = PhaseMeter::start("first", false, Some(totals_with_attempts(published.get())));
    published.set(published.get() + 2);
    let first = first.finish(Some(totals_with_attempts(published.get())));
    published.set(published.get() + 5);
    let second = PhaseMeter::start("second", false, Some(totals_with_attempts(published.get())));
    published.set(published.get() + 1);
    let second = second.finish(Some(totals_with_attempts(published.get())));
    assert_eq!((credited(&first), credited(&second)), (2, 1));
    assert_eq!(published.get() - credited(&first) - credited(&second), 5, "the gap's work");
}

/// The body of `source`'s method `name`, up to the next method.
fn method_in(source: &str, name: &str) -> String {
    // A CRLF checkout is read as LF, so method ends match either way.
    let source = source.replace("\r\n", "\n");
    let start = source.find(&format!("    fn {name}(")).unwrap_or_else(|| panic!("{name}"));
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
    assert!(snapshot < finish.find("meter.finish(counters_end)").expect("finish"));
    let begin = method("begin_phase");
    let go_write = begin.find("go/{role}").expect("GO write");
    let snapshot =
        begin.find("let counters_start = self.counter_totals();").expect("start snapshot");
    let opened = begin.find("PhaseMeter::start(phase.name").expect("meter opens");
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
    probe.meter = Some(PhaseMeter::start(phase.name, false, None));
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

/// One checkpoint driven through `checkpoint_turn` on a fake clock: a private scratch directory
/// for its request and footprint files, the pending state and records the probe keeps, and the
/// number of times the fake sampler was called.
struct CheckpointRun {
    scratch: PathBuf,
    start: Instant,
    managed: bool,
    pending: Option<PendingCheckpoint>,
    records: Vec<CheckpointRecord>,
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
        CheckpointRun {
            scratch,
            start: Instant::now(),
            managed,
            pending: None,
            records: Vec::new(),
            sampler_calls: 0,
        }
    }

    fn at(&self, millis: u64) -> Instant {
        self.start + Duration::from_millis(millis)
    }

    /// One turn at checkpoint `ordinal` (`label`) at `millis`; `complete` answers each attempt
    /// from the turn's time in ms.
    fn turn_at(
        &mut self,
        ordinal: usize,
        label: &'static str,
        millis: u64,
        complete: impl Fn(u64) -> bool,
    ) -> CheckpointOutcome {
        let site = CheckpointSite {
            scratch: &self.scratch,
            managed: self.managed,
            sampling: true,
            footprint_wait: Duration::from_millis(1_000),
        };
        let step = CheckpointStep {
            ordinal,
            label,
            now: self.at(millis),
            unix_s: 1_000.0 + millis as f64 / 1_000.0,
            fresh_after_unix_s: None,
            frame_texture_bytes: None,
        };
        let calls = &mut self.sampler_calls;
        checkpoint_turn(&mut self.pending, &mut self.records, &site, &step, |_, _, _| {
            *calls += 1;
            complete(millis)
        })
    }

    fn turn(&mut self, millis: u64, complete: impl Fn(u64) -> bool) -> CheckpointOutcome {
        self.turn_at(0, "end", millis, complete)
    }

    /// The wake `next_deadline` uses while the checkpoint is pending.
    fn wake(&self, millis: u64) -> Option<Instant> {
        self.pending.as_ref().and_then(|pending| pending.wake(self.at(millis)))
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
    assert!(run.wake(10).is_some_and(|wake| wake <= run.at(50)), "wakes for the retry");
    assert_eq!(run.turn(50, partial_until_50), CheckpointOutcome::Advance);
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
        assert!(run.wake(millis).is_some_and(|wake| wake <= run.at(millis + 50)));
    }
    assert_eq!(run.turn(100, complete_from_100), CheckpointOutcome::Advance);
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
    }
    assert_eq!(run.turn(450, |_| false), CheckpointOutcome::Advance);
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
        assert_eq!(run.wake(millis), Some(run.at(millis + 50)), "polls every 50 ms");
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
        assert_eq!(run.wake(480), Some(run.at(500)));
        assert_eq!(run.turn(last_turn, |_| true), CheckpointOutcome::Advance, "at {last_turn} ms");
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
    let site = CheckpointSite {
        scratch: &scratch.scratch,
        managed: false,
        sampling: false,
        footprint_wait: Duration::from_secs(1),
    };
    let step = CheckpointStep {
        ordinal: 0,
        label: "end",
        now: scratch.start,
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
