use super::*;

use crate::process_memory::MemoryMetric;
use std::fs;
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
    BreadcrumbLimits { queue_capacity: 8, ring_capacity: 8, max_file_bytes: 4096 }
}

fn version() -> AppVersion {
    "1.2.3".parse().expect("valid version")
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
        "event=retention session_bytes=33 renderer_bytes=44 live_renderers=2",
    ] {
        assert!(written.contains(expected), "missing {expected:?} in {written:?}");
    }

    fs::remove_dir_all(dir).expect("remove scratch directory");
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
fn repeated_state_is_coalesced_to_the_latest_value() {
    let dir = scratch("coalesced");
    let limits = BreadcrumbLimits { queue_capacity: 64, ring_capacity: 8, max_file_bytes: 4096 };
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
fn memory_and_disk_are_bounded() {
    let dir = scratch("bounded");
    let limits = BreadcrumbLimits { queue_capacity: 64, ring_capacity: 3, max_file_bytes: 130 };
    let writer = BreadcrumbWriter::start(&dir, "session-bounded", limits).expect("start");
    let recorder = writer.recorder();

    for _ in 0..20 {
        let _ = recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready));
    }
    drop(recorder);
    writer.shutdown().expect("flush writer");

    let path = breadcrumb_path(&dir, "session-bounded").expect("path");
    let metadata = fs::metadata(&path).expect("breadcrumb metadata");
    let written = fs::read_to_string(path).expect("read breadcrumbs");
    assert!(written.lines().count() <= 3, "ring capacity exceeded: {written}");
    assert!(metadata.len() <= 130, "disk byte bound exceeded: {}", metadata.len());

    fs::remove_dir_all(dir).expect("remove scratch directory");
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
