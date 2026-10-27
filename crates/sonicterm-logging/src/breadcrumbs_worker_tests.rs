//! Worker scheduling, bounded history, and snapshot replacement contracts.

use super::super::test_support::*;
use super::*;
use crate::process_memory::MemoryMetric;

/// Deterministic sampler that exposes each delivered value to the test thread.
struct SequenceSampler {
    next: AtomicU64,
    ticks: std::sync::mpsc::Sender<u64>,
}

impl PressureSampler for SequenceSampler {
    /// Increment both the pressure fixture and an observation channel unless cancelled.
    fn sample(&self, cancellation: &SamplerCancellation) -> Option<ProcessPressure> {
        if cancellation.is_cancelled() {
            return None;
        }
        let value = self.next.fetch_add(1, Ordering::Relaxed);
        let _ = self.ticks.send(value);
        Some(ProcessPressure {
            private_committed: MemoryMetric::Bytes(value),
            resident: MemoryMetric::Bytes(value.saturating_add(1_000)),
        })
    }
}

/// Sampler that holds the worker until its out-of-band cancellation signal arrives.
struct BlockingSampler {
    entered: std::sync::mpsc::SyncSender<()>,
}

impl PressureSampler for BlockingSampler {
    /// Announce entry, wait for cancellation, and suppress a post-cancel sample.
    fn sample(&self, cancellation: &SamplerCancellation) -> Option<ProcessPressure> {
        let _ = self.entered.send(());
        cancellation.wait_until_cancelled();
        None
    }
}

