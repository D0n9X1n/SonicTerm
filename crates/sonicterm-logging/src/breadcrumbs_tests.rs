use super::*;

use crate::process_memory::{MemoryMetric, ProcessPressure};
use std::fs;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("sonicterm-breadcrumbs-{label}-{}-{unique}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn limits() -> BreadcrumbLimits {
    let ring_capacity = 8;
    let required = required_file_bytes(ring_capacity).expect("valid test limit");
    BreadcrumbLimits {
        queue_capacity: 8,
        ring_capacity,
        max_file_bytes: required.max(MIN_FILE_BYTES),
        pressure_interval: Duration::from_secs(5),
        history_capacity: 48,
    }
}

fn version() -> AppVersion {
    "1.2.3".parse().expect("valid version")
}

struct SequenceSampler {
    next: AtomicU64,
    ticks: std::sync::mpsc::Sender<u64>,
}

impl PressureSampler for SequenceSampler {
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

struct BlockingSampler {
    entered: std::sync::mpsc::SyncSender<()>,
}

impl PressureSampler for BlockingSampler {
    fn sample(&self, cancellation: &SamplerCancellation) -> Option<ProcessPressure> {
        let _ = self.entered.send(());
        cancellation.wait_until_cancelled();
        None
    }
}

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

    let written = fs::read_to_string(breadcrumb_path(&dir, "immediate-pressure").expect("path"))
        .expect("read");
    assert!(
        written.contains("event=resource_history private_committed=41 resident=1041"),
        "{written}"
    );
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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

    let written =
        fs::read_to_string(breadcrumb_path(&dir, "cancel-pressure").expect("path")).expect("read");
    assert!(
        !written.contains("event=resource_history"),
        "cancelled sample was recorded: {written}"
    );
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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

    let written =
        fs::read_to_string(breadcrumb_path(&dir, "long-pressure").expect("path")).expect("read");
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
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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
    let written = fs::read_to_string(breadcrumb_path(&dir, "deadline-priority").expect("path"))
        .expect("read");
    assert!(written.contains("event=resource_history"));
    assert!(written.contains("event=counts"));
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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
    let written = fs::read_to_string(&path).expect("read");
    assert_eq!(written.lines().filter(|line| line.contains("event=lifecycle")).count(), 64);
    assert!(written.contains("event=resource_history"));
    assert!(fs::metadata(path).expect("metadata").len() <= max_file_bytes);
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn failed_atomic_replace_preserves_prior_file_and_removes_temp() {
    let dir = scratch("atomic-failure");
    let path = dir.join("breadcrumbs.log");
    fs::write(&path, "prior-valid\n").expect("seed prior file");
    let mut state = WorkerState::new(3, 1);
    state.capture(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started));

    let error = persist_state_with_replace(&path, &state, MIN_FILE_BYTES, |_, _| {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected replace failure"))
    })
    .expect_err("replace must fail");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read_to_string(&path).expect("read prior"), "prior-valid\n");
    assert!(!path.with_extension("tmp").exists(), "temporary file survived failure");
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn recording_never_waits_for_a_full_channel() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(1);
    let recorder = BreadcrumbRecorder::from_sender(tx);

    assert_eq!(
        recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started)),
        RecordOutcome::Queued
    );
    let started = Instant::now();
    assert_eq!(
        recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready)),
        RecordOutcome::DroppedFull
    );
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "a full breadcrumb queue blocked the caller for {:?}",
        started.elapsed()
    );
    assert_eq!(recorder.stats(), BreadcrumbStats { queued: 1, dropped: 1 });
}

