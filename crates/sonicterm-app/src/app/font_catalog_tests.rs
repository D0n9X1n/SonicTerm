use std::sync::mpsc;
use std::time::Duration;

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

use super::*;

/// An app whose loaded config asks for `family`.
fn app_with_family(family: &str) -> App {
    let mut config = Config::default();
    config.font.family = family.into();
    App::new(Theme::default(), config, Keymap::default())
}

/// The worker never holds its caller: `spawn` returns while enumeration is still blocked, and the
/// report arrives only once enumeration finishes, so the first frame never waits for fonts.
#[test]
fn enumeration_runs_after_spawn_returns() {
    let (release, gate) = mpsc::channel::<()>();
    let (report_tx, report_rx) = mpsc::channel();
    spawn(
        None,
        move || {
            gate.recv_timeout(Duration::from_secs(10)).ok();
            Ok(vec!["Menlo".to_string()])
        },
        move |report| report_tx.send(report).unwrap(),
    );
    assert!(report_rx.try_recv().is_err(), "no report before enumeration finishes");
    release.send(()).unwrap();
    let report = report_rx.recv_timeout(Duration::from_secs(10)).expect("the worker reports");
    assert_eq!(
        report,
        FontCatalogReport { families: vec!["Menlo".into()], family_configured: false }
    );
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

/// The first platform default the host lists wins, in its installed spelling; an alias that names
/// no installed family, such as fontconfig's `monospace`, picks nothing.
#[test]
fn the_first_installed_platform_default_wins() {
    let installed = vec!["consolas".to_string(), "Menlo".to_string()];
    assert_eq!(
        first_installed(&["Cascadia Mono", "Consolas"], &installed),
        Some("consolas".into())
    );
    assert_eq!(first_installed(&["Menlo"], &installed), Some("Menlo".into()));
    assert_eq!(first_installed(&["monospace"], &installed), None);
}

/// A start whose config names no family, on a host without the first default, takes the next
/// installed default as its live family and baseline in memory: no config file is written. The
/// catalog keeps the whole list for the picker.
#[test]
fn an_unconfigured_start_adopts_the_first_installed_default_in_memory() {
    let mut app = app_with_family("Cascadia Mono");
    let dir = std::env::temp_dir().join(format!("sonicterm-font-adopt-{}", std::process::id()));
    let path = dir.join("sonicterm.toml");
    app.runtime_config_path = Some(path.clone());
    let installed = vec!["Consolas".to_string(), "Fira Code".to_string()];
    app.adopt_font_catalog_from(
        &["Cascadia Mono", "Consolas"],
        installed.clone(),
        false,
        app.font_family_generation,
    );
    assert_eq!(app.config.font.family, "Consolas");
    assert_eq!(app.configured_font_family, "Consolas");
    assert_eq!(app.font_catalog, FontCatalog::Ready(installed));
    assert!(!path.exists(), "adoption writes no config file");
}

/// A family the config names, or one the session already switched to, is never replaced; the
/// catalog still becomes ready for the picker.
#[test]
fn a_configured_or_changed_family_is_kept() {
    let installed = vec!["Consolas".to_string()];
    let candidates = ["Cascadia Mono", "Consolas"];
    let mut configured = app_with_family("Cascadia Mono");
    configured.adopt_font_catalog_from(
        &candidates,
        installed.clone(),
        true,
        configured.font_family_generation,
    );
    assert_eq!(configured.config.font.family, "Cascadia Mono");
    assert_eq!(configured.font_catalog, FontCatalog::Ready(installed.clone()));

    let mut changed = app_with_family("Cascadia Mono");
    changed.config.font.family = "Monaco".into();
    changed.adopt_font_catalog_from(
        &candidates,
        installed.clone(),
        false,
        changed.font_family_generation,
    );
    assert_eq!(changed.config.font.family, "Monaco");
    assert_eq!(changed.configured_font_family, "Cascadia Mono");
    assert_eq!(changed.font_catalog, FontCatalog::Ready(installed));
}

/// A config built in memory, as the perf harness builds one, never reaches the file the worker
/// reads; its non-default family must survive even though that file names none, or the live font
/// changes under a running session and every pane refits.
#[test]
fn a_non_default_family_survives_an_unconfigured_report() {
    let installed = vec!["Cascadia Mono".to_string(), "Consolas".to_string()];
    let mut app = app_with_family("Consolas");
    app.adopt_font_catalog_from(
        &["Cascadia Mono", "Consolas"],
        installed.clone(),
        false,
        app.font_family_generation,
    );
    assert_eq!(app.config.font.family, "Consolas");
    assert_eq!(app.configured_font_family, "Consolas");
    assert_eq!(app.font_catalog, FontCatalog::Ready(installed));
}

/// A report that lands after the event loop ended is dropped quietly. The delivery here sends on a
/// channel whose receiver is gone, as `EventLoopProxy::send_event` does once the loop exits
/// (`EventLoopClosed` on every winit backend), and the worker still finishes without a panic.
#[test]
fn a_report_after_shutdown_is_dropped_quietly() {
    let (loop_tx, loop_rx) = mpsc::channel::<FontCatalogReport>();
    drop(loop_rx);
    let (done_tx, done_rx) = mpsc::channel();
    spawn(
        None,
        || Ok(vec!["Menlo".to_string()]),
        move |report| {
            done_tx.send(loop_tx.send(report).is_err()).unwrap();
        },
    );
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(10)), Ok(true));
}

/// A reload that names the platform default itself, applied while the worker that read no family
/// still enumerates, wins over that worker's stale report: the reloaded family is kept.
#[test]
fn a_report_older_than_a_reload_keeps_the_reloaded_family() {
    let mut app = app_with_family("Cascadia Mono");
    let spawned_at = app.font_family_generation;
    let mut reloaded = Config::default();
    reloaded.font.family = "Cascadia Mono".into();
    app.apply_new_config(reloaded);
    let installed = vec!["Consolas".to_string()];
    app.adopt_font_catalog_from(
        &["Cascadia Mono", "Consolas"],
        installed.clone(),
        false,
        spawned_at,
    );
    assert_eq!(app.config.font.family, "Cascadia Mono");
    assert_eq!(app.font_catalog, FontCatalog::Ready(installed));
}