/// Startup persists a pressure sample before the long periodic deadline.
///
/// A one-hour interval and observed sequence value prove the worker sampled immediately.
#[test]
fn immediate_pressure_sample_is_persisted() {
    let dir = scratch("immediate-pressure");
    let (ticks_tx, ticks_rx) = std::sync::mpsc::channel();
    let sampler = SequenceSampler { next: AtomicU64::new(41), ticks: ticks_tx };
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "immediate-pressure",
        BreadcrumbLimits { pressure_interval: Duration::from_secs(3600), ..limits() },
        sampler,
    )
    .expect("start");
    assert_eq!(ticks_rx.recv_timeout(Duration::from_secs(5)).expect("immediate tick"), 41);
    let recorder = writer.recorder();
    assert_eq!(
        recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started)),
        RecordOutcome::Queued
    );
    drop(recorder);
    writer.shutdown().expect("shutdown");

    let written =
        std::fs::read_to_string(breadcrumb_path(&dir, "immediate-pressure").expect("path"))
            .expect("read");
    assert!(
        written.contains("event=resource_history private_committed=41 resident=1041"),
        "{written}"
    );
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// Ordinary receive deadlines repeatedly invoke the pressure sampler.
///
/// The sampler's channel supplies deterministic ticks, avoiding timing assertions or test sleeps.
#[test]
fn pressure_sampler_runs_on_ordinary_deadlines_without_sleeps() {
    let dir = scratch("deadline-pressure");
    let (ticks_tx, ticks_rx) = std::sync::mpsc::channel();
    let sampler = SequenceSampler { next: AtomicU64::new(1), ticks: ticks_tx };
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "deadline-pressure",
        BreadcrumbLimits {
            pressure_interval: Duration::from_millis(1),
            history_capacity: 4,
            ..limits()
        },
        sampler,
    )
    .expect("start");
    let observed: Vec<_> = (0..3)
        .map(|_| ticks_rx.recv_timeout(Duration::from_secs(5)).expect("pressure tick"))
        .collect();
    assert_eq!(observed, vec![1, 2, 3]);
    writer.shutdown().expect("shutdown");
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// Shutdown wakes a blocked sampler and persists no result after cancellation.
///
/// The blocking fixture announces entry, then only the cancellation condition can release it.
#[test]
fn shutdown_cancels_a_blocked_sampler_and_records_no_cancelled_sample() {
    let dir = scratch("cancel-pressure");
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "cancel-pressure",
        limits(),
        BlockingSampler { entered: entered_tx },
    )
    .expect("start");
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("sampler entry");

    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = done_tx.send(writer.shutdown());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("cancellation must release sampler")
        .expect("shutdown");

    let written = std::fs::read_to_string(breadcrumb_path(&dir, "cancel-pressure").expect("path"))
        .expect("read");
    assert!(
        !written.contains("event=resource_history"),
        "cancelled sample was recorded: {written}"
    );
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// A full event queue cannot prevent shutdown of a blocked sampler.
///
/// Capacity one fills the channel before shutdown; the out-of-band cancellation path must still
/// release and join the worker.
#[test]
fn full_queue_during_blocked_sample_does_not_strand_shutdown() {
    let dir = scratch("full-queue-cancel");
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "full-queue-cancel",
        BreadcrumbLimits { queue_capacity: 1, ..limits() },
        BlockingSampler { entered: entered_tx },
    )
    .expect("start");
    let recorder = writer.recorder();
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("sampler entry");
    assert_eq!(
        recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started)),
        RecordOutcome::Queued
    );

    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = done_tx.send(writer.shutdown());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("full queue must not strand shutdown")
        .expect("shutdown");
    assert_eq!(
        recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready)),
        RecordOutcome::WorkerStopped
    );
    drop(recorder);
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// Dropping the writer stops its worker even while another sender remains alive.
///
/// A retained recorder prevents channel disconnect, so only explicit cancellation and join can
/// complete the drop thread.
#[test]
fn dropping_writer_exits_even_while_a_recorder_clone_is_alive() {
    let dir = scratch("drop-pressure");
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "drop-pressure",
        limits(),
        BlockingSampler { entered: entered_tx },
    )
    .expect("start");
    let recorder = writer.recorder();
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("sampler entry");

    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        drop(writer);
        let _ = done_tx.send(());
    });
    done_rx.recv_timeout(Duration::from_secs(5)).expect("writer drop must join worker");
    assert_eq!(
        recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready)),
        RecordOutcome::WorkerStopped
    );
    drop(recorder);
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// A long run preserves mandatory records while bounding history to its newest suffix.
///
/// Monotonic sample values expose gaps, stale retention, and accidental eviction of pinned or
/// lifecycle records.
#[test]
fn long_sampling_run_keeps_mandatory_records_and_only_newest_history() {
    let dir = scratch("long-pressure");
    let (ticks_tx, ticks_rx) = std::sync::mpsc::channel();
    let sampler = SequenceSampler { next: AtomicU64::new(1), ticks: ticks_tx };
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "long-pressure",
        BreadcrumbLimits {
            queue_capacity: 64,
            pressure_interval: Duration::from_millis(1),
            history_capacity: 4,
            ..limits()
        },
        sampler,
    )
    .expect("start");
    let recorder = writer.recorder();
    for event in [
        BreadcrumbEvent::Version(version()),
        BreadcrumbEvent::Platform(Platform::current()),
        BreadcrumbEvent::Renderer {
            identity: RendererIdentity::Wgpu,
            mode: RendererMode::Gpu,
            adapter: AdapterClass::Hardware,
        },
        BreadcrumbEvent::Counts { windows: 1, panes: 2 },
        BreadcrumbEvent::ResourceSnapshot(ProcessMemory {
            private_committed: MemoryMetric::Bytes(100),
            resident: MemoryMetric::Bytes(200),
            virtual_bytes: MemoryMetric::Bytes(300),
        }),
        BreadcrumbEvent::RetentionSnapshot {
            session_bytes: 400,
            renderer_bytes: 500,
            row_glyph_cache_bytes: 600,
            row_glyph_cache_items: 6,
            row_quad_cache_bytes: 700,
            row_quad_cache_items: 7,
            live_renderers: 2,
            allocator: None,
        },
        BreadcrumbEvent::Lifecycle(LifecycleEvent::Started),
        BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready),
    ] {
        assert_eq!(recorder.record(event), RecordOutcome::Queued);
    }
    let mut observed = Vec::new();
    for _ in 0..10 {
        observed.push(ticks_rx.recv_timeout(Duration::from_secs(5)).expect("pressure tick"));
    }
    drop(recorder);
    writer.shutdown().expect("shutdown");
    observed.extend(ticks_rx.try_iter());

    let written = std::fs::read_to_string(breadcrumb_path(&dir, "long-pressure").expect("path"))
        .expect("read");
    for mandatory in [
        "event=version",
        "event=platform",
        "event=renderer",
        "event=counts",
        "event=resource",
        "event=retention",
        "lifecycle=started",
        "lifecycle=ready",
    ] {
        assert!(written.contains(mandatory), "missing {mandatory}: {written}");
    }
    let history: Vec<_> =
        written.lines().filter(|line| line.contains("event=resource_history")).collect();
    assert_eq!(history.len(), 4, "{written}");
    let retained: Vec<u64> = history
        .iter()
        .map(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("private_committed="))
                .expect("pressure value")
                .parse()
                .expect("numeric pressure value")
        })
        .collect();
    assert!(retained.windows(2).all(|pair| pair[1] == pair[0] + 1), "{retained:?}");
    let newest = *retained.last().expect("newest retained pressure");
    assert!(observed.contains(&newest), "retained sample was never observed: {newest}");
    assert_eq!(retained, ((newest - 3)..=newest).collect::<Vec<_>>());
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// Continuous caller traffic cannot starve an elapsed pressure deadline.
///
/// A producer floods the bounded queue while the sequence channel must still observe a later tick.
#[test]
fn continuous_events_do_not_starve_pressure_deadlines() {
    let dir = scratch("deadline-priority");
    let (ticks_tx, ticks_rx) = std::sync::mpsc::channel();
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "deadline-priority",
        BreadcrumbLimits {
            queue_capacity: 256,
            pressure_interval: Duration::from_millis(1),
            ..limits()
        },
        SequenceSampler { next: AtomicU64::new(1), ticks: ticks_tx },
    )
    .expect("start");
    assert_eq!(ticks_rx.recv_timeout(Duration::from_secs(5)).expect("immediate tick"), 1);
    let recorder = writer.recorder();
    let producer = std::thread::spawn(move || {
        for panes in 1..=10_000 {
            let _ = recorder.record(BreadcrumbEvent::Counts { windows: 1, panes });
        }
    });
    let tick = ticks_rx.recv_timeout(Duration::from_secs(5)).expect("deadline tick under load");
    assert!(tick >= 2);
    producer.join().expect("producer");
    writer.shutdown().expect("shutdown");
    let written =
        std::fs::read_to_string(breadcrumb_path(&dir, "deadline-priority").expect("path"))
            .expect("read");
    assert!(written.contains("event=resource_history"));
    assert!(written.contains("event=counts"));
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// The computed file cap fits every configured lifecycle slot plus pressure history.
///
/// Filling all 64 slots exercises the serialized worst-case structure under the derived budget.
#[test]
fn large_lifecycle_capacity_stays_within_dynamic_file_cap() {
    let dir = scratch("large-lifecycle");
    let ring_capacity = 64;
    let max_file_bytes =
        required_file_bytes(ring_capacity).expect("required bytes").max(MIN_FILE_BYTES);
    let (ticks_tx, ticks_rx) = std::sync::mpsc::channel();
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "large-lifecycle",
        BreadcrumbLimits {
            queue_capacity: 256,
            ring_capacity,
            max_file_bytes,
            pressure_interval: Duration::from_secs(3600),
            history_capacity: 4,
        },
        SequenceSampler { next: AtomicU64::new(1), ticks: ticks_tx },
    )
    .expect("start");
    ticks_rx.recv_timeout(Duration::from_secs(5)).expect("immediate history");
    let recorder = writer.recorder();
    for _ in 0..ring_capacity {
        assert_eq!(
            recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready)),
            RecordOutcome::Queued
        );
    }
    drop(recorder);
    writer.shutdown().expect("shutdown");
    let path = breadcrumb_path(&dir, "large-lifecycle").expect("path");
    let written = std::fs::read_to_string(&path).expect("read");
    assert_eq!(written.lines().filter(|line| line.contains("event=lifecycle")).count(), 64);
    assert!(written.contains("event=resource_history"));
    assert!(std::fs::metadata(path).expect("metadata").len() <= max_file_bytes);
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// Failed replacement leaves the previous complete file and removes its candidate.
///
/// An injected permission error exercises the cleanup branch without depending on platform IO.
#[test]
fn failed_atomic_replace_preserves_prior_file_and_removes_temp() {
    let dir = scratch("atomic-failure");
    let path = dir.join("breadcrumbs.log");
    std::fs::write(&path, "prior-valid\n").expect("seed prior file");
    let mut state = WorkerState::new(3, 1);
    state.capture(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started));

    let error = persist_state_with_replace(&path, &state, MIN_FILE_BYTES, |_, _| {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected replace failure"))
    })
    .expect_err("replace must fail");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read_to_string(&path).expect("read prior"), "prior-valid\n");
    assert!(!path.with_extension("tmp").exists(), "temporary file survived failure");
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}

