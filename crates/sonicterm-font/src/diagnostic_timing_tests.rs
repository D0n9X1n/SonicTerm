use super::*;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
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
    let bytes = output.bytes.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

// Disabled timing must not read a clock, retain request context, or emit records.
#[test]
fn disabled_timing_skips_clock_and_request_context() {
    let output = capture("render_timing=info", || {
        assert!(Timing::begin_with_clock("disabled", || panic!("disabled clock")).is_none());
        assert!(
            RequestTiming::capture_with_clock(42, || panic!("disabled enqueue clock")).is_none()
        );
        Timing::finish(None, "returned");
    });
    assert!(output.is_empty(), "{output}");
}

// A disabled request cannot borrow the worker's timing filter or suppress its unrelated diagnostics.
#[test]
fn disabled_request_preserves_work_and_unrelated_logs() {
    let mut called = false;
    let output = capture("render_timing=debug,diagnostic_control=warn", || {
        RequestTiming::run(None, || {
            called = true;
            let timing = Timing::begin_with_clock("disabled_worker", || panic!("disabled clock"));
            assert!(timing.is_none());
            Timing::finish(timing, "returned");
            tracing::warn!(target: "diagnostic_control", "ordinary-worker-warning");
        });
        let token = Timing::begin("after_disabled");
        assert!(token.is_some());
        Timing::finish(token, "returned");
    });
    assert!(called);
    assert!(output.contains("ordinary-worker-warning"), "{output}");
    assert!(!output.contains("disabled_worker"), "{output}");
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("operation=\"after_disabled\"") && line.contains("DEBUG"))
            .count(),
        2
    );
}

// A failed disabled request must restore timing before the reused thread processes more work.
#[test]
fn panicking_disabled_request_restores_timing() {
    let output = capture("render_timing=debug", || {
        let result = std::panic::catch_unwind(|| {
            RequestTiming::run(None, || {
                assert!(Timing::begin_with_clock("disabled_panic", || panic!("disabled clock"))
                    .is_none());
                std::panic::panic_any(73_u32);
            })
        });
        assert_eq!(result.unwrap_err().downcast_ref::<u32>(), Some(&73));
        let token = Timing::begin("after_disabled_panic");
        assert!(token.is_some());
        Timing::finish(token, "returned");
    });
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 2, "{output}");
    assert!(
        lines
            .iter()
            .all(|line| line.contains("DEBUG")
                && line.contains("operation=\"after_disabled_panic\"")),
        "{output}"
    );
}

// Explicit operation results retain DEBUG level and their captured parent even under another active span.
#[test]
fn operation_records_pair_with_original_parent_and_outcome() {
    let output = capture("render_timing=debug", || {
        let parent = tracing::debug_span!(target: "render_timing", "font_phase", window_id = 42, scale = 1.25, phase = "BaselineRender");
        for outcome in ["returned", "ok", "error"] {
            let token = parent.in_scope(|| Timing::begin("shape_impl"));
            assert!(token.is_some());
            let other =
                tracing::debug_span!(target: "render_timing", "wrong_parent", window_id = 99);
            other.in_scope(|| Timing::finish(token, outcome));
        }
    });
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 6, "{output}");
    assert!(lines.iter().all(|line| line.contains("DEBUG")), "{output}");
    for (pair, outcome) in lines.as_chunks::<2>().0.iter().zip(["returned", "ok", "error"]) {
        assert!(pair[0].contains("phase=\"enter\""), "{output}");
        assert!(pair[1].contains("phase=\"return\""), "{output}");
        assert!(pair[1].contains(&format!("outcome=\"{outcome}\"")), "{output}");
        assert!(pair[1].contains("elapsed_ms="), "{output}");
        assert!(pair.iter().all(|line| line.contains("window_id=42")), "{output}");
    }
    assert!(!output.contains("wrong_parent"));
}

// Unwinding must leave an unmatched entry instead of inventing successful completion.
#[test]
fn unwinding_does_not_emit_a_return() {
    let output = capture("render_timing=debug", || {
        let result = std::panic::catch_unwind(|| {
            let _timing = Timing::begin("incomplete");
            panic!("fixture panic");
        });
        assert!(result.is_err());
    });
    assert_eq!(output.lines().count(), 1, "{output}");
    assert!(output.contains("phase=\"enter\"") && !output.contains("phase=\"return\""));
}

