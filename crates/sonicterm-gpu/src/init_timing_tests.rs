use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

fn capture(filter: &str, work: impl FnOnce()) -> String {
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .without_time()
        .with_writer(output.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, work);
    let bytes = output.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

// Disabled diagnostics must skip both the clock and event output.
#[test]
fn disabled_timing_never_reads_the_clock() {
    let output = capture("render_timing=info,memory=debug", || {
        let token = InitTiming::begin_with_clock("disabled", || panic!("disabled clock read"));
        assert!(token.is_none());
        InitTiming::finish(token, InitOutcome::Returned);
    });
    assert!(output.is_empty(), "{output}");
}

// Enabling an unrelated diagnostic target cannot enable initialization timing.
#[test]
fn unrelated_target_never_reads_the_clock() {
    let output = capture("memory=debug", || {
        let token = InitTiming::begin_with_clock("disabled", || panic!("wrong target clock read"));
        assert!(token.is_none());
        InitTiming::finish(token, InitOutcome::Returned);
    });
    assert!(output.is_empty(), "{output}");
}

// A return is not a success claim; Result outcomes remain distinct and ordered.
#[test]
fn explicit_returns_preserve_operation_and_outcome() {
    let output = capture("render_timing=debug", || {
        for outcome in [InitOutcome::Returned, InitOutcome::Ok, InitOutcome::Error] {
            let token = InitTiming::begin("request");
            assert!(token.is_some());
            InitTiming::finish(token, outcome);
        }
    });
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 6, "{output}");
    assert!(lines.iter().all(|line| line.contains("DEBUG render_timing:")), "{output}");
    for (pair, outcome) in lines.as_chunks::<2>().0.iter().zip(["returned", "ok", "error"]) {
        assert!(pair[0].contains("operation=\"request\""), "{output}");
        assert!(pair[0].contains("phase=\"enter\""), "{output}");
        assert!(pair[1].contains("operation=\"request\""), "{output}");
        assert!(pair[1].contains("phase=\"return\""), "{output}");
        assert!(pair[1].contains(&format!("outcome=\"{outcome}\"")), "{output}");
        assert!(pair[1].contains("elapsed_ms="), "{output}");
    }
}

// A return keeps the originating window span even when another span is current.
#[test]
fn finish_keeps_the_parent_captured_at_entry() {
    let output = capture("render_timing=debug", || {
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = 42);
        let token = span.in_scope(|| InitTiming::begin("font_stacks"));
        assert!(token.is_some());
        let other = tracing::debug_span!(target: "render_timing", "unrelated", window_id = 99);
        other.in_scope(|| InitTiming::finish(token, InitOutcome::Returned));
    });
    let returned = output.lines().find(|line| line.contains("phase=\"return\"")).unwrap();
    assert!(returned.contains("renderer_init{window_id=42}"), "{output}");
    assert!(!returned.contains("unrelated"), "{output}");
    assert!(!returned.contains("window_id=99"), "{output}");
}

// Unwinding cannot fabricate a completion record for a call that never returned.
#[test]
fn unwind_leaves_the_entry_unmatched() {
    let output = capture("render_timing=debug", || {
        let result = std::panic::catch_unwind(|| {
            let _token = InitTiming::begin("incomplete");
            panic!("operation did not return");
        });
        assert!(result.is_err());
    });
    assert_eq!(output.lines().count(), 1, "{output}");
    assert!(output.contains("phase=\"enter\""), "{output}");
    assert!(!output.contains("phase=\"return\""), "{output}");
    assert!(!output.contains("outcome="), "{output}");
}
