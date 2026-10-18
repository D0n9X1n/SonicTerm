use super::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The WiX manifest the MSI is built from.
const WIX_MANIFEST: &str = include_str!("../wix/main.wxs");
/// This module's own source, scanned for registry-writing APIs.
const BACKEND_SOURCE: &str = include_str!("default_handlers.rs");

type Reported = Arc<Mutex<Vec<(usize, HandlerOutcome)>>>;

/// A sink that records each outcome.
fn recording_sink() -> (HandlerResultSink, Reported) {
    let reported: Reported = Arc::default();
    let store = Arc::clone(&reported);
    let sink = HandlerResultSink::new(
        1,
        Arc::new(move |_, index, outcome| store.lock().unwrap().push((index, outcome))),
    );
    (sink, reported)
}

/// A launcher that records each URI and answers from `results` in order.
fn scripted_launcher(
    results: Vec<Result<(), String>>,
) -> (SettingsLauncher, Arc<Mutex<Vec<String>>>) {
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let store = Arc::clone(&opened);
    let answers = Mutex::new(results.into_iter());
    let launcher: SettingsLauncher = Arc::new(move |uri| {
        store.lock().unwrap().push(uri.to_string());
        answers.lock().unwrap().next().unwrap_or(Ok(()))
    });
    (launcher, opened)
}

/// Contract: the per-app URI is exact and names the HKLM registration the MSI writes, and the
/// listed extensions are exactly the ones the MSI registers.
#[test]
fn uris_and_extensions_match_the_msi_registration() {
    assert_eq!(per_app_settings_uri(), "ms-settings:defaultapps?registeredAppMachine=SonicTerm");
    assert_eq!(DEFAULT_APPS_URI, "ms-settings:defaultapps");
    assert!(WIX_MANIFEST
        .contains(&format!("Key='Software\\RegisteredApplications' Name='{REGISTERED_APP_NAME}'")));
    for extension in SCRIPT_EXTENSIONS {
        assert!(
            WIX_MANIFEST.contains(&format!("Capabilities\\FileAssociations' Name='{extension}'")),
            "{extension}"
        );
    }
}

/// Contract: the plan only opens Settings, and lists each extension to choose SonicTerm for.
#[test]
fn plan_opens_settings_and_lists_the_extensions() {
    let (launcher, opened) = scripted_launcher(Vec::new());
    let mut backend = WindowsDefaultHandlers::new(launcher);
    let plan = backend.plan().unwrap();
    assert_eq!(backend.platform(), "windows");
    assert_eq!(plan.mode, PlanMode::OpenSettings);
    assert_eq!(plan.items, vec![HandlerItem::new(HandlerKind::SettingsPage, "Default apps")]);
    assert_eq!(
        plan.confirm_notes,
        vec![".ps1 → SonicTerm", ".cmd → SonicTerm", ".bat → SonicTerm", ".sh → SonicTerm"]
    );
    assert!(plan.report_notes[0].contains("drafts"));
    assert!(opened.lock().unwrap().is_empty(), "planning opens nothing");
}

/// Contract: the per-app page is tried first and reported when it opens.
#[test]
fn the_per_app_page_is_preferred() {
    let (launcher, opened) = scripted_launcher(vec![Ok(())]);
    let (sink, reported) = recording_sink();
    open_default_apps(&launcher, &sink);
    assert_eq!(*opened.lock().unwrap(), vec![per_app_settings_uri()]);
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Opened("SonicTerm's Default apps page".into()))]
    );
}

/// Contract: when the per-app page fails the general page opens; when both fail the report
/// carries both errors, and its notes still give the steps as text.
#[test]
fn the_general_page_is_the_fallback_and_failure_keeps_the_steps() {
    let (launcher, opened) = scripted_launcher(vec![Err("first".into()), Ok(())]);
    let (sink, reported) = recording_sink();
    open_default_apps(&launcher, &sink);
    assert_eq!(*opened.lock().unwrap(), vec![per_app_settings_uri(), DEFAULT_APPS_URI.into()]);
    assert_eq!(*reported.lock().unwrap(), vec![(0, HandlerOutcome::Opened("Default apps".into()))]);

    let (launcher, _) = scripted_launcher(vec![Err("first".into()), Err("second".into())]);
    let (sink, reported) = recording_sink();
    open_default_apps(&launcher, &sink);
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Failed("first; second".into()))]
    );
    assert!(choice_instructions()[1].contains("Settings › Apps › Default apps"));
}

/// Contract: Apply opens Settings on a worker thread and reports through the sink.
#[test]
fn apply_reports_from_a_worker() {
    let (launcher, _) = scripted_launcher(vec![Ok(())]);
    let mut backend = WindowsDefaultHandlers::new(launcher);
    let plan = backend.plan().unwrap();
    let (sink, reported) = recording_sink();
    backend.apply(&plan, sink);
    let deadline = Instant::now() + Duration::from_secs(10);
    while reported.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the worker never reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(reported.lock().unwrap()[0], (0, HandlerOutcome::Opened(_))));
}

/// Contract: no registry-writing API, and no `UserChoice` key, is reachable from this backend.
#[test]
fn no_registry_write_is_reachable() {
    let code: String = BACKEND_SOURCE
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in ["Registry", "RegSet", "RegCreate", "RegDelete", "SHSetValue", "UserChoice"] {
        assert!(!code.contains(forbidden), "{forbidden} appears in the backend");
    }
}
