use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
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

/// The worker the picker started is kept, and shutdown never waits for it. A report that lands
/// after `finish_session` is ignored without a panic and changes neither the catalog nor the font.
#[test]
fn a_report_after_finish_session_changes_nothing() {
    let mut app = app_with_family("Menlo");
    app.font_enumerator = || Ok(vec!["Menlo".to_string(), "Monaco".to_string()]);
    let child = app.__test_seed_child_window(&["work"]);
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    app.finish_session();
    let worker = app.font_catalog_worker.take().expect("the picker started a worker");
    worker.join().expect("the worker finishes on its own");
    app.adopt_font_catalog(vec!["Menlo".into(), "Monaco".into()]);
    assert_eq!(app.font_catalog, FontCatalog::Loading);
    assert_eq!(app.config.font.family, "Menlo");
}
