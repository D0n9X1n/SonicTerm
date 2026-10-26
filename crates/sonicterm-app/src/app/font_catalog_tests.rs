use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use winit::keyboard::{Key, NamedKey};

use super::*;

/// An app whose loaded config asks for `family`, reading config from a path that does not exist.
fn app_with_family(family: &str) -> App {
    let mut config = Config::default();
    config.font.family = family.into();
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let scratch = std::env::temp_dir().join(format!("sonicterm-catalog-{}", std::process::id()));
    app.runtime_config_path = Some(scratch.join("absent/sonicterm.toml"));
    app
}

/// The worker never holds its caller: `spawn` returns while enumeration is still blocked, and the
/// report arrives only once enumeration finishes.
#[test]
fn enumeration_runs_after_spawn_returns() {
    let (release, gate) = mpsc::channel::<()>();
    let (report_tx, report_rx) = mpsc::channel();
    spawn(
        move || {
            gate.recv_timeout(Duration::from_secs(10)).ok();
            Ok(vec!["Menlo".to_string()])
        },
        move |report| report_tx.send(report).unwrap(),
    );
    assert!(report_rx.try_recv().is_err(), "no report before enumeration finishes");
    release.send(()).unwrap();
    let report = report_rx.recv_timeout(Duration::from_secs(10)).expect("the worker reports");
    assert_eq!(report, FontCatalogReport { families: vec!["Menlo".into()] });
}

/// Only a config file that names `[font] family` counts as configured: no file, or one that sets
/// other font keys, leaves the family to the platform default.
#[test]
fn only_a_named_family_counts_as_configured() {
    let dir = std::env::temp_dir().join(format!("sonicterm-font-catalog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    assert!(!config_sets_family(&path), "a missing file names no family");
    std::fs::write(&path, "[font]\nsize = 14.0\n").unwrap();
    assert!(!config_sets_family(&path));
    std::fs::write(&path, "[font]\nfamily = \"Monaco\"\n").unwrap();
    assert!(config_sets_family(&path));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Enumerations the counting enumerator ran; only one test injects it.
static ENUMERATIONS: AtomicUsize = AtomicUsize::new(0);

/// Counts each call and lists one family.
fn counting_enumerator() -> anyhow::Result<Vec<String>> {
    ENUMERATIONS.fetch_add(1, Ordering::SeqCst);
    Ok(vec!["Menlo".to_string()])
}

/// Startup enumerates nothing. The first picker open starts one enumeration, and later opens reuse
/// the loading catalog instead of starting another.
#[test]
fn enumeration_waits_for_the_picker_and_runs_once() {
    let mut app = app_with_family("Menlo");
    app.font_enumerator = counting_enumerator;
    let child = app.__test_seed_child_window(&["work"]);
    app.adopt_resolved_font_family();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ENUMERATIONS.load(Ordering::SeqCst), 0, "startup enumerates nothing");
    assert_eq!(app.font_catalog, FontCatalog::Idle);
    for _ in 0..3 {
        assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
        app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while ENUMERATIONS.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ENUMERATIONS.load(Ordering::SeqCst), 1, "one enumeration across three opens");
    assert_eq!(app.font_catalog, FontCatalog::Loading);
}

/// A config that names no family, on a host without the platform default, adopts the face that
/// stood in for it as the live family and baseline, in memory only: no config file is written.
#[test]
fn an_unconfigured_start_adopts_the_resolved_face_in_memory() {
    let mut app = app_with_family("Cascadia Mono");
    let generation = app.font_family_generation;
    app.adopt_resolved_family_from("Cascadia Mono", Some("Consolas".into()), false, generation);
    assert_eq!(app.config.font.family, "Consolas");
    assert_eq!(app.configured_font_family, "Consolas");
    assert!(!app.runtime_config_path.as_ref().unwrap().exists(), "adoption writes no file");
}

/// The resolved face never replaces a family the config names, a non-default family, a family the
/// session already changed, or a family a reload set after the reading was taken.
#[test]
fn an_explicit_changed_or_newer_family_is_kept() {
    let resolved = || Some("Consolas".to_string());
    let mut configured = app_with_family("Cascadia Mono");
    let generation = configured.font_family_generation;
    configured.adopt_resolved_family_from("Cascadia Mono", resolved(), true, generation);
    assert_eq!(configured.config.font.family, "Cascadia Mono");

    let mut other = app_with_family("Monaco");
    let generation = other.font_family_generation;
    other.adopt_resolved_family_from("Cascadia Mono", resolved(), false, generation);
    assert_eq!(other.config.font.family, "Monaco");

    let mut changed = app_with_family("Cascadia Mono");
    changed.config.font.family = "Monaco".into();
    let generation = changed.font_family_generation;
    changed.adopt_resolved_family_from("Cascadia Mono", resolved(), false, generation);
    assert_eq!(changed.config.font.family, "Monaco");

    let mut reloaded = app_with_family("Cascadia Mono");
    let before = reloaded.font_family_generation;
    let mut config = Config::default();
    config.font.family = "Cascadia Mono".into();
    reloaded.apply_new_config(config);
    reloaded.adopt_resolved_family_from("Cascadia Mono", resolved(), false, before);
    assert_eq!(reloaded.config.font.family, "Cascadia Mono");
}

/// How long the gated enumerator waits to be released, so a failing test cannot hang on it.
const GATE_TIMEOUT: Duration = Duration::from_secs(5);

/// The gated enumerator's gate: it signals the first sender once enumeration has started, then
/// waits on the receiver. Only the shutdown test installs it.
static GATE: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = Mutex::new(None);

/// An enumerator held inside enumeration until the test releases its gate, or `GATE_TIMEOUT` ends.
fn gated_enumerator() -> anyhow::Result<Vec<String>> {
    let gate = GATE.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        // A timeout means the test failed before releasing the gate; enumeration ends anyway.
        let _ = release.recv_timeout(GATE_TIMEOUT);
    }
    Ok(vec!["Menlo".to_owned(), "Monaco".to_owned()])
}

/// Releases the gate when dropped, so the worker ends on every path out of the test.
struct Release(mpsc::Sender<()>);

// Lifecycle: dropping the guard releases a waiting worker and clears a gate no worker took.
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
        GATE.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

/// Shutdown never waits on enumeration: with the worker held inside it, `finish_session` returns
/// while the worker still runs. The worker ends once released, and its report, delivered after
/// shutdown, changes nothing.
#[test]
fn a_report_after_finish_session_changes_nothing() {
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *GATE.lock().unwrap_or_else(PoisonError::into_inner) = Some((entered_tx, release_rx));
    let release = Release(release_tx);
    let mut app = app_with_family("Menlo");
    app.font_enumerator = gated_enumerator;
    let child = app.__test_seed_child_window(&["work"]);
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    entered.recv_timeout(GATE_TIMEOUT).expect("the worker entered enumeration");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    app.finish_session();
    let worker = app.font_catalog_worker.take().expect("the picker started a worker");
    assert!(!worker.is_finished(), "finish_session waited for the held enumeration");
    drop(release);
    let deadline = Instant::now() + GATE_TIMEOUT;
    while !worker.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(worker.is_finished(), "the released worker ends");
    worker.join().expect("the worker ends without panicking");
    // A unit test runs no event loop, so the report is delivered here, as the event loop would.
    app.adopt_font_catalog(vec!["Menlo".into(), "Monaco".into()]);
    assert_eq!(app.font_catalog, FontCatalog::Loading);
    assert_eq!(app.config.font.family, "Menlo");
}