// A held operation exposes its entry before release and records completion only after work returns.
#[test]
fn held_operation_has_no_premature_return() {
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("render_timing=debug")
        .with_ansi(false)
        .without_time()
        .with_writer(output.clone())
        .finish();
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel(1);
    let worker = sonicterm_logging::test_capture::with_default(subscriber, || {
        let context = RequestTiming::capture(33);
        std::thread::spawn(move || {
            RequestTiming::run(context, || {
                let token = Timing::begin("held_operation");
                entered.send(()).unwrap();
                held.recv_timeout(Duration::from_secs(5)).unwrap();
                Timing::finish(token, "returned");
            })
        })
    });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let before = String::from_utf8(output.bytes.lock().unwrap().clone()).unwrap();
    release.send(()).unwrap();
    worker.join().unwrap();
    let after = String::from_utf8(output.bytes.lock().unwrap().clone()).unwrap();
    let before: Vec<_> =
        before.lines().filter(|line| line.contains("operation=\"held_operation\"")).collect();
    let after: Vec<_> =
        after.lines().filter(|line| line.contains("operation=\"held_operation\"")).collect();
    assert_eq!(before.len(), 1);
    assert!(before[0].contains("phase=\"enter\""));
    assert_eq!(after.len(), 2);
    assert!(after[1].contains("phase=\"return\""));
}

// Ordered observations from the locator, the receive-entry record and the shaping caller.
#[derive(Debug)]
enum FontWaitEvent {
    LocatorEntered(Vec<char>),
    LocatorReleased,
    ReceiveEntered,
    CallerReturned,
}

struct ReceiveBoundary {
    events: mpsc::Sender<FontWaitEvent>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ReceiveBoundary {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        #[derive(Default)]
        struct Fields {
            receive: bool,
            enter: bool,
        }
        impl tracing::field::Visit for Fields {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                match field.name() {
                    "operation" => self.receive = value == "fallback_receive",
                    "phase" => self.enter = value == "enter",
                    _ => {}
                }
            }

            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        if event.metadata().target() == "render_timing" && fields.receive && fields.enter {
            let _ = self.events.send(FontWaitEvent::ReceiveEntered);
        }
    }
}

struct HeldLocator {
    events: mpsc::Sender<FontWaitEvent>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl crate::locator::FontLocator for HeldLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<crate::parser::ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(
        &self,
        requested: &[char],
    ) -> anyhow::Result<Vec<crate::parser::ParsedFont>> {
        self.events.send(FontWaitEvent::LocatorEntered(requested.to_vec()))?;
        self.release.lock().unwrap().recv_timeout(Duration::from_secs(5))?;
        self.events.send(FontWaitEvent::LocatorReleased)?;
        Ok(Vec::new())
    }
}