#[test]
fn writer_persists_allowlisted_events_on_its_background_thread() {
    let dir = scratch("async");
    let writer = BreadcrumbWriter::start(&dir, "session-async", limits()).expect("start writer");
    let recorder = writer.recorder();

    assert_eq!(recorder.record(BreadcrumbEvent::Version(version())), RecordOutcome::Queued);
    assert_eq!(
        recorder.record(BreadcrumbEvent::Platform(Platform::current())),
        RecordOutcome::Queued
    );
    assert_eq!(
        recorder.record(BreadcrumbEvent::Renderer {
            identity: RendererIdentity::Wgpu,
            mode: RendererMode::Gpu,
            adapter: AdapterClass::Hardware,
        }),
        RecordOutcome::Queued
    );
    assert_eq!(
        recorder.record(BreadcrumbEvent::Counts { windows: 2, panes: 3 }),
        RecordOutcome::Queued
    );
    assert_eq!(
        recorder.record(BreadcrumbEvent::ResourceSnapshot(ProcessMemory {
            private_committed: MemoryMetric::Bytes(11),
            resident: MemoryMetric::Bytes(22),
            virtual_bytes: MemoryMetric::Unsupported,
        })),
        RecordOutcome::Queued
    );
    assert_eq!(
        recorder.record(BreadcrumbEvent::RetentionSnapshot {
            session_bytes: 33,
            renderer_bytes: 44,
            live_renderers: 2,
            allocator: Some(BreadcrumbAllocator {
                allocated_bytes: 55,
                reserved_bytes: 66,
                allocations: 77,
                blocks: 88,
                largest_block_bytes: 99,
            }),
        }),
        RecordOutcome::Queued
    );
    drop(recorder);

    let stats = writer.shutdown().expect("flush writer");
    assert_eq!(stats, BreadcrumbStats { queued: 6, dropped: 0 });
    let written = fs::read_to_string(breadcrumb_path(&dir, "session-async").expect("path"))
        .expect("read breadcrumbs");
    for expected in [
        "event=version version=1.2.3",
        "event=platform platform=",
        "event=renderer identity=wgpu mode=gpu adapter=hardware",
        "event=counts windows=2 panes=3",
        "event=resource private_committed=11 resident=22 virtual=unsupported",
        "event=retention session_bytes=33 renderer_bytes=44 live_renderers=2 allocator_allocated_bytes=55 allocator_reserved_bytes=66 allocator_allocations=77 allocator_blocks=88 allocator_largest_block_bytes=99",
    ] {
        assert!(written.contains(expected), "missing {expected:?} in {written:?}");
    }

    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn retention_snapshot_renders_all_maximum_allocator_fields() {
    let rendered = BreadcrumbEvent::RetentionSnapshot {
        session_bytes: u64::MAX,
        renderer_bytes: u64::MAX,
        live_renderers: u32::MAX,
        allocator: Some(BreadcrumbAllocator {
            allocated_bytes: u64::MAX,
            reserved_bytes: u64::MAX,
            allocations: u32::MAX,
            blocks: u32::MAX,
            largest_block_bytes: u64::MAX,
        }),
    }
    .render(i64::MIN);
    for expected in [
        format!("allocator_allocated_bytes={}", u64::MAX),
        format!("allocator_reserved_bytes={}", u64::MAX),
        format!("allocator_allocations={}", u32::MAX),
        format!("allocator_blocks={}", u32::MAX),
        format!("allocator_largest_block_bytes={}", u64::MAX),
    ] {
        assert!(rendered.contains(&expected), "missing {expected}: {rendered}");
    }
}

#[test]
fn retention_snapshot_renders_explicitly_unsupported_allocator() {
    let rendered = BreadcrumbEvent::RetentionSnapshot {
        session_bytes: 1,
        renderer_bytes: 2,
        live_renderers: 3,
        allocator: None,
    }
    .render(4);
    assert!(rendered.ends_with("allocator=unsupported"), "{rendered}");
}

#[test]
fn breadcrumb_event_and_allocator_remain_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<BreadcrumbEvent>();
    assert_copy::<BreadcrumbAllocator>();
}

#[test]
fn default_pressure_window_is_four_minutes() {
    let limits = BreadcrumbLimits::default();
    assert_eq!(limits.pressure_interval, Duration::from_secs(5));
    assert_eq!(limits.history_capacity, 48);
    assert_eq!(
        limits.pressure_interval.saturating_mul(limits.history_capacity as u32),
        Duration::from_secs(240)
    );
}