/// Worker serialization orders pinned state, lifecycle transitions, then newest history.
///
/// Out-of-order insertion and an over-capacity history ring expose ordering and eviction mistakes.
#[test]
fn worker_state_serializes_pinned_lifecycle_then_newest_history() {
    let mut state = WorkerState::new(3, 2);
    state.capture(BreadcrumbEvent::Counts { windows: 1, panes: 1 });
    state.capture(BreadcrumbEvent::Version(version()));
    state.capture(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started));
    state.capture(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready));
    state.capture_pressure(ProcessPressure {
        private_committed: MemoryMetric::Bytes(10),
        resident: MemoryMetric::Bytes(20),
    });
    state.capture_pressure(ProcessPressure {
        private_committed: MemoryMetric::Bytes(30),
        resident: MemoryMetric::Bytes(40),
    });
    state.capture_pressure(ProcessPressure {
        private_committed: MemoryMetric::Bytes(50),
        resident: MemoryMetric::Bytes(60),
    });

    assert_eq!(state.history.len(), 2);
    let lines = state.render_for_limit(MIN_FILE_BYTES);
    let text = lines.join("\n");
    let version_pos = text.find("event=version").expect("version");
    let counts_pos = text.find("event=counts").expect("counts");
    let started_pos = text.find("lifecycle=started").expect("started");
    let ready_pos = text.find("lifecycle=ready").expect("ready");
    let first_history_pos = text.find("private_committed=30").expect("newer history");
    let newest_history_pos = text.find("private_committed=50").expect("newest history");
    assert!(version_pos < counts_pos);
    assert!(counts_pos < started_pos);
    assert!(started_pos < ready_pos);
    assert!(ready_pos < first_history_pos);
    assert!(first_history_pos < newest_history_pos);
    assert!(!text.contains("private_committed=10"));
}

