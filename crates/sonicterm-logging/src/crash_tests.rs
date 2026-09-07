use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const CHILD_CASE: &str = "SONICTERM_CRASH_FILTER_CASE";
const CHILD_DIRECTORY: &str = "SONICTERM_CRASH_FILTER_DIR";
static RING_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn child_output(command: &mut std::process::Command) -> std::process::Output {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read_pipe = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read_pipe(Box::new(stdout));
    let stderr = read_pipe(Box::new(stderr));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            stdout.join().unwrap();
            stderr.join().unwrap();
            panic!("logging test subprocess exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    std::process::Output { status, stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() }
}

struct Counted<'a>(&'a AtomicUsize);

impl std::fmt::Debug for Counted<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fetch_add(1, Ordering::SeqCst);
        formatter.write_str("raw-shaping-probe")
    }
}

// Fresh dispatchers exercise the selected filter and log bridge rather than a filter-only stand-in.
#[test]
fn production_crash_admission_uses_selected_filter_in_fresh_processes() {
    let mut failures = Vec::new();
    for (case, filter) in [
        ("error", None),
        ("warn", None),
        ("info", None),
        ("debug", None),
        ("trace", Some("sonicterm=trace")),
        (
            "target-off",
            Some(
                "sonicterm=trace,sonicterm_font::shaper::harfbuzz=off,sonicterm_font::payload=off",
            ),
        ),
        ("invalid", Some("[=invalid")),
    ] {
        let directory =
            std::env::temp_dir().join(format!("sonicterm-crash-{}-{case}", std::process::id()));
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "crash::crash_tests::production_crash_filter_child", "--nocapture"])
            .env(CHILD_CASE, case)
            .env(CHILD_DIRECTORY, &directory)
            .env_remove("RUST_LOG");
        if let Some(filter) = filter {
            command.env("RUST_LOG", filter);
        }
        let output = child_output(&mut command);
        let _ = std::fs::remove_dir_all(&directory);
        if !output.status.success() {
            failures.push(format!(
                "case {case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// The child emits both facades and a WARN control through the actual init_in subscriber.
#[test]
fn production_crash_filter_child() {
    let Ok(case) = std::env::var(CHILD_CASE) else { return };
    let level = match case.as_str() {
        "error" => crate::LogLevel::Error,
        "info" => crate::LogLevel::Info,
        "debug" => crate::LogLevel::Debug,
        _ => crate::LogLevel::Warn,
    };
    let directory = PathBuf::from(std::env::var_os(CHILD_DIRECTORY).unwrap());
    let guard =
        crate::init_in(&crate::LoggingConfig { level, ..Default::default() }, &directory).unwrap();
    let tracing_formats = AtomicUsize::new(0);
    let log_formats = AtomicUsize::new(0);
    let tracing_eager = AtomicUsize::new(0);
    let log_eager = AtomicUsize::new(0);
    tracing::trace!(target: "sonicterm_font::shaper::harfbuzz", value = ?{ tracing_eager.fetch_add(1, Ordering::SeqCst); Counted(&tracing_formats) }, "trace-probe");
    log::trace!(target: "sonicterm_font::shaper::harfbuzz", "{:?}", { log_eager.fetch_add(1, Ordering::SeqCst); Counted(&log_formats) });
    tracing::debug!(target: "sonicterm_font::shaper::harfbuzz", stage = "metrics", "safe-debug");
    log::debug!(target: "sonicterm_font::shaper::harfbuzz", "safe-log-debug");
    tracing::warn!(target: "sonicterm_font::shaper::harfbuzz", "safe-shaper-warn");
    log::warn!(target: "sonicterm_font::shaper::harfbuzz", "safe-log-warn");
    tracing::warn!(target: "sonicterm_logging::control", "warn-positive-control");
    tracing::error!(target: "sonicterm_logging::control", "error-positive-control");
    tracing::warn!(target: "sonicterm_font::payload", "raw-tracing-payload");
    log::warn!(target: "sonicterm_font::payload", "raw-log-payload");
    let dump = __test_write_dump(&directory.join("dump"), "bounded probe").unwrap();
    let history = std::fs::read_to_string(dump).unwrap();
    drop(guard);
    if case == "trace" {
        assert!(tracing_formats.load(Ordering::SeqCst) > 0);
        assert!(log_formats.load(Ordering::SeqCst) > 0);
    } else {
        assert_eq!(tracing_formats.load(Ordering::SeqCst), 0);
        assert_eq!(log_formats.load(Ordering::SeqCst), 0);
        assert_eq!(tracing_eager.load(Ordering::SeqCst), 0);
        assert_eq!(log_eager.load(Ordering::SeqCst), usize::from(case == "target-off"));
    }
    assert!(history.contains("error-positive-control"));
    assert_eq!(history.contains("warn-positive-control"), case != "error");
    let shaper_warn = !matches!(case.as_str(), "error" | "target-off");
    assert_eq!(history.contains("safe-shaper-warn"), shaper_warn);
    assert_eq!(history.contains("safe-log-warn"), shaper_warn);
    assert_eq!(history.contains("safe-debug"), matches!(case.as_str(), "debug" | "trace"));
    assert_eq!(history.contains("safe-log-debug"), matches!(case.as_str(), "debug" | "trace"));
    assert!(!history.contains("raw-shaping-probe") && !history.contains("trace-probe"));
    assert!(!history.contains("raw-tracing-payload") && !history.contains("raw-log-payload"));
}

// The actual panic hook bounds dump payload and summary separately without changing unwind semantics.
#[test]
fn panic_text_is_bounded_in_a_fresh_process() {
    const CHILD: &str = "SONICTERM_PANIC_BOUND_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = child_output(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "crash::crash_tests::panic_text_is_bounded_in_a_fresh_process",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("SONICTERM_PANIC_ABORT")
                .env_remove("SONIC_PANIC_ABORT")
                .env_remove("RUST_LOG"),
        );
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("sonicterm-panic-bound-{}", std::process::id()));
    let payload = "panic文".repeat(100_000);
    let summary = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured = summary.clone();
    std::panic::set_hook(Box::new(move |info| {
        *captured.lock().unwrap() = summarize(info).to_string();
    }));
    install_panic_hook(directory.clone());
    assert!(std::panic::catch_unwind(|| std::panic::panic_any(payload)).is_err());
    std::panic::set_hook(Box::new(|_| {}));
    let summary = summary.lock().unwrap();
    assert!(summary.len() <= 4096);
    assert!(summary.ends_with("[truncated]"));
    let dump =
        std::fs::read_dir(directory.join("crashes")).unwrap().next().unwrap().unwrap().path();
    let contents = std::fs::read_to_string(dump).unwrap();
    let payload_line = contents.lines().find_map(|line| line.strip_prefix("message:   ")).unwrap();
    assert!(payload_line.len() <= 4096);
    assert!(payload_line.ends_with("[truncated]"));
    std::fs::remove_dir_all(directory).unwrap();
}

// Byte admission complements record count, including target bytes and UTF-8 truncation markers.
#[test]
fn crash_history_bounds_large_multibyte_and_many_small_records() {
    let _serial = RING_TEST_LOCK.lock().unwrap();
    let target = "target".repeat(100);
    let message = "boundary文".repeat(1000);
    __test_push(tracing::Level::WARN, &target, &message);
    {
        let history = ring().lock();
        let entry = history.last().unwrap();
        assert!(entry.target.len() <= 256);
        assert!(entry.target.len() + entry.message.len() <= 4096);
        assert!(entry.message.ends_with("[truncated]"));
    }
    for _ in 0..60 {
        __test_push(tracing::Level::WARN, &target, &message);
    }
    {
        let history = ring().lock();
        assert!(history.len() <= 50);
        assert!(
            history.iter().map(|entry| entry.target.len() + entry.message.len()).sum::<usize>()
                <= 64 * 1024
        );
    }
    for _ in 0..60 {
        __test_push(tracing::Level::WARN, "short", "short");
    }
    let history = ring().lock();
    assert_eq!(history.len(), 50);
    assert!(history.iter().all(|entry| &*entry.message == "short"));
}

// Fragmented UTF-8 output and formatting callbacks stop at the recorder's byte bound, not after allocation.
#[test]
fn bounded_text_stops_streaming_and_keeps_markers_within_capacity() {
    struct Streaming<'a>(&'a AtomicUsize);
    impl std::fmt::Debug for Streaming<'_> {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for _ in 0..100_000 {
                self.0.fetch_add(1, Ordering::SeqCst);
                formatter.write_str("文x")?;
            }
            Ok(())
        }
    }
    let calls = AtomicUsize::new(0);
    let text = BoundedText::<RECORD_BYTES>::from_args(
        RECORD_BYTES,
        format_args!("{:?}", Streaming(&calls)),
    );
    assert!(text.truncated);
    assert!(calls.load(Ordering::SeqCst) <= RECORD_BYTES / 4 + 1);
    assert!(text.as_str().ends_with(TRUNCATED));
    let owned = text.into_boxed_str();
    assert!(std::mem::size_of_val(&*owned) <= RECORD_BYTES);

    for limit in 0..=TARGET_BYTES {
        let mut text = BoundedText::<TARGET_BYTES>::new(limit);
        let _ = text.write_str(&"文".repeat(limit / 3));
        let _ = text.write_str(&"x".repeat(TARGET_BYTES + 1));
        assert!(text.len <= limit);
        assert!(std::str::from_utf8(&text.bytes[..text.len]).is_ok());
        if limit >= TRUNCATED.len() {
            assert!(text.as_str().ends_with(TRUNCATED));
        }
    }
    let exact = BoundedText::<TARGET_BYTES>::from_args(
        TARGET_BYTES,
        format_args!("{}", "x".repeat(TARGET_BYTES)),
    );
    assert_eq!(exact.len, TARGET_BYTES);
    assert!(!exact.truncated);
}

