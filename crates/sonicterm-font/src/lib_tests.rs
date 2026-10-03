//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{linear_u8_to_srgb8, SrgbaPixel};
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use crate::parser::ParsedFont;
use crate::rangeset::RangeSet;
use crate::select_fallback_fonts;
use std::path::PathBuf;

pub(super) fn child_output(command: &mut std::process::Command) -> std::process::Output {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let read_pipe = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read_pipe(Box::new(child.stdout.take().unwrap()));
    let stderr = read_pipe(Box::new(child.stderr.take().unwrap()));
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
            panic!("font test subprocess exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    std::process::Output { status, stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() }
}

// Real fallback warnings preserve stage/count identity while both WARN and DEBUG exclude requested text.
#[test]
fn fallback_resolution_diagnostics_exclude_requested_text() {
    const CHILD: &str = "SONICTERM_RESOLVER_LOG_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for filter in ["sonicterm=warn", "sonicterm=debug", "sonicterm=trace"] {
            let output = child_output(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "lib_tests::fallback_resolution_diagnostics_exclude_requested_text",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .env("RUST_LOG", filter),
            );
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    struct FailingLocator;
    impl crate::locator::FontLocator for FailingLocator {
        fn load_fonts(
            &self,
            _: &[config::FontAttributes],
            _: &mut std::collections::HashSet<config::FontAttributes>,
            _: u16,
        ) -> anyhow::Result<Vec<ParsedFont>> {
            Ok(Vec::new())
        }
        fn locate_fallback_for_codepoints(&self, _: &[char]) -> anyhow::Result<Vec<ParsedFont>> {
            Err(anyhow::anyhow!("synthetic-secret-\u{1f600}"))
        }
    }
    let directory =
        std::env::temp_dir().join(format!("sonicterm-resolver-log-{}", std::process::id()));
    let guard =
        sonicterm_logging::init_in(&sonicterm_logging::LoggingConfig::default(), &directory)
            .unwrap();
    for warn in [true, false] {
        let mut config = config::Config::default();
        config.warn_about_missing_glyphs = warn;
        crate::FallbackResolveInfo {
            timing: None,
            no_glyphs: vec!['\u{1f600}'],
            pending: Default::default(),
            completion: Box::new(|| {}),
            font_dirs: std::sync::Arc::new(crate::db::FontDatabase::new()),
            built_in: std::sync::Arc::new(crate::db::FontDatabase::new()),
            locator: std::sync::Arc::new(FailingLocator),
            config: config::ConfigHandle::new(config),
            notice: crate::FallbackNotice::new(),
            cancel: Default::default(),
            hooks: Default::default(),
        }
        .process();
    }
    tracing::warn!(target: "sonicterm_font::control", "resolver-positive-control");
    let path =
        sonicterm_logging::crash::__test_write_dump(&directory.join("dump"), "synthetic").unwrap();
    let history = std::fs::read_to_string(path).unwrap();
    drop(guard);
    let mut sink = String::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("sonicterm.log") {
            sink.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
    }
    let trace_requested = std::env::var("RUST_LOG").unwrap() == "sonicterm=trace";
    assert_eq!(sink.contains("synthetic-secret"), trace_requested);
    assert_eq!(sink.contains("sonicterm_font::payload"), trace_requested);
    assert!(history.contains("resolver-positive-control"));
    assert!(history.contains("stage=font-locator requested=1 error=font"));
    assert!(history.contains("1 unresolved codepoints"));
    assert!(sink.contains("https://github.com/D0n9X1n/SonicTerm/wiki/Configuration"));
    assert!(sink.contains("[font].family"));
    assert!(!sink.contains("wezterm.org"));
    assert!(!sink.contains("warn_about_missing_glyphs=false"));
    assert!(
        !history.contains("synthetic-secret")
            && !history.contains('\u{1f600}')
            && !history.contains("1f600")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_font_diagnostics_use_sonicterm_guidance_without_hiding_errors() {
    const CHILD: &str = "SONICTERM_CONFIGURED_FONT_LOG_CHILD";
    const MISSING: &str = "SonicTerm Missing Diagnostic Primary";
    const OTHER: &str = "SonicTerm Missing Diagnostic Style";
    const FALLBACK: &str = "SonicTerm Missing Diagnostic Fallback";
    const PACKAGED: &str = "Rec Mono St.Helens";
    const URL: &str = "https://github.com/D0n9X1n/SonicTerm/wiki/Configuration";
    // These targets must survive a font capture; a replacement filter can silently drop them from files and crash history.
    const RETAINED_WARNINGS: [&str; 5] = [
        "font-capture-exit-control",
        "font-capture-atlas-control",
        "font-capture-reclaimed-control",
        "font-capture-wgpu-control",
        "font-capture-naga-control",
    ];

    // Isolated production logging proves missing requests remain visible and valid or synthetic requests stay quiet.
    let Some(mode) = std::env::var_os(CHILD) else {
        let capture_filter = format!("config=error,{}", sonicterm_logging::DEFAULT_FILTER);
        // Both published recipes must match the filter whose retained warnings this test exercises.
        for documentation in [
            include_str!("../../../wiki/Logging.md"),
            include_str!("../../../wiki/Logging-zh-CN.md"),
        ] {
            assert!(documentation.contains(&format!("RUST_LOG={capture_filter}")));
        }
        for mode in ["default", "admitted"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "lib_tests::configured_font_diagnostics_use_sonicterm_guidance_without_hiding_errors",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env_remove("RUST_LOG");
            if mode == "admitted" {
                command.env("RUST_LOG", &capture_filter);
            }
            let output = child_output(&mut command);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "{}\n{stderr}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(stderr.contains("font-diagnostic-positive-control"));
            assert!(stderr.contains("configured-font-error-positive-control"));
            for marker in RETAINED_WARNINGS {
                assert!(stderr.contains(marker), "missing stderr warning {marker}");
            }
            let errors: Vec<_> =
                stderr.lines().filter(|line| line.contains("Unable to load")).collect();
            assert_eq!(errors.len(), 4, "{stderr}");
            assert!(errors.iter().all(|line| line.contains("ERROR") && line.contains("config")));
            assert_eq!(errors.iter().filter(|line| line.contains(MISSING)).count(), 2);
            assert_eq!(errors.iter().filter(|line| line.contains(OTHER)).count(), 1);
            assert_eq!(errors.iter().filter(|line| line.contains(FALLBACK)).count(), 1);
            assert!(errors.iter().all(|line| !line.contains(PACKAGED)));
            let primary = errors
                .iter()
                .find(|line| line.contains(MISSING) && line.contains("Regular"))
                .unwrap();
            assert!(primary.contains("configured primary font"), "{primary}");
            assert!(
                primary.contains("weight=\"Regular\"")
                    && primary.contains("stretch=Normal")
                    && primary.contains("style=Normal")
            );
            let derived =
                errors.iter().find(|line| line.contains(MISSING) && line.contains("Bold")).unwrap();
            assert!(derived.contains("derived from the configured primary font"), "{derived}");
            for family in [OTHER, FALLBACK] {
                let requested = errors.iter().find(|line| line.contains(family)).unwrap();
                assert!(requested.contains("requested font"), "{requested}");
                assert!(!requested.contains("configured primary"), "{requested}");
            }
            for line in errors {
                assert!(line.contains(URL), "{line}");
                assert!(line.contains("[font].family"), "{line}");
                assert!(!line.contains("wezterm"), "{line}");
                assert!(!line.contains("font_rules"), "{line}");
                assert!(!line.contains("sonicterm.font("), "{line}");
                assert!(!line.contains("separate font file"), "{line}");
            }
        }
        return;
    };

    let directory =
        std::env::temp_dir().join(format!("sonicterm-font-diagnostic-{}", std::process::id()));
    let guard =
        sonicterm_logging::init_in(&sonicterm_logging::LoggingConfig::default(), &directory)
            .unwrap();
    tracing::warn!(target: "sonicterm_font::control", "font-diagnostic-positive-control");
    tracing::warn!(target: "sonic_exit", "font-capture-exit-control");
    tracing::warn!(target: "sonic::glyph_atlas", "font-capture-atlas-control");
    tracing::warn!(target: "memory::reclaimed", "font-capture-reclaimed-control");
    tracing::warn!(target: "wgpu", "font-capture-wgpu-control");
    tracing::warn!(target: "naga", "font-capture-naga-control");
    config::show_error("configured-font-error-positive-control");
    let primary = config::FontAttributes::new(MISSING);
    let mut settings = config::Config::default();
    settings.font_locator = config::FontLocatorSelection::ConfigDirsOnly;
    settings.font_dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    settings.font.font = vec![
        primary.clone(),
        config::FontAttributes::new_fallback(FALLBACK),
        config::FontAttributes::new_fallback(PACKAGED),
    ];
    let settings = config::ConfigHandle::new(settings);
    let fonts = crate::FontConfigInner::new(Some(settings.clone()), 72).unwrap();
    let (_, handles) = fonts.resolve_font_helper(&settings.font, &settings, 14).unwrap();
    assert_eq!(handles.first().unwrap().names().family, PACKAGED);
    for synthetic in [settings.font.make_bold(), settings.font.make_italic()] {
        fonts.resolve_font_helper(&synthetic, &settings, 14).unwrap();
    }

    // These nonsynthetic requests exercise the private helper's alternate wording, not the shipping style-selection path.
    let requests = [
        config::FontAttributes { weight: config::FontWeight::BOLD, ..primary },
        config::FontAttributes::new(OTHER),
        config::FontAttributes::new(FALLBACK),
        config::FontAttributes::new(PACKAGED),
    ];
    for requested in requests {
        let style = config::TextStyle {
            font: vec![requested, config::FontAttributes::new_fallback(PACKAGED)],
            foreground: None,
        };
        let (_, handles) = fonts.resolve_font_helper(&style, &settings, 14).unwrap();
        assert_eq!(handles.first().unwrap().names().family, PACKAGED);
    }
    let dump =
        sonicterm_logging::crash::__test_write_dump(&directory.join("dump"), "synthetic").unwrap();
    let history = std::fs::read_to_string(dump).unwrap();
    drop(guard);
    let mut sink = String::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("sonicterm.log") {
            sink.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
    }
    for captured in [&sink, &history] {
        assert!(captured.contains("font-diagnostic-positive-control"));
        for marker in RETAINED_WARNINGS {
            assert!(captured.contains(marker), "missing retained warning {marker}");
        }
        if mode == "admitted" {
            assert!(captured.contains("configured-font-error-positive-control"));
            assert_eq!(captured.lines().filter(|line| line.contains("Unable to load")).count(), 4);
            assert!(
                captured.contains(MISSING)
                    && captured.contains(OTHER)
                    && captured.contains(FALLBACK)
            );
            assert!(!captured
                .lines()
                .filter(|line| line.contains("Unable to load"))
                .any(|line| line.contains(PACKAGED)));
        } else {
            assert!(!captured.contains("configured-font-error-positive-control"));
            assert!(!captured.contains("Unable to load"));
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

fn fallback_fixture(coverage_chars: &[char], is_math_font: bool) -> ParsedFont {
    let mut coverage = RangeSet::new();
    for ch in coverage_chars {
        coverage.add(*ch as u32);
    }
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::BuiltIn,
        coverage: Some(coverage),
    };
    let mut font = ParsedFont::from_locator(&handle).unwrap();
    font.is_math_font = is_math_font;
    font
}

#[test]
fn exports_color_primitives() {
    assert_eq!(linear_u8_to_srgb8(0), 0);
    assert_eq!(SrgbaPixel::rgba(1, 2, 3, 4).as_rgba(), (1, 2, 3, 4));
}

#[test]
fn fallback_selection_prefers_text_for_shared_symbols_and_keeps_math_only_coverage() {
    // Contract: math coverage cannot displace text coverage or be dropped when it alone fills a gap.
    let shared = '\u{23fa}';
    let math_only = '\u{2211}';
    let math = fallback_fixture(&[shared, math_only], true);
    let text = fallback_fixture(&[shared], false);
    let mut wanted = RangeSet::new();
    wanted.add(shared as u32);
    wanted.add(math_only as u32);

    let mut selected = vec![math, text];
    select_fallback_fonts(&mut selected, &mut wanted, true);

    assert_eq!(selected.len(), 2);
    assert!(!selected[0].is_math_font);
    assert!(selected[1].is_math_font);
    assert!(wanted.is_empty());
}

#[test]
fn gdi_font_creation_failures_are_rejected_before_use() {
    const SOURCE: &str = include_str!("locator/gdi.rs");

    assert!(SOURCE.contains("anyhow::ensure!(!font.is_null(), \"font handle is null\")"));
    assert!(SOURCE.contains("anyhow::ensure!(!hdc.is_null(), \"CreateCompatibleDC failed\")"));
    assert!(SOURCE.contains("if previous.is_null()"));
    assert!(SOURCE.contains("SelectObject(hdc, previous)"));
    assert_eq!(
        SOURCE.matches("anyhow::ensure!(!font.is_null(), \"CreateFontIndirectW failed\")").count(),
        2
    );
}

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::{Duration, Instant};

/// How long any fallback test waits for the worker before failing instead of hanging.
const WORKER_WAIT: Duration = Duration::from_secs(10);

/// A one-shot gate: closed until `open`, and `wait` fails the test after `WORKER_WAIT`.
#[derive(Default)]
struct Latch {
    opened: StdMutex<bool>,
    changed: Condvar,
}

impl Latch {
    fn open(&self) {
        *self.opened.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let opened = self.opened.lock().unwrap();
        let (opened, timeout) =
            self.changed.wait_timeout_while(opened, WORKER_WAIT, |opened| !*opened).unwrap();
        assert!(*opened && !timeout.timed_out(), "a fallback latch was never opened");
    }
}

/// A locator that waits on `gate`, counts its calls, panics on `panic_on`, and answers every
/// other request with Rec Mono covering `resolves`.
struct GatedLocator {
    gate: Arc<Latch>,
    calls: Arc<AtomicUsize>,
    resolves: Vec<char>,
    panic_on: Option<char>,
}

impl crate::locator::FontLocator for GatedLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(
        &self,
        codepoints: &[char],
    ) -> anyhow::Result<Vec<ParsedFont>> {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.panic_on.is_some_and(|character| codepoints.contains(&character)) {
            panic!("test locator panics on its configured character");
        }
        self.gate.wait();
        let wanted: Vec<char> = codepoints
            .iter()
            .copied()
            .filter(|character| self.resolves.contains(character))
            .collect();
        Ok(if wanted.is_empty() { Vec::new() } else { vec![fallback_fixture(&wanted, false)] })
    }
}

/// A configuration whose only primary face is the ASCII-only sample font (family Roboto), so é, ñ
/// and ü reach the test locator; `directory` holds the font copy and is removed on drop.
struct FallbackFixture {
    configuration: crate::FontConfiguration,
    directory: PathBuf,
}

impl Drop for FallbackFixture {
    // Lifecycle: dropping `FallbackFixture` removes its temporary font `directory`.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn fallback_configuration(name: &str, locator: GatedLocator) -> FallbackFixture {
    let directory =
        std::env::temp_dir().join(format!("sonicterm-fallback-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c/test.ttf"),
        directory.join("primary.ttf"),
    )
    .unwrap();
    let mut settings = config::Config::default_config();
    settings.font =
        config::TextStyle { font: vec![config::FontAttributes::new("Roboto")], foreground: None };
    settings.font_dirs = vec![directory.clone()];
    settings.search_font_dirs_for_fallback = false;
    settings.warn_about_missing_glyphs = false;
    let configuration = crate::FontConfiguration::new_with_locator_for_test(
        config::ConfigHandle::new(settings),
        96,
        Arc::new(locator),
    )
    .unwrap();
    FallbackFixture { configuration, directory }
}

fn locator(gate: &Arc<Latch>, calls: &Arc<AtomicUsize>) -> GatedLocator {
    GatedLocator {
        gate: Arc::clone(gate),
        calls: Arc::clone(calls),
        resolves: vec!['é', 'ñ', 'ü'],
        panic_on: None,
    }
}

/// The first glyph id frame shaping gives `character`; 0 is notdef.
fn frame_glyph(font: &crate::LoadedFont, character: char) -> u32 {
    let shaped = font
        .shape_for_frame(
            &character.to_string(),
            Some(crate::Presentation::Text),
            crate::Direction::LeftToRight,
            None,
            None,
        )
        .unwrap();
    shaped[0].glyph_pos
}

fn wait_for_generation(configuration: &crate::FontConfiguration, generation: u64) {
    let started = Instant::now();
    while configuration.fallback_notice().generation() < generation {
        assert!(started.elapsed() < WORKER_WAIT, "generation {generation} was never published");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn frame_shaping_returns_notdef_at_once_while_the_locator_is_blocked() {
    // A frame never waits for fallback discovery: with the locator blocked, the character shapes as
    // notdef at once; after the face is published, a later frame merges it and gets the real glyph.
    let gate = Arc::new(Latch::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("blocked", locator(&gate, &calls));
    let font = fixture.configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, 'é'), 0, "notdef while the locator is blocked");
    gate.open();
    wait_for_generation(&fixture.configuration, 1);
    assert_ne!(frame_glyph(&font, 'é'), 0, "a later frame merges the published face");
}

/// A worker pause point: it signals `arrived`, then waits for `release`.
fn pause(arrived: &Arc<Latch>, release: &Arc<Latch>) -> Arc<dyn Fn() + Send + Sync> {
    let (arrived, release) = (Arc::clone(arrived), Arc::clone(release));
    Arc::new(move || {
        arrived.open();
        release.wait();
    })
}

#[test]
fn frame_shaping_skips_the_merge_while_the_worker_holds_the_pending_lock() {
    // Paused mid-append the worker holds `pending_fallback`, and a frame shapes notdef at once.
    // Paused after the unlock and before the completion, a frame already merges the real glyph
    // while the generation is still 0, so the completion always follows the unlock.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("contended", locator(&gate, &calls));
    let (in_append, leave_append) = (Arc::new(Latch::default()), Arc::new(Latch::default()));
    let (before_completion, complete) = (Arc::new(Latch::default()), Arc::new(Latch::default()));
    fixture.configuration.set_fallback_worker_hooks_for_test(crate::FallbackWorkerHooks {
        during_append: Some(pause(&in_append, &leave_append)),
        before_completion: Some(pause(&before_completion, &complete)),
    });
    let font = fixture.configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, 'é'), 0);
    in_append.wait();
    assert_eq!(frame_glyph(&font, 'é'), 0, "the lock is busy, so the frame skips the merge");
    leave_append.open();
    before_completion.wait();
    assert_ne!(frame_glyph(&font, 'é'), 0, "after the unlock a frame merges");
    assert_eq!(fixture.configuration.fallback_notice().generation(), 0, "no completion yet");
    complete.open();
    wait_for_generation(&fixture.configuration, 1);
}

#[test]
fn frame_shaping_gives_up_after_eight_attempts_and_the_next_frame_retries() {
    // A run that keeps asking to be shaped again costs at most 8 synchronous shapes, then errors
    // for this frame; the next frame starts over with its own 8.
    let attempts = std::cell::Cell::new(0);
    let shape = || -> anyhow::Result<u32> {
        attempts.set(attempts.get() + 1);
        Err(crate::ClearShapeCache {}.into())
    };
    assert!(crate::bounded_frame_shape(shape).is_err());
    assert_eq!(attempts.get(), crate::MAX_FRAME_SHAPE_ATTEMPTS);
    assert_eq!(crate::MAX_FRAME_SHAPE_ATTEMPTS, 8);
    assert!(crate::bounded_frame_shape(shape).is_err());
    assert_eq!(attempts.get(), 16, "the next frame retries");
    assert_eq!(crate::bounded_frame_shape(|| Ok(7_u32)).unwrap(), 7);
}

#[test]
fn queued_requests_are_skipped_once_the_configuration_is_dropped() {
    // A request queued behind a blocked lookup makes no native call after cancellation, and the
    // worker ends once the configuration and its sender are gone.
    let gate = Arc::new(Latch::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("cancelled", locator(&gate, &calls));
    let font = fixture.configuration.default_font().unwrap();
    frame_glyph(&font, 'é');
    let started = Instant::now();
    while calls.load(AtomicOrdering::SeqCst) == 0 {
        assert!(started.elapsed() < WORKER_WAIT, "the worker never reached the locator");
        std::thread::sleep(Duration::from_millis(5));
    }
    frame_glyph(&font, 'ñ');
    let worker = fixture.configuration.take_fallback_worker_for_test().expect("a worker");
    drop(font);
    drop(fixture);
    gate.open();
    worker.join().expect("the worker ends after the configuration is dropped");
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1, "the queued request made no native call");
}

#[cfg(panic = "unwind")]
#[test]
fn an_ended_worker_loses_one_request_and_the_next_character_spawns_a_new_one() {
    // The worker panics on é and ends. ñ is the request that finds it gone: it is dropped with one
    // logged error and the channel is cleared. ñ stays unresolved without a new worker, ü spawns
    // the second worker and resolves, and no other request is lost.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration(
        "ended",
        GatedLocator { panic_on: Some('é'), ..locator(&gate, &calls) },
    );
    let configuration = &fixture.configuration;
    let font = configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, 'é'), 0);
    let worker = configuration.take_fallback_worker_for_test().expect("a worker");
    assert!(worker.join().is_err(), "the worker ended in a panic");
    assert!(configuration.has_fallback_channel_for_test(), "a panic does not clear the channel");
    assert_eq!(frame_glyph(&font, 'ñ'), 0);
    assert_eq!(configuration.fallback_send_failures_for_test(), 1);
    assert!(!configuration.has_fallback_channel_for_test(), "the failed send clears the channel");
    assert_eq!(frame_glyph(&font, 'ñ'), 0, "ñ stays unresolved");
    assert_eq!(configuration.fallback_spawns_for_test(), 1, "and schedules no worker");
    assert_eq!(frame_glyph(&font, 'ü'), 0);
    assert_eq!(configuration.fallback_spawns_for_test(), 2, "ü spawns a second worker");
    wait_for_generation(configuration, 1);
    assert_ne!(frame_glyph(&font, 'ü'), 0, "the second worker resolves ü");
    assert_eq!(configuration.fallback_send_failures_for_test(), 1, "one error in total");
}

#[test]
fn blocking_shape_still_retries_after_clear_shape_cache() {
    // The explicit blocking path keeps its meaning: it waits for the fallback, merges it through a
    // `ClearShapeCache` retry and returns the real glyph.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("blocking", locator(&gate, &calls));
    let font = fixture.configuration.default_font().unwrap();
    let shaped = font
        .blocking_shape(
            "é",
            Some(crate::Presentation::Text),
            crate::Direction::LeftToRight,
            None,
            None,
        )
        .unwrap();
    assert_ne!(shaped[0].glyph_pos, 0);
}