/// The tight valid byte cap preserves mandatory records and at least one history sample.
///
/// Starting a real worker at the derived bound verifies validation and serialization agree.
#[test]
fn valid_binding_cap_preserves_mandatory_and_newest_history() {
    let dir = scratch("binding-cap");
    let ring_capacity = MIN_LIFECYCLE_CAPACITY;
    let max_file_bytes =
        required_file_bytes(ring_capacity).expect("required bytes").max(MIN_FILE_BYTES);
    let limits = BreadcrumbLimits {
        queue_capacity: 64,
        ring_capacity,
        max_file_bytes,
        pressure_interval: Duration::from_secs(3600),
        history_capacity: 2,
    };
    let (ticks_tx, ticks_rx) = std::sync::mpsc::channel();
    let writer = BreadcrumbWriter::start_with_sampler(
        &dir,
        "binding-cap",
        limits,
        SequenceSampler { next: AtomicU64::new(1), ticks: ticks_tx },
    )
    .expect("start");
    ticks_rx.recv_timeout(Duration::from_secs(5)).expect("immediate history");
    let recorder = writer.recorder();
    for event in [
        BreadcrumbEvent::Version(version()),
        BreadcrumbEvent::Platform(Platform::current()),
        BreadcrumbEvent::Counts { windows: 1, panes: 2 },
        BreadcrumbEvent::Lifecycle(LifecycleEvent::Started),
        BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready),
        BreadcrumbEvent::Lifecycle(LifecycleEvent::CleanShutdown),
    ] {
        assert_eq!(recorder.record(event), RecordOutcome::Queued);
    }
    drop(recorder);
    writer.shutdown().expect("shutdown");

    let path = breadcrumb_path(&dir, "binding-cap").expect("path");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("event=version"));
    assert!(text.contains("event=platform"));
    assert!(text.contains("event=counts"));
    assert!(text.contains("lifecycle=started"));
    assert!(text.contains("lifecycle=ready"));
    assert!(text.contains("lifecycle=clean_shutdown"));
    assert!(text.contains("event=resource_history"));
    assert!(!text
        .lines()
        .any(|line| line.contains("event=resource_history") && line.contains("virtual=")));
    assert!(std::fs::metadata(path).expect("metadata").len() <= max_file_bytes);
    std::fs::remove_dir_all(dir).expect("remove scratch directory");
}