// The production visitor bounds multiple fields and skips later Debug work after exhausting the record.
#[test]
fn production_visitor_enforces_owned_capacity_and_keeps_safe_fields() {
    use tracing_subscriber::layer::SubscriberExt;
    let _serial = RING_TEST_LOCK.lock().unwrap();
    ring().lock().clear();
    let subscriber = tracing_subscriber::registry()
        .with(ring_layer().with_filter(persistence_filter(EnvFilter::new("sonicterm=debug"))));
    let late = AtomicUsize::new(0);
    tracing::subscriber::with_default(subscriber, || {
        for index in 0..70 {
            let payload = if index % 2 == 0 { "x".repeat(8000) } else { "文".repeat(3000) };
            tracing::warn!(target: "sonicterm_logging::capacity", payload = %payload, late = ?Counted(&late), "bounded-record");
        }
        tracing::warn!(target: "sonicterm_font::shaper::harfbuzz", stage = "load", font_idx = 3, "safe-failure");
    });
    assert_eq!(late.load(Ordering::SeqCst), 0);
    let history = ring().lock();
    assert!(history.len() <= RING_CAPACITY);
    assert!(history.iter().all(|entry| {
        entry.target.len() <= TARGET_BYTES
            && std::mem::size_of_val(&*entry.target) + std::mem::size_of_val(&*entry.message)
                <= RECORD_BYTES
    }));
    assert!(
        history
            .iter()
            .map(|entry| std::mem::size_of_val(&*entry.target)
                + std::mem::size_of_val(&*entry.message))
            .sum::<usize>()
            <= RING_BYTES
    );
    let safe = history.last().unwrap();
    assert_eq!(&*safe.target, "sonicterm_font::shaper::harfbuzz");
    assert!(
        safe.message.contains("safe-failure")
            && safe.message.contains("stage=load")
            && safe.message.contains("font_idx=3")
    );
}