// Shape a missing glyph with fallback lookup held; each call owns fresh font state so tried glyphs never carry over.
fn shape_with_held_locator(blocking: bool) -> Vec<crate::shaper::GlyphInfo> {
    use tracing_subscriber::layer::SubscriberExt;

    let (events, observed) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let (requests, pending) = mpsc::channel::<crate::FallbackResolveInfo>();
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("render_timing=debug")
        .with_ansi(false)
        .without_time()
        .with_writer(output.clone())
        .finish()
        .with(ReceiveBoundary { events: events.clone() });
    let worker = std::thread::spawn(move || {
        let request = pending.recv_timeout(Duration::from_secs(5)).unwrap();
        request.process();
    });
    let caller = std::thread::spawn(move || {
        sonicterm_logging::test_capture::with_default(subscriber, || {
            let mut settings = config::Config::default();
            settings.font_locator = config::FontLocatorSelection::ConfigDirsOnly;
            settings.font_dirs = vec![
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")
            ];
            settings.font.font = vec![config::FontAttributes::new("Rec Mono St.Helens")];
            settings.warn_about_missing_glyphs = false;
            let mut inner =
                crate::FontConfigInner::new(Some(config::ConfigHandle::new(settings)), 96).unwrap();
            inner.locator =
                Arc::new(HeldLocator { events: events.clone(), release: Mutex::new(held) });
            inner.built_in = std::cell::RefCell::new(Arc::new(crate::db::FontDatabase::new()));
            inner.fallback_channel = std::cell::RefCell::new(Some(requests));
            let fonts = crate::FontConfiguration { inner: std::rc::Rc::new(inner) };
            let font = fonts.default_font().unwrap();
            let baseline = font
                .blocking_shape(
                    "H",
                    Some(crate::Presentation::Text),
                    crate::Direction::LeftToRight,
                    None,
                    None,
                )
                .unwrap();
            assert!(baseline.iter().any(|glyph| glyph.glyph_pos != 0));
            // Removing only implicit built-ins makes this miss independent of the host's installed emoji fonts.
            let mut wanted = crate::rangeset::RangeSet::new();
            wanted.add('\u{1f600}' as u32);
            for handle in font.clone_handles() {
                assert!(handle.coverage_intersection(&wanted).unwrap().is_empty());
            }
            let shaped = if blocking {
                font.blocking_shape(
                    "\u{1f600}",
                    Some(crate::Presentation::Text),
                    crate::Direction::LeftToRight,
                    None,
                    None,
                )
            } else {
                font.shape(
                    "\u{1f600}",
                    || {},
                    |_| {},
                    Some(crate::Presentation::Text),
                    crate::Direction::LeftToRight,
                    None,
                    None,
                )
            };
            let _ = events.send(FontWaitEvent::CallerReturned);
            shaped.unwrap()
        })
    });
    let observation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut locator_entered = false;
        let mut receive_entered = false;
        let mut caller_returned = false;
        while !locator_entered || if blocking { !receive_entered } else { !caller_returned } {
            match observed.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap()
            {
                FontWaitEvent::LocatorEntered(requested) => {
                    assert_eq!(requested, ['\u{1f600}']);
                    locator_entered = true;
                }
                FontWaitEvent::LocatorReleased => {
                    panic!("locator released before observer permission")
                }
                FontWaitEvent::ReceiveEntered => receive_entered = true,
                FontWaitEvent::CallerReturned => {
                    assert!(
                        !blocking,
                        "blocking_shape returned before the held locator was released"
                    );
                    caller_returned = true;
                }
            }
        }
        assert_eq!(receive_entered, blocking);
        assert_eq!(caller_returned, !blocking);
        assert!(matches!(observed.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let before = String::from_utf8(output.bytes.lock().unwrap().clone()).unwrap();
        let locator: Vec<_> =
            before.lines().filter(|line| line.contains("operation=\"fallback_locator\"")).collect();
        assert_eq!(locator.len(), 1, "{before}");
        assert!(locator[0].contains("phase=\"enter\""), "{before}");
        assert!(!before.contains("completion_called="), "{before}");
    }));
    // Release and collect both owned threads before propagating a failed observation.
    let released = release.send(());
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !caller.is_finished() || !worker.is_finished() {
        assert!(
            Instant::now() < deadline,
            "held-font fixture threads did not settle after release"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let caller = caller.join();
    let worker = worker.join();
    observation.unwrap();
    released.unwrap();
    worker.unwrap();
    let shaped = caller.unwrap();
    assert!(!shaped.is_empty());
    let remaining: Vec<_> = observed.try_iter().collect();
    if blocking {
        assert!(
            matches!(
                remaining.as_slice(),
                [FontWaitEvent::LocatorReleased, FontWaitEvent::CallerReturned]
            ),
            "blocking_shape must return after locator release: {remaining:?}"
        );
    } else {
        assert!(matches!(remaining.as_slice(), [FontWaitEvent::LocatorReleased]), "{remaining:?}");
    }
    let after = String::from_utf8(output.bytes.lock().unwrap().clone()).unwrap();
    assert!(
        after.lines().any(|line| line.contains("operation=\"fallback_locator\"")
            && line.contains("phase=\"return\"")
            && line.contains("outcome=\"ok\"")),
        "{after}"
    );
    if blocking {
        assert!(
            after.lines().any(|line| line.contains("operation=\"fallback_receive\"")
                && line.contains("phase=\"return\"")
                && line.contains("outcome=\"error\"")),
            "{after}"
        );
    }
    shaped
}

// The same missing glyph returns from shape while lookup is held, but blocking_shape waits until lookup disconnects.
#[test]
fn held_locator_distinguishes_blocking_and_async_shape() {
    let asynchronous = shape_with_held_locator(false);
    let blocking = shape_with_held_locator(true);
    assert_eq!(asynchronous, blocking);
}

// Real resolver errors must retain stage and request identity without copying error text or requested characters.
#[test]
fn resolver_wiring_records_error_outcome_without_payload() {
    struct FailingLocator;
    impl crate::locator::FontLocator for FailingLocator {
        fn load_fonts(
            &self,
            _: &[config::FontAttributes],
            _: &mut std::collections::HashSet<config::FontAttributes>,
            _: u16,
        ) -> anyhow::Result<Vec<crate::parser::ParsedFont>> {
            Ok(Vec::new())
        }

        fn locate_fallback_for_codepoints(
            &self,
            requested: &[char],
        ) -> anyhow::Result<Vec<crate::parser::ParsedFont>> {
            assert_eq!(requested, &['\u{1f600}']);
            Err(anyhow::anyhow!("private-font-input-\u{1f600}"))
        }
    }
    let pending = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let output = capture("render_timing=debug", || {
        let phase_span = tracing::debug_span!(target: "render_timing", "font_phase", window_id = 42, scale = 1.0, phase = "BaselineRender");
        phase_span.in_scope(|| {
            let mut settings = config::Config::default();
            settings.warn_about_missing_glyphs = false;
            settings.search_font_dirs_for_fallback = true;
            let completed = Arc::clone(&completed);
            let request = crate::FallbackResolveInfo {
                timing: RequestTiming::capture(91),
                no_glyphs: vec!['\u{1f600}'],
                pending: Arc::clone(&pending),
                completion: Box::new(move || {
                    completed.store(true, std::sync::atomic::Ordering::SeqCst)
                }),
                font_dirs: Arc::new(crate::db::FontDatabase::new()),
                built_in: Arc::new(crate::db::FontDatabase::new()),
                locator: Arc::new(FailingLocator),
                config: config::ConfigHandle::new(settings),
            };
            std::thread::spawn(move || request.process()).join().unwrap();
        });
    });
    assert!(pending.lock().unwrap().is_empty());
    assert!(!completed.load(std::sync::atomic::Ordering::SeqCst));
    for operation in
        ["fallback_locator", "fallback_font_dirs", "fallback_built_in", "fallback_selection"]
    {
        let lines: Vec<_> = output
            .lines()
            .filter(|line| line.contains(&format!("operation=\"{operation}\"")))
            .collect();
        assert_eq!(lines.len(), 2, "{output}");
        assert!(lines[0].contains("phase=\"enter\""), "{output}");
        assert!(lines[1].contains("phase=\"return\""), "{output}");
        assert!(
            lines.iter().all(|line| line.contains("DEBUG")
                && line.contains("request_id=91")
                && line.contains("window_id=42")),
            "{output}"
        );
    }
    assert!(
        output.lines().any(|line| line.contains("operation=\"fallback_locator\"")
            && line.contains("outcome=\"error\"")),
        "{output}"
    );
    assert!(output.contains("completion_called=false"), "{output}");
    assert!(!output.contains("private-font-input") && !output.contains('\u{1f600}'), "{output}");
}

// Raster timing must identify the font operation without logging a glyph index that can reveal the displayed character.
#[test]
fn raster_timing_excludes_character_identifiers() {
    let mut settings = config::Config::default();
    settings.font_locator = config::FontLocatorSelection::ConfigDirsOnly;
    settings.font_dirs =
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    settings.font.font = vec![config::FontAttributes::new("Rec Mono St.Helens")];
    let fonts =
        crate::FontConfiguration::new(Some(config::ConfigHandle::new(settings)), 96).unwrap();
    let font = fonts.default_font().unwrap();
    let glyphs = font
        .blocking_shape(
            "H",
            Some(crate::Presentation::Text),
            crate::Direction::LeftToRight,
            None,
            None,
        )
        .unwrap();
    let glyph = glyphs.iter().find(|glyph| glyph.glyph_pos != 0).unwrap();
    let output = capture("render_timing=debug", || {
        let raster = font.rasterize_glyph(glyph.glyph_pos, glyph.font_idx).unwrap();
        assert!(!raster.data.is_empty());
    });
    assert!(output.contains("font_raster"), "{output}");
    assert!(output.contains("operation=\"rasterize_glyph\""), "{output}");
    assert!(output.contains("outcome=\"ok\""), "{output}");
    assert!(output.contains("loaded_font_id=") && output.contains("fallback_idx="), "{output}");
    for line in output.lines() {
        let fields = line.split("font_raster{").nth(1).unwrap().split('}').next().unwrap();
        let keys: std::collections::BTreeSet<_> =
            fields.split_whitespace().map(|field| field.split_once('=').unwrap().0).collect();
        assert_eq!(keys, ["loaded_font_id", "fallback_idx"].into_iter().collect(), "{output}");
        assert_eq!(fields.split_whitespace().count(), 2, "{output}");
    }
}

// Capture belongs to each queued request, and the blocking caller measures shaping separately from receiving.
#[test]
fn timing_wiring_preserves_request_and_wait_boundaries() {
    let source = include_str!("lib.rs");
    let scheduler = source
        .split("fn schedule_fallback_resolve")
        .nth(1)
        .unwrap()
        .split("fn compute_title_font")
        .next()
        .unwrap();
    assert!(
        scheduler.find("timing: RequestTiming::capture_next()").unwrap()
            < scheduler.find("std::thread::spawn").unwrap()
    );
    let spawned = scheduler.split("std::thread::spawn").nth(1).unwrap();
    assert!(!spawned.contains("RequestTiming::capture"));
    let blocking = source
        .split("pub fn blocking_shape")
        .nth(1)
        .unwrap()
        .split("pub fn shape<")
        .next()
        .unwrap();
    assert!(blocking.contains("loaded_font_id = self.id, iteration"));
    assert!(blocking.contains("Timing::begin(\"shape_impl\")"));
    assert!(
        blocking.contains("Timing::result(\"fallback_receive\", || fallback_done_receiver.recv())")
    );
    let worker =
        source.split("fn process_inner").nth(1).unwrap().split("enum Entity").next().unwrap();
    assert!(
        worker.find("completion_called = true").unwrap()
            < worker.find("(self.completion)()").unwrap()
    );
}

// One reused worker must bind each request to its own subscriber and parent, not the worker's initial default.
#[test]
fn reused_worker_preserves_each_requests_dispatcher_and_parent() {
    let (request_sender, request_receiver) = mpsc::sync_channel::<Option<RequestTiming>>(1);
    let (done, completed) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        for context in request_receiver {
            RequestTiming::run(context, || {
                let timing = Timing::begin("locator");
                Timing::finish(timing, "ok");
            });
            if done.send(std::thread::current().id()).is_err() {
                break;
            }
        }
    });
    let mut outputs = Vec::new();
    let mut thread_ids = Vec::new();
    for (request, window, scale, phase) in
        [(11, 42, 1.0, "BaselineRender"), (22, 99, 2.0, "CandidateRender")]
    {
        outputs.push(capture("render_timing=debug", || {
            let phase_span = tracing::debug_span!(target: "render_timing", "font_phase", window_id = window, scale, phase);
            let context = phase_span.in_scope(|| RequestTiming::capture(request));
            request_sender.send(context).unwrap();
            thread_ids.push(completed.recv_timeout(Duration::from_secs(5)).unwrap());
        }));
    }
    drop(request_sender);
    worker.join().unwrap();
    assert_eq!(thread_ids[0], thread_ids[1]);
    for (index, (request, window, scale, phase)) in
        [(11, 42, "1.0", "BaselineRender"), (22, 99, "2.0", "CandidateRender")]
            .into_iter()
            .enumerate()
    {
        let output = &outputs[index];
        assert!(output.contains(&format!("request_id={request}")), "{output}");
        assert!(output.contains(&format!("window_id={window}")), "{output}");
        assert!(output.contains(&format!("scale={scale}")), "{output}");
        assert!(output.contains(&format!("phase=\"{phase}\"")), "{output}");
        assert!(output.lines().all(|line| line.contains("DEBUG")), "{output}");
        assert!(output.contains("operation=\"queue_wait\""), "{output}");
        assert!(output.contains("operation=\"locator\""), "{output}");
        let other_request = if request == 11 { 22 } else { 11 };
        assert!(!output.contains(&format!("request_id={other_request}")), "{output}");
    }
}
