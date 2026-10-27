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

// Real fallback warnings preserve stage/count identity while both WARN and DEBUG exclude requested text;
// a failed stage is logged with its stage, and only a request whose stages all ran warns of unresolved ones.
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
    let directory =
        std::env::temp_dir().join(format!("sonicterm-resolver-log-{}", std::process::id()));
    let guard =
        sonicterm_logging::init_in(&sonicterm_logging::LoggingConfig::default(), &directory)
            .unwrap();
    for warn in [true, false] {
        let mut config = config::Config::default();
        config.warn_about_missing_glyphs = warn;
        let config = config::ConfigHandle::new(config);
        let locators: [std::sync::Arc<dyn crate::locator::FontLocator + Send + Sync>; 2] =
            [std::sync::Arc::new(FailingLocator), std::sync::Arc::new(EmptyLocator)];
        for locator in locators {
            fallback_request(
                vec!['\u{1f600}'],
                locator,
                config.clone(),
                crate::FallbackNotice::new(),
            )
            .process();
        }
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

pub(super) fn fallback_fixture(coverage_chars: &[char], is_math_font: bool) -> ParsedFont {
    let mut coverage = RangeSet::new();
    for ch in coverage_chars {
        coverage.add(*ch as u32);
    }
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
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
pub(super) struct Latch {
    opened: StdMutex<bool>,
    changed: Condvar,
}

impl Latch {
    pub(super) fn open(&self) {
        *self.opened.lock().unwrap() = true;
        self.changed.notify_all();
    }

    pub(super) fn wait(&self) {
        let opened = self.opened.lock().unwrap();
        let (opened, timeout) =
            self.changed.wait_timeout_while(opened, WORKER_WAIT, |opened| !*opened).unwrap();
        assert!(*opened && !timeout.timed_out(), "a fallback latch was never opened");
    }
}

/// A locator that waits on `gate`, counts its calls, panics on `panic_on`, and answers every
/// other request with Rec Mono covering `resolves`.
pub(super) struct GatedLocator {
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
pub(super) struct FallbackFixture {
    pub(super) configuration: crate::FontConfiguration,
    directory: PathBuf,
}

impl Drop for FallbackFixture {
    // Lifecycle: dropping `FallbackFixture` removes its temporary font `directory`.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub(super) fn fallback_configuration(name: &str, locator: GatedLocator) -> FallbackFixture {
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

pub(super) fn locator(gate: &Arc<Latch>, calls: &Arc<AtomicUsize>) -> GatedLocator {
    GatedLocator {
        gate: Arc::clone(gate),
        calls: Arc::clone(calls),
        resolves: vec!['é', 'ñ', 'ü'],
        panic_on: None,
    }
}

/// The first glyph id frame shaping gives `character`; 0 is notdef.
pub(super) fn frame_glyph(font: &crate::LoadedFont, character: char) -> u32 {
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

pub(super) fn wait_for_generation(configuration: &crate::FontConfiguration, generation: u64) {
    let started = Instant::now();
    while configuration.fallback_notice().generation() < generation {
        assert!(started.elapsed() < WORKER_WAIT, "generation {generation} was never published");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A worker pause point: it signals `arrived`, then waits for `release`.
pub(super) fn pause(arrived: &Arc<Latch>, release: &Arc<Latch>) -> Arc<dyn Fn() + Send + Sync> {
    let (arrived, release) = (Arc::clone(arrived), Arc::clone(release));
    Arc::new(move || {
        arrived.open();
        release.wait();
    })
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

/// A tracing sink for one test thread's captured output.
#[derive(Clone, Default)]
struct LogCapture {
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogCapture {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self {
        self.clone()
    }
}

#[cfg(panic = "unwind")]
#[test]
fn an_ended_worker_loses_one_request_and_the_next_character_spawns_a_new_one() {
    // The worker panics on ENDED and ends. LOST is the request that finds it gone: it is dropped
    // with exactly one logged error and the channel is cleared. LOST stays unresolved without a new
    // worker, é spawns the second worker and resolves, and ENDED and LOST stay unresolved after it.
    // Both are plane-16 private-use code points that no shipped or system face covers, so no face
    // merged for é can resolve them by accident.
    const ENDED: char = '\u{100001}';
    const LOST: char = '\u{100002}';
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration(
        "ended",
        GatedLocator { panic_on: Some(ENDED), resolves: vec!['é'], ..locator(&gate, &calls) },
    );
    let configuration = &fixture.configuration;
    let font = configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, ENDED), 0);
    let worker = configuration.take_fallback_worker_for_test().expect("a worker");
    assert!(worker.join().is_err(), "the worker ended in a panic");
    assert!(configuration.has_fallback_channel_for_test(), "a panic does not clear the channel");
    let output = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("error")
        .with_ansi(false)
        .without_time()
        .with_writer(output.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, || {
        assert_eq!(frame_glyph(&font, LOST), 0);
        assert!(
            !configuration.has_fallback_channel_for_test(),
            "the failed send clears the channel"
        );
        assert_eq!(frame_glyph(&font, LOST), 0, "the lost request stays unresolved");
        assert_eq!(configuration.fallback_spawns_for_test(), 1, "and schedules no worker");
        assert_eq!(frame_glyph(&font, 'é'), 0);
        assert_eq!(configuration.fallback_spawns_for_test(), 2, "é spawns a second worker");
        wait_for_generation(configuration, 1);
        assert_ne!(frame_glyph(&font, 'é'), 0, "the second worker resolves é");
        assert_eq!(
            frame_glyph(&font, ENDED),
            0,
            "the character whose worker ended stays unresolved"
        );
        assert_eq!(frame_glyph(&font, LOST), 0, "the lost request stays unresolved");
    });
    let logged = String::from_utf8(output.bytes.lock().unwrap().clone()).unwrap();
    assert_eq!(
        logged.matches("Failed to schedule font fallback resolve").count(),
        1,
        "exactly one error line: {logged}"
    );
}

#[test]
fn face_epoch_moves_only_on_face_replacement() {
    // The style-face memo above this crate trusts `face_epoch`: it must move once for each writer
    // that drops loaded faces (`config_changed`, `change_scaling`) and never for a fallback merge,
    // which mutates a face in place and so keeps every memoized `Rc` valid.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("face-epoch", locator(&gate, &calls));
    let configuration = &fixture.configuration;
    let start = configuration.face_epoch();
    let font = configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, 'é'), 0, "notdef until the fallback face is published");
    wait_for_generation(configuration, 1);
    assert_ne!(frame_glyph(&font, 'é'), 0, "the merge reached the face");
    assert_eq!(configuration.face_epoch(), start, "a fallback merge does not move the epoch");
    configuration.change_scaling(configuration.get_font_scale(), 144);
    assert_eq!(configuration.face_epoch(), start + 1, "change_scaling bumps it once");
    configuration.config_changed(&configuration.config()).unwrap();
    assert_eq!(configuration.face_epoch(), start + 2, "config_changed bumps it once");
}

#[test]
fn config_changed_bumps_the_face_epoch_before_the_fallible_reload() {
    // A reload that fails after `fonts.clear()` must still leave every memo stale, so the bump
    // sits between the clear and the fallible font-directory rebuild. The reload cannot fail
    // today, so the order is pinned on the source, CRLF-normalized.
    let source = include_str!("lib.rs").replace("\r\n", "\n");
    let body_start = source.find("fn config_changed(&self, config: &ConfigHandle)").unwrap();
    let body = &source[body_start..];
    let body = &body[..body.find("\n    }\n").unwrap()];
    let clear = body.find("fonts.clear();").expect("config_changed clears the loaded faces");
    let bump = body.find("bump_face_epoch()").expect("config_changed bumps the face epoch");
    let reload = body.find("FontDatabase::with_font_dirs").expect("config_changed reloads");
    assert!(clear < bump && bump < reload, "clear, then bump, then the fallible reload");
}

/// A locator whose fallback lookup always fails.
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

/// A locator whose fallback lookup succeeds and finds nothing.
struct EmptyLocator;

impl crate::locator::FontLocator for EmptyLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<ParsedFont>> {
        Ok(Vec::new())
    }
    fn locate_fallback_for_codepoints(&self, _: &[char]) -> anyhow::Result<Vec<ParsedFont>> {
        Ok(Vec::new())
    }
}

/// One fallback request for `no_glyphs` through `locator` with empty font databases, at epoch 0.
fn fallback_request(
    no_glyphs: Vec<char>,
    locator: std::sync::Arc<dyn crate::locator::FontLocator + Send + Sync>,
    config: config::ConfigHandle,
    notice: std::sync::Arc<crate::FallbackNotice>,
) -> crate::FallbackResolveInfo {
    crate::FallbackResolveInfo {
        timing: None,
        no_glyphs,
        pending: Default::default(),
        completion: Box::new(|| {}),
        font_dirs: std::sync::Arc::new(crate::db::FontDatabase::new()),
        built_in: std::sync::Arc::new(crate::db::FontDatabase::new()),
        locator,
        config,
        notice,
        cancel: Default::default(),
        hooks: Default::default(),
        epoch: 0,
    }
}

#[test]
fn an_uncovered_codepoint_is_unresolved_only_when_every_stage_ran() {
    // U+0378 is unassigned and not private use, so no face covers it. With every stage succeeding
    // it is unresolved; with the locator failing it is a lookup failure and never unresolved.
    let config = config::ConfigHandle::new(config::Config::default());
    let notice = crate::FallbackNotice::new();
    fallback_request(
        vec!['\u{378}'],
        std::sync::Arc::new(EmptyLocator),
        config.clone(),
        notice.clone(),
    )
    .process();
    assert_eq!(notice.unresolved_snapshot().codepoints, vec!['\u{378}']);
    assert!(notice.lookup_failed().is_empty());

    let failed = crate::FallbackNotice::new();
    fallback_request(vec!['\u{378}'], std::sync::Arc::new(FailingLocator), config, failed.clone())
        .process();
    assert!(failed.unresolved_snapshot().codepoints.is_empty());
    assert_eq!(failed.lookup_failed(), vec!['\u{378}']);
    assert_eq!(failed.generation(), 0, "an unresolved-only result never bumps the face generation");
}

#[test]
fn a_reload_racing_the_worker_publishes_nothing_for_the_old_configuration() {
    // The worker pauses before publishing; a reload resets the notice to a newer epoch meanwhile,
    // so the result for epoch 0 is discarded and the revision does not move.
    let notice = crate::FallbackNotice::new();
    let reload = notice.clone();
    let mut request = fallback_request(
        vec!['\u{378}'],
        std::sync::Arc::new(EmptyLocator),
        config::ConfigHandle::new(config::Config::default()),
        notice.clone(),
    );
    request.hooks.before_unresolved_publish = Some(std::sync::Arc::new(move || reload.reset(1)));
    request.process();
    assert!(notice.unresolved_snapshot().codepoints.is_empty());
    assert_eq!(notice.unresolved_revision(), 0);
}

#[test]
fn a_real_reload_racing_the_worker_never_publishes_the_old_configurations_result() {
    // The worker pauses after selection, before publishing U+0378; `config_changed` runs; the
    // worker resumes. A later request in the new configuration publishes U+0379, and the worker
    // runs requests in order, so once it has, the old one is settled: only U+0379 is unresolved.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("reload-race", locator(&gate, &calls));
    let (arrived, release) = (Arc::new(Latch::default()), Arc::new(Latch::default()));
    fixture.configuration.set_fallback_worker_hooks_for_test(crate::FallbackWorkerHooks {
        before_unresolved_publish: Some(pause(&arrived, &release)),
        ..Default::default()
    });
    let font = fixture.configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, '\u{378}'), 0);
    arrived.wait();
    fixture.configuration.config_changed(&fixture.configuration.config()).unwrap();
    release.open();
    let reloaded = fixture.configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&reloaded, '\u{379}'), 0);
    let notice = fixture.configuration.fallback_notice();
    let started = Instant::now();
    while notice.unresolved_revision() == 0 {
        assert!(started.elapsed() < WORKER_WAIT, "the new request never published");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(notice.unresolved_snapshot().codepoints, vec!['\u{379}']);
}

/// A stand-in fontconfig that answers every configured family with the Rec Mono fixture, tagged
/// as a substitution for the requested name, as fontconfig does for a family it lacks.
struct SubstitutingLocator;

impl crate::locator::FontLocator for SubstitutingLocator {
    fn load_fonts(
        &self,
        fonts_selection: &[config::FontAttributes],
        loaded: &mut std::collections::HashSet<config::FontAttributes>,
        _pixel_size: u16,
    ) -> anyhow::Result<Vec<ParsedFont>> {
        let mut fonts = vec![];
        for attr in fonts_selection.iter().filter(|attr| !attr.is_fallback) {
            let handle = FontDataHandle {
                source: FontDataSource::OnDisk(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
                ),
                index: 0,
                variation: 0,
                origin: FontOrigin::FontConfigMatch(attr.family.clone()),
                coverage: None,
            };
            fonts.push(ParsedFont::from_locator(&handle)?);
            loaded.insert(attr.clone());
        }
        Ok(fonts)
    }

    fn locate_fallback_for_codepoints(
        &self,
        _codepoints: &[char],
    ) -> anyhow::Result<Vec<ParsedFont>> {
        Ok(vec![])
    }
}

/// fontconfig answers a family it lacks with its own best match tagged with the requested name,
/// so the name check accepts it. The resolver still reports that the configured family is not
/// installed and names the face used instead; a generic alias such as `monospace` is exempt.
#[test]
fn a_fontconfig_substitution_for_a_missing_family_is_reported() {
    const MISSING: &str = "SonicTerm Substituted Diagnostic Family";
    let reported = |family: &str| {
        crate::REPORTED_FONT_DIAGNOSTICS.lock().unwrap().iter().any(|message| {
            message.contains(&format!("{family:?} (weight="))
                && message.contains("Using \"Rec Mono St.Helens\" instead")
        })
    };
    let resolve = |family: &str| {
        let mut settings = config::Config::default();
        settings.font.font = vec![config::FontAttributes::new(family)];
        let settings = config::ConfigHandle::new(settings);
        let fonts = crate::FontConfigInner::with_locator(
            settings.clone(),
            72,
            Arc::new(SubstitutingLocator),
        )
        .unwrap();
        let (_, handles) = fonts.resolve_font_helper(&settings.font, &settings, 14).unwrap();
        handles.first().unwrap().names().family.clone()
    };
    assert_eq!(resolve(MISSING), "Rec Mono St.Helens");
    assert!(reported(MISSING), "the substitution is reported");
    assert_eq!(resolve("monospace"), "Rec Mono St.Helens");
    assert!(!reported("monospace"), "an alias resolving to another face is not a miss");
}
