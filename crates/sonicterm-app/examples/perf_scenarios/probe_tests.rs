//! Pins the phase boundaries: each phase's counter delta covers exactly its own span, and work
//! between phases (the gap, checkpoints, progress writes) is credited to no phase.

pub(super) use std::cell::Cell;

use super::*;
pub(super) use crate::counters::{FieldValue, Section, SourceValue};

/// Totals whose only supported field is `attempts`, standing in for an App snapshot.
pub(super) fn totals_with_attempts(attempts: u64) -> CounterTotals {
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
pub(super) fn credited(record: &PhaseRecord) -> u64 {
    let counters = record.frame_counters.as_ref().expect("a counting phase");
    match counters.get("attempts") {
        Some(FieldValue::Count(attempts)) => *attempts,
        other => panic!("attempts reads {other:?}"),
    }
}

/// The body of `source`'s method `name`, up to the next method.
pub(super) fn method_in(source: &str, name: &str) -> String {
    // A CRLF checkout is read as LF, so method ends match either way.
    let source = source.replace("\r\n", "\n");
    // A generic method's name is followed by its parameters' `<`, an ordinary one's by `(`.
    let start = ["(", "<"]
        .iter()
        .find_map(|opener| source.find(&format!("fn {name}{opener}")))
        .unwrap_or_else(|| panic!("{name}"));
    let rest = &source[start..];
    rest[..rest.find("\n    }\n").expect("method closing brace") + "\n    }".len()].to_owned()
}

/// The body of probe.rs's method `name`, up to the next method.
pub(super) fn method(name: &str) -> String {
    method_in(
        concat!(
            include_str!("probe.rs"),
            "\n",
            include_str!("probe_api.rs"),
            "\n",
            include_str!("probe_atlas.rs"),
            "\n",
            include_str!("probe_bootstrap.rs"),
            "\n",
            include_str!("probe_callbacks.rs"),
            "\n",
            include_str!("probe_checkpoint.rs"),
            "\n",
            include_str!("probe_input.rs"),
            "\n",
            include_str!("probe_occlusion.rs"),
            "\n",
            include_str!("probe_phases.rs"),
            "\n",
            include_str!("probe_sampling.rs"),
            "\n",
            include_str!("probe_scan.rs")
        ),
        name,
    )
}

/// A headless probe running S11/release's phase `name`, its meter open and its barrier started at `act`,
/// with the renderer readings stubbed at `frames` presented and an empty image atlas.
pub(super) fn release_probe(name: &str, act: Instant, frames: u64) -> (Probe, PhaseSpec) {
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
pub(super) fn promoted_atlas(items: usize) -> ResourceAmount {
    ResourceAmount { bytes: 2048 * 2048 * 4, items }
}

/// The run deadline the checkpoint fixture's wakes are bounded by; far past every checkpoint wake.
pub(super) const FIXTURE_RUN_DEADLINE: Duration = Duration::from_secs(60);

/// One checkpoint driven through the probe's production checkpoint path on a fake clock: a private
/// scratch directory for its request and footprint files, the pending state and records the probe
/// keeps, the plan step the probe would be on, and the number of times the sampler was called.
///
/// Each turn runs `checkpoint_turn` with an injected clock and file check, then moves the plan step
/// on exactly when `plan_moves_on` says so, as `Probe::advance_checkpoint` and the step loop do.
/// Its wake is `steps_wake`, the function `Probe::next_deadline` uses while steps run.
pub(super) struct CheckpointRun {
    pub(super) scratch: PathBuf,
    pub(super) start: Instant,
    managed: bool,
    sampling: bool,
    /// The fake clock every decision reads; shared, so a sampler reads the time a decision saw.
    clock: std::rc::Rc<Cell<Instant>>,
    /// When set, the next `.done` existence check moves the clock here, as slow I/O would.
    pub(super) jump_on_done: Cell<Option<Instant>>,
    pub(super) pending: Option<PendingCheckpoint>,
    pub(super) records: Vec<CheckpointRecord>,
    /// The plan step the probe is on: 0 until the checkpoint moves the plan on.
    pub(super) plan_step: usize,
    pub(super) sampler_calls: u32,
}

impl CheckpointRun {
    pub(super) fn new(name: &str, managed: bool) -> Self {
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

    pub(super) fn at(&self, millis: u64) -> Instant {
        self.start + Duration::from_millis(millis)
    }

    /// The fake clock's time, in ms from the start.
    pub(super) fn now_ms(&self) -> u64 {
        self.clock.get().saturating_duration_since(self.start).as_millis() as u64
    }

    /// One turn at checkpoint `ordinal` (`label`) starting at `millis`, with `sample` answering
    /// each attempt (index, label, attempt) with whether it measured every pane.
    pub(super) fn turn_with(
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
            completeness: None,
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
    pub(super) fn turn_at(
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

    pub(super) fn turn(
        &mut self,
        millis: u64,
        complete: impl Fn(u64) -> bool,
    ) -> CheckpointOutcome {
        self.turn_at(0, "end", millis, complete)
    }

    /// The probe's next wake at `millis` while it runs its steps, with no phase wake of its own.
    pub(super) fn wake(&self, millis: u64) -> Instant {
        steps_wake(self.pending.as_ref(), self.at(millis), [], self.start + FIXTURE_RUN_DEADLINE)
    }

    /// Answer the footprint request as the comparison script does: the JSON, then `.done`.
    pub(super) fn answer_footprint(&self, stem: &str) {
        std::fs::write(self.scratch.join(format!("checkpoints/{stem}.json")), b"{}").expect("json");
        std::fs::write(self.scratch.join(format!("checkpoints/{stem}.done")), b"").expect("done");
    }

    /// Request files written so far.
    pub(super) fn requests(&self) -> usize {
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
    pub(super) fn request_text(&self, stem: &str) -> String {
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

/// The golden runs `scripts/perf-compare_tests.py` reads: each case's `result.json` from the
/// production serializer and the `memory` log lines the App's hook emitted, under the build's key.
pub(super) const CHECKPOINT_FIXTURE: &str = "../../scripts/perf-compare_checkpoint_fixture.json";

/// A `tracing` writer that keeps everything written to it, so a test can read the log lines the
/// production format layer produced.
#[derive(Clone, Default)]
pub(super) struct LogSink(Arc<parking_lot::Mutex<Vec<u8>>>);

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
pub(super) struct FixtureCase {
    pub(super) name: &'static str,
    managed: bool,
    turns: &'static [(u64, bool)],
    /// When set, the `.done` check of the turn at this ms moves the clock to the second time.
    jump: Option<(u64, u64)>,
    /// Answer the footprint before the turn at this ms.
    answer_before: Option<u64>,
    pub(super) attempts: Option<u32>,
}

/// The cases a build with the hook writes: late exhaustion by count of turns, by a stale clock, by a
/// single turn at 600 ms, and at or after the deadline after five partial attempts.
#[cfg(feature = "perf-hook-checkpoint-memory")]
pub(super) const FIXTURE_CASES: &[FixtureCase] = &[
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
pub(super) const FIXTURE_CASES: &[FixtureCase] = &[FixtureCase {
    name: "unsupported",
    managed: false,
    turns: &[(0, true)],
    jump: None,
    answer_before: None,
    attempts: None,
}];

/// `result.json` for one fixture run: a valid S1 run with one phase and the probe's checkpoints.
pub(super) fn fixture_result(checkpoints: Vec<CheckpointRecord>) -> RunResult {
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
            dispatch_presenting_ms: Vec::new(),
            dispatch_nonpresenting_ms: Vec::new(),
            go_to_first_output_ms: None,
            first_output_to_present_ms: None,
            slow_dispatches: Vec::new(),
            dispatch_count: 2,
            present_interval_ms: vec![16.6],
            allocations_per_frame: None,
            frame_counters: None,
            updates: None,
            s10_attribution: None,
            guard_correlation: None,
            guard_correlation_transport_ns: None,
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
pub(super) fn run_fixture_case(case: &FixtureCase) -> (serde_json::Value, Vec<String>) {
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
pub(super) fn memory_marker() -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/perf_memory.py");
    let source = std::fs::read_to_string(&script).expect("perf_memory.py").replace("\r\n", "\n");
    let line = source
        .lines()
        .find(|line| line.starts_with("MEMORY_MARKER = \""))
        .expect("perf_memory.py defines MEMORY_MARKER");
    line.trim_start_matches("MEMORY_MARKER = \"").trim_end_matches('"').to_owned()
}

/// The shape of a leading log stamp: `utc` for the fmt layer's `YYYY-MM-DDTHH:MM:SS[.frac]Z`, which
/// `parse_utc_stamp` reads, else `other`.
pub(super) fn stamp_shape(line: &str) -> &'static str {
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
pub(super) fn log_projection(line: &str, marker: &str) -> Vec<String> {
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

/// A probe for `scenario`/`variant` whose App forces the counter gate on and holds one counting
/// pane, typing into that pane from row 0, column 0 under the grid's current identity.
pub(super) fn typing_probe(scenario: &'static str, variant: &'static str) -> (Probe, u64) {
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
pub(super) fn credit_now(probe: &mut Probe) {
    let started = Instant::now();
    let snapshot = probe.echo_snapshot();
    probe.attribute(
        &DispatchObservation { before: snapshot, after: probe.echo_snapshot(), advanced: true },
        started,
        Instant::now(),
    );
}

/// A settling scene titled `title`: what the driver's tests feed, with no fixture rows, so only the
/// barrier and the counts decide what each dispatch does.
pub(super) fn retry_scene(title: &str) -> Scene {
    Scene {
        title: title.to_owned(),
        fallback: (1, 0, 0),
        grid: (80, 24),
        cursor: (23, 0),
        rows: Vec::new(),
    }
}

/// A reading of `retry_scene(title)` on both sides, with `cached` applied on each.
pub(super) fn retry_reading(
    title: &str,
    cached: Option<atlas_retry::AppliedSample>,
) -> SceneReading {
    SceneReading {
        before: Some(retry_scene(title)),
        after: Some(retry_scene(title)),
        cached_before: cached.clone(),
        cached_after: cached,
    }
}

/// An applied sample taken `offset_ms` after `start`, naming `process`.
pub(super) fn applied(
    start: Instant,
    offset_ms: u64,
    process: Option<&str>,
) -> atlas_retry::AppliedSample {
    atlas_retry::AppliedSample {
        sampled_at: start + Duration::from_millis(offset_ms),
        process: process.map(str::to_owned),
    }
}

/// A driver settling from `start`, its final process `sleep` observed at `start + 100 ms`, expecting
/// the F489 title: the barrier the driver tests run against.
pub(super) fn barred_driver(start: Instant) -> AtlasRetryDriver {
    let mut driver =
        AtlasRetryDriver::new(start, atlas_retry::FinalLookup::new("sleep".to_owned()));
    driver.machine.set_barrier(atlas_retry::Barrier {
        final_at: Some(start + Duration::from_millis(100)),
        final_name: "sleep".to_owned(),
        expected_title: Some("#1 \u{F489} shell".to_owned()),
        fixture_problem: None,
    });
    driver
}

/// The counts of a forwarded `RequestRedraw`, which attempts no frame.
pub(super) fn no_attempt() -> Counts {
    Counts::default()
}

/// The counts of one steady settling frame.
pub(super) fn one_frame() -> Counts {
    Counts { attempts: 1, presented: 1, ..Counts::default() }
}

/// A headless probe whose finalization clock steps one second a read from `act`, whose transport is recorded,
/// and whose scratch is a fresh directory; the recorded calls are shared with the test.
pub(super) fn guard_probe(
    name: &str,
    act: Instant,
) -> (Probe, PhaseSpec, std::rc::Rc<std::cell::RefCell<Vec<GuardTransportCall>>>, ScratchDir) {
    let (mut probe, phase) = release_probe(name, act, 0);
    let scratch = ScratchDir::new(name);
    probe.scratch = scratch.path.clone();
    let mut reads = 0_u32;
    probe.guard_clock_override = Some(Box::new(move || {
        reads += 1;
        act + Duration::from_secs(u64::from(reads))
    }));
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = std::rc::Rc::clone(&calls);
    probe.guard_transport_override = Some(Box::new(move |call| recorded.borrow_mut().push(call)));
    (probe, phase, calls, scratch)
}

/// A test's own scratch directory: unique to this process and this fixture, whatever the label, and removed
/// when dropped, an unwinding test included.
pub(super) struct ScratchDir {
    pub(super) path: PathBuf,
}

impl ScratchDir {
    /// A fresh directory named for `label`, this process and a per-process counter.
    pub(super) fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // Ordering: Relaxed suffices; NEXT only hands out distinct numbers and orders no other memory.
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("sonicterm-guard-probe-{label}-{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

// Lifecycle: dropping ScratchDir calls remove_dir_all on its path; a directory already gone is ignored.
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A drag lagging its schedule, pressed mid-cycle, with its phase ending at `deadline`.
pub(super) fn lagging_drag(deadline: Instant, quiet_from: Option<Instant>) -> Drag {
    Drag {
        interval: Duration::from_millis(8),
        started: deadline - Duration::from_secs(2),
        ticks: 30,
        cycle_ticks: 72,
        move_ticks: 60,
        start: (0.0, 0.0),
        end: (100.0, 100.0),
        pressed: true,
        deadline,
        quiet_from,
        finished: false,
    }
}

/// A meter fed `presenting` 1 ms presenting redraws, then non-presenting 0.1 ms ones up to 100.
pub(super) fn meter_with_mix(presenting: usize) -> PhaseRecord {
    let mut meter = PhaseMeter::start(
        "typing",
        crate::scenarios::PhaseKind::Sustained,
        None,
        false,
        None,
        None,
    );
    let started = meter.started;
    let mut frames = 0_u64;
    for index in 0..100 {
        let shows = index < presenting;
        let micros = if shows { 1_000 } else { 100 };
        let ended = started + Duration::from_micros(micros);
        let after = frames + u64::from(shows);
        meter.observe_dispatch(Dispatch::Redraw, started, ended, (frames, after), None);
        frames = after;
    }
    meter.finish(None, crate::transition::Completion::default())
}

/// The middle value of `values`, the upper one for an even count.
pub(super) fn median_of(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

/// A print meter, as `begin_phase` opens one for the phase that releases the roles, placed on `probe`.
pub(super) fn open_print_meter(probe: &mut Probe) {
    probe.meter = Some(PhaseMeter::start(
        "print",
        crate::scenarios::PhaseKind::Transition,
        None,
        false,
        None,
        None,
    ));
    if let Some(meter) = probe.meter.as_mut() {
        meter.present.await_output(0.0);
    }
}

/// One presenting main-window redraw, observed now, advancing the presented count by one.
pub(super) fn present_one_frame(probe: &mut Probe, presented_after: u64) {
    let meter = probe.meter.as_mut().expect("an open meter");
    let started = Instant::now();
    meter.observe_dispatch(
        Dispatch::Redraw,
        started,
        started + Duration::from_millis(1),
        (presented_after - 1, presented_after),
        None,
    );
}