#[test]
fn lifecycle_history_is_ordered_and_not_coalesced() {
    let dir = scratch("lifecycle-history");
    let writer = BreadcrumbWriter::start(&dir, "session-lifecycle", limits()).expect("start");
    let recorder = writer.recorder();

    for event in [LifecycleEvent::Started, LifecycleEvent::Ready, LifecycleEvent::CleanShutdown] {
        assert_eq!(recorder.record(BreadcrumbEvent::Lifecycle(event)), RecordOutcome::Queued);
    }
    drop(recorder);
    writer.shutdown().expect("flush writer");

    let written = fs::read_to_string(breadcrumb_path(&dir, "session-lifecycle").expect("path"))
        .expect("read breadcrumbs");
    let lifecycle: Vec<_> =
        written.lines().filter(|line| line.contains("event=lifecycle")).collect();
    assert_eq!(lifecycle.len(), 3, "lifecycle history was coalesced: {written}");
    for (line, expected) in lifecycle.iter().zip(["started", "ready", "clean_shutdown"]) {
        assert!(line.contains(expected), "expected {expected:?} in {line:?}");
    }

    fs::remove_dir_all(dir).expect("remove scratch directory");
}

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
    let text = fs::read_to_string(&path).expect("read");
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
    assert!(fs::metadata(path).expect("metadata").len() <= max_file_bytes);
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn repeated_state_is_coalesced_to_the_latest_value() {
    let dir = scratch("coalesced");
    let limits = BreadcrumbLimits { queue_capacity: 64, ..limits() };
    let writer = BreadcrumbWriter::start(&dir, "session-coalesced", limits).expect("start");
    let recorder = writer.recorder();

    for panes in 1..=20 {
        let _ = recorder.record(BreadcrumbEvent::Counts { windows: 1, panes });
    }
    drop(recorder);
    writer.shutdown().expect("flush writer");

    let written = fs::read_to_string(breadcrumb_path(&dir, "session-coalesced").expect("path"))
        .expect("read breadcrumbs");
    let count_lines: Vec<_> =
        written.lines().filter(|line| line.contains("event=counts")).collect();
    assert_eq!(count_lines.len(), 1, "state updates were not coalesced: {written}");
    assert!(count_lines[0].contains("panes=20"), "latest state was not retained: {written}");

    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn start_rejects_each_invalid_limit() {
    let dir = scratch("invalid-limits");
    let large_ring = 64;
    let dynamic_required = required_file_bytes(large_ring).expect("required bytes");
    assert!(dynamic_required > MIN_FILE_BYTES);
    let over_lifecycle_required =
        required_file_bytes(MAX_LIFECYCLE_CAPACITY + 1).expect("over-cap lifecycle bytes");
    let cases = [
        BreadcrumbLimits { max_file_bytes: MIN_FILE_BYTES - 1, ..limits() },
        BreadcrumbLimits {
            ring_capacity: large_ring,
            max_file_bytes: dynamic_required - 1,
            ..limits()
        },
        BreadcrumbLimits { ring_capacity: MIN_LIFECYCLE_CAPACITY - 1, ..limits() },
        BreadcrumbLimits {
            ring_capacity: MAX_LIFECYCLE_CAPACITY + 1,
            max_file_bytes: over_lifecycle_required,
            ..limits()
        },
        BreadcrumbLimits { history_capacity: 0, ..limits() },
        BreadcrumbLimits { history_capacity: MAX_HISTORY_CAPACITY + 1, ..limits() },
        BreadcrumbLimits { queue_capacity: 0, ..limits() },
        BreadcrumbLimits { queue_capacity: MAX_QUEUE_CAPACITY + 1, ..limits() },
        BreadcrumbLimits { pressure_interval: Duration::ZERO, ..limits() },
        BreadcrumbLimits {
            pressure_interval: MAX_PRESSURE_INTERVAL + Duration::from_secs(1),
            ..limits()
        },
    ];
    for (index, invalid) in cases.into_iter().enumerate() {
        assert_eq!(
            BreadcrumbWriter::start(&dir, &format!("invalid-{index}"), invalid)
                .expect_err("invalid limits must be rejected")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    let accepted = BreadcrumbLimits { history_capacity: MAX_HISTORY_CAPACITY, ..limits() };
    BreadcrumbWriter::start(&dir, "accepted-max-history", accepted)
        .expect("maximum history capacity is accepted")
        .shutdown()
        .expect("shutdown");
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn dynamic_file_budget_includes_mandatory_records_and_one_history() {
    let minimum = required_file_bytes(MIN_LIFECYCLE_CAPACITY).expect("required bytes");
    assert!(minimum <= MIN_FILE_BYTES);
    let large = required_file_bytes(64).expect("large required bytes");
    assert!(large > minimum);
}

#[test]
fn arbitrary_terminal_command_and_environment_text_cannot_enter_an_event() {
    fn assert_typed_and_copy<T: Copy>() {}
    assert_typed_and_copy::<BreadcrumbEvent>();

    for text in
        ["1.2.3\ncommand=ssh host", "TOKEN=secret", "1.2.3 shell=/bin/zsh", "../../../environment"]
    {
        assert!(
            text.parse::<AppVersion>().is_err(),
            "accepted free-form version payload: {text:?}"
        );
    }

    let dir = scratch("privacy");
    for id in ["../command", "session\nenvironment=secret", "token=credential"] {
        assert_eq!(
            BreadcrumbWriter::start(&dir, id, limits())
                .expect_err("unsafe session id must be rejected")
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    let renderer = BreadcrumbEvent::Renderer {
        identity: RendererIdentity::Wgpu,
        mode: RendererMode::Gpu,
        adapter: AdapterClass::Hardware,
    };
    assert!(!format!("{renderer:?}").contains("command"));
    assert!(!format!("{renderer:?}").contains("TOKEN"));

    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn retention_defaults_bound_crashes_and_breadcrumbs_on_all_three_axes() {
    let config = crate::config::LoggingConfig::default();
    assert_eq!(config.max_crash_dumps, 10);
    assert_eq!(config.max_crash_age_days, 2);
    assert_eq!(config.max_crash_bytes, 10 * 1024 * 1024);
    assert_eq!(config.max_breadcrumb_files, 10);
    assert_eq!(config.max_breadcrumb_age_days, 2);
    assert_eq!(config.max_breadcrumb_bytes, 1024 * 1024);
}

fn write_sized(path: &Path, bytes: usize) {
    fs::write(path, vec![b'x'; bytes]).expect("write sized artifact");
}

fn set_modified(path: &Path, modified: std::time::SystemTime) {
    let file = fs::OpenOptions::new().write(true).open(path).expect("open artifact");
    file.set_times(fs::FileTimes::new().set_modified(modified)).expect("set artifact timestamp");
}

fn retained_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .expect("read artifact directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn cleanup_bounds_crashes_by_count_age_and_aggregate_bytes() {
    let dir = scratch("crash-retention");
    let crashes = dir.join("crashes");
    fs::create_dir_all(&crashes).expect("crash directory");
    let old = crashes.join("crash-old.log");
    let oldest = crashes.join("crash-1.log");
    let middle = crashes.join("crash-2.log");
    let newest = crashes.join("crash-3.log");
    for path in [&old, &oldest, &middle, &newest] {
        write_sized(path, 60);
    }
    let now = std::time::SystemTime::now();
    set_modified(&old, now - Duration::from_secs(3 * 86_400));
    set_modified(&oldest, now - Duration::from_secs(30));
    set_modified(&middle, now - Duration::from_secs(20));
    set_modified(&newest, now - Duration::from_secs(10));

    let config = crate::config::LoggingConfig {
        max_crash_dumps: 2,
        max_crash_age_days: 1,
        max_crash_bytes: 100,
        max_breadcrumb_files: 100,
        max_breadcrumb_age_days: 0,
        max_breadcrumb_bytes: u64::MAX,
        ..Default::default()
    };
    crate::cleanup::cleanup_old_files(&dir, &config);

    assert_eq!(
        retained_names(&crashes),
        vec!["crash-3.log"],
        "age removes old; count and bytes evict oldest survivors"
    );
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn cleanup_bounds_breadcrumbs_by_count_age_and_aggregate_bytes() {
    let dir = scratch("breadcrumb-retention");
    let breadcrumbs = dir.join("breadcrumbs");
    fs::create_dir_all(&breadcrumbs).expect("breadcrumb directory");
    let unrelated = breadcrumbs.join("notes.txt");
    let old = breadcrumbs.join("breadcrumbs-old.log");
    let oldest = breadcrumbs.join("breadcrumbs-1.log");
    let middle = breadcrumbs.join("breadcrumbs-2.log");
    let newest = breadcrumbs.join("breadcrumbs-3.log");
    write_sized(&unrelated, 500);
    for path in [&old, &oldest, &middle, &newest] {
        write_sized(path, 60);
    }
    let now = std::time::SystemTime::now();
    set_modified(&old, now - Duration::from_secs(3 * 86_400));
    set_modified(&oldest, now - Duration::from_secs(30));
    set_modified(&middle, now - Duration::from_secs(20));
    set_modified(&newest, now - Duration::from_secs(10));

    let config = crate::config::LoggingConfig {
        max_crash_dumps: 100,
        max_crash_age_days: 0,
        max_crash_bytes: u64::MAX,
        max_breadcrumb_files: 2,
        max_breadcrumb_age_days: 1,
        max_breadcrumb_bytes: 100,
        ..Default::default()
    };
    crate::cleanup::cleanup_old_files(&dir, &config);

    assert_eq!(
        retained_names(&breadcrumbs),
        vec!["breadcrumbs-3.log", "notes.txt"],
        "cleanup must bound breadcrumb artifacts without deleting unrelated files"
    );
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn cleanup_bounds_interrupted_breadcrumb_temp_files() {
    let dir = scratch("breadcrumb-temp-retention");
    let breadcrumbs = dir.join("breadcrumbs");
    fs::create_dir_all(&breadcrumbs).expect("breadcrumb directory");
    let interrupted = breadcrumbs.join("breadcrumbs-interrupted.tmp");
    write_sized(&interrupted, 200);

    let config = crate::config::LoggingConfig {
        max_breadcrumb_files: 10,
        max_breadcrumb_age_days: 0,
        max_breadcrumb_bytes: 100,
        ..Default::default()
    };
    crate::cleanup::cleanup_old_files(&dir, &config);

    assert!(
        !interrupted.exists(),
        "a process killed mid-rename must not leave bytes outside the breadcrumb budget"
    );
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn zero_aggregate_bytes_disables_that_axis_without_disabling_count_or_age() {
    let dir = scratch("retention-zero");
    let crashes = dir.join("crashes");
    fs::create_dir_all(&crashes).expect("crash directory");
    write_sized(&crashes.join("crash-1.log"), 60);
    write_sized(&crashes.join("crash-2.log"), 60);

    let config = crate::config::LoggingConfig {
        max_crash_dumps: 1,
        max_crash_age_days: 0,
        max_crash_bytes: 0,
        ..Default::default()
    };
    crate::cleanup::cleanup_old_files(&dir, &config);

    assert_eq!(retained_names(&crashes).len(), 1, "count cap must still apply");
    fs::remove_dir_all(dir).expect("remove scratch directory");
}

#[test]
fn worker_io_failure_never_turns_record_into_a_blocking_write() {
    let dir = scratch("io-failure");
    let blocked_parent = dir.join("not-a-directory");
    fs::write(&blocked_parent, "file").expect("blocking parent file");
    let writer = BreadcrumbWriter::start(&blocked_parent, "session-io", limits())
        .expect("thread spawn does not perform breadcrumb IO");
    let recorder = writer.recorder();

    let started = Instant::now();
    let outcome = recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started));
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "record attempted filesystem IO for {:?}",
        started.elapsed()
    );
    assert!(matches!(outcome, RecordOutcome::Queued | RecordOutcome::WorkerStopped));
    drop(recorder);
    assert!(writer.shutdown().is_err(), "worker must surface its filesystem error at shutdown");

    fs::remove_dir_all(dir).expect("remove scratch directory");
}
