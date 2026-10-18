use std::sync::{Arc, Mutex};

use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::{CommandPaletteMode, ConfirmRow};
use winit::keyboard::{Key, NamedKey};

use crate::app::App;
use crate::default_handlers::{
    DefaultHandlerBackend, HandlerItem, HandlerKind, HandlerOutcome, HandlerPlan,
    HandlerResultSink, PlanMode,
};

/// What the fake backend was asked to do.
#[derive(Default)]
struct Calls {
    plans: usize,
    applied: Vec<Vec<HandlerItem>>,
}

/// A backend that records every call and changes nothing on the host.
struct RecordingBackend {
    calls: Arc<Mutex<Calls>>,
    plan: Result<HandlerPlan, String>,
    /// Outcomes reported synchronously from `apply`, by item index.
    immediate: Vec<(usize, HandlerOutcome)>,
}

impl DefaultHandlerBackend for RecordingBackend {
    fn platform(&self) -> &'static str {
        "fake"
    }

    fn plan(&mut self) -> Result<HandlerPlan, String> {
        self.calls.lock().unwrap().plans += 1;
        self.plan.clone()
    }

    fn apply(&mut self, plan: &HandlerPlan, sink: HandlerResultSink) {
        self.calls.lock().unwrap().applied.push(plan.items.clone());
        for (index, outcome) in &self.immediate {
            sink.report(*index, outcome.clone());
        }
    }
}

/// A plan changing one content type and one scheme.
fn two_item_plan() -> HandlerPlan {
    HandlerPlan {
        mode: PlanMode::Change,
        items: vec![
            HandlerItem::new(HandlerKind::ContentType, "public.shell-script"),
            HandlerItem::new(HandlerKind::UrlScheme, "ssh"),
        ],
        confirm_notes: vec!["confirm note".to_string()],
        report_notes: vec!["report note".to_string()],
    }
}

type Posted = Arc<Mutex<Vec<(u64, usize, HandlerOutcome)>>>;

/// A headless app with one main tab, `backend`, and a poster that records results.
fn app_with(
    plan: Result<HandlerPlan, String>,
    immediate: Vec<(usize, HandlerOutcome)>,
) -> (App, Arc<Mutex<Calls>>, Posted) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    app.set_default_handler_backend(Box::new(RecordingBackend {
        calls: Arc::clone(&calls),
        plan,
        immediate,
    }));
    let posted: Posted = Arc::default();
    let store = Arc::clone(&posted);
    app.default_handlers.poster_override = Some(Arc::new(move |request, index, outcome| {
        store.lock().unwrap().push((request, index, outcome));
    }));
    (app, calls, posted)
}

/// Feed every recorded result back into the app, as the event loop would.
fn deliver(app: &mut App, posted: &Posted) {
    let results: Vec<_> = posted.lock().unwrap().drain(..).collect();
    for (request, index, outcome) in results {
        app.handle_default_handler_result(request, index, outcome);
    }
}

fn press(app: &mut App, key: NamedKey) {
    assert!(app.command_palette_handle_logical_key(&Key::Named(key)));
}

/// Contract: startup through the platform shell and both config-reload paths never ask
/// the backend for a plan or an apply; only the palette command does.
#[test]
fn startup_and_reload_never_call_the_backend() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let mut app = crate::shell::LinuxShell::new(
        sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default()),
        Theme::default(),
        Config::default(),
        Keymap::default(),
    )
    .with_default_handlers(Box::new(RecordingBackend {
        calls: Arc::clone(&calls),
        plan: Ok(two_item_plan()),
        immediate: Vec::new(),
    }))
    .into_app_for_test();
    app.__test_seed_tab("main");
    app.apply_new_config(Config::default());
    let scratch = tempfile::tempdir().unwrap();
    let config_path = scratch.path().join("sonicterm.toml");
    std::fs::write(&config_path, "").unwrap();
    app.runtime_config_path = Some(config_path);
    app.force_reload_config();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.plans, 0);
    assert!(calls.applied.is_empty());
    assert!(app.default_handlers.backend.is_some(), "the shell installed the backend");
}

/// Contract: selecting the command shows Apply, Cancel and every affected item before
/// anything changes, with Cancel highlighted so a stray Enter changes nothing.
#[test]
fn selecting_the_command_shows_the_list_and_changes_nothing() {
    let (mut app, calls, _) = app_with(Ok(two_item_plan()), Vec::new());
    assert!(app.run_action(&Action::SetDefaultHandlers));
    assert_eq!(app.command_palette.mode(), CommandPaletteMode::Confirm);
    assert!(app.command_palette.is_open());
    let rows: Vec<_> = app.command_palette.confirm_rows().to_vec();
    assert_eq!(
        rows,
        vec![
            ConfirmRow::Apply("Apply".to_string()),
            ConfirmRow::Cancel("Cancel".to_string()),
            ConfirmRow::Item("Content type public.shell-script".to_string()),
            ConfirmRow::Item("URL scheme ssh:".to_string()),
            ConfirmRow::Item("confirm note".to_string()),
        ]
    );
    assert_eq!(app.command_palette.confirm_title(), "SonicTerm will become the default for:");
    assert!(!app.command_palette.confirm_applies());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.plans, 1);
    assert!(calls.applied.is_empty());
}

/// Contract: Escape, and Enter on the initially highlighted Cancel row, close the page
/// and make zero apply calls.
#[test]
fn cancel_makes_no_backend_call() {
    let (mut app, calls, _) = app_with(Ok(two_item_plan()), Vec::new());
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::Escape);
    assert!(!app.command_palette.is_open());
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::Enter);
    assert!(!app.command_palette.is_open());
    assert!(app.default_handlers.pending.is_none());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.plans, 2);
    assert!(calls.applied.is_empty());
}

/// Contract: Apply makes exactly one apply call covering every listed item, and the
/// report appears once all items have answered.
#[test]
fn apply_calls_the_backend_once_and_reports_every_item() {
    let (mut app, calls, posted) = app_with(Ok(two_item_plan()), Vec::new());
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::ArrowUp);
    assert!(app.command_palette.confirm_applies());
    press(&mut app, NamedKey::Enter);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.applied, vec![two_item_plan().items]);
    }
    assert_eq!(app.__test_main_notification_message(), Some("Applying default handlers…"));
    let request = app.default_handlers.next_request;
    posted.lock().unwrap().push((request, 0, HandlerOutcome::Changed));
    deliver(&mut app, &posted);
    assert!(app.default_handlers.active.is_some(), "one item is still outstanding");
    posted.lock().unwrap().push((request, 1, HandlerOutcome::Denied));
    deliver(&mut app, &posted);
    assert!(app.default_handlers.active.is_none());
    let message = app.__test_main_notification_message().unwrap();
    assert!(message.starts_with("Partly applied: 1 of 2 changed."), "{message}");
    assert!(message.contains("URL scheme ssh:: denied"));
    assert!(message.ends_with("report note"));
}

/// Contract: outcomes a backend reports synchronously from `apply` still produce the report.
#[test]
fn synchronous_results_are_reported() {
    let (mut app, _, posted) = app_with(
        Ok(two_item_plan()),
        vec![(0, HandlerOutcome::Changed), (1, HandlerOutcome::Changed)],
    );
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::ArrowDown);
    press(&mut app, NamedKey::Enter);
    deliver(&mut app, &posted);
    let message = app.__test_main_notification_message().unwrap();
    assert!(message.starts_with("Defaults changed: all 2"), "{message}");
}

/// Contract: when the deadline passes first, missing items are reported as pending and a
/// later result for that run changes nothing.
#[test]
fn deadline_reports_missing_items_as_pending_and_ignores_late_results() {
    let (mut app, _, posted) = app_with(Ok(two_item_plan()), vec![(0, HandlerOutcome::Changed)]);
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::ArrowUp);
    press(&mut app, NamedKey::Enter);
    deliver(&mut app, &posted);
    let request = app.default_handlers.next_request;
    app.handle_default_handler_deadline(request + 1);
    assert!(app.default_handlers.active.is_some(), "another run's deadline is ignored");
    app.handle_default_handler_deadline(request);
    let message = app.__test_main_notification_message().unwrap().to_string();
    assert!(message.contains("URL scheme ssh:: pending"), "{message}");
    app.handle_default_handler_result(request, 1, HandlerOutcome::Changed);
    assert_eq!(app.__test_main_notification_message(), Some(message.as_str()));
}

/// Contract: a backend refusal (missing bundle, utility or entry) is shown and no
/// confirmation page opens.
#[test]
fn refusal_is_reported_without_a_confirmation_page() {
    let (mut app, calls, _) =
        app_with(Err("Move SonicTerm to Applications first.".into()), Vec::new());
    app.run_action(&Action::SetDefaultHandlers);
    assert!(!app.command_palette.is_open());
    assert_eq!(
        app.__test_main_notification_message(),
        Some("Move SonicTerm to Applications first.")
    );
    assert!(calls.lock().unwrap().applied.is_empty());
}

/// Contract: a build without a backend says the command is unavailable and opens nothing.
#[test]
fn no_backend_reports_unavailable() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    app.run_action(&Action::SetDefaultHandlers);
    assert!(!app.command_palette.is_open());
    assert_eq!(
        app.__test_main_notification_message(),
        Some("Setting default handlers is not available in this build.")
    );
}

/// Contract: while a run waits for results, the command refuses to start another.
#[test]
fn a_second_run_waits_for_the_first() {
    let (mut app, calls, _) = app_with(Ok(two_item_plan()), Vec::new());
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::ArrowUp);
    press(&mut app, NamedKey::Enter);
    app.run_action(&Action::SetDefaultHandlers);
    assert!(!app.command_palette.is_open());
    assert_eq!(calls.lock().unwrap().plans, 1);
    assert!(app.__test_main_notification_message().unwrap().starts_with("Still waiting"));
}

/// Contract: Apply only acts on the confirmation shown in the current palette session;
/// a page replaced by another palette mode applies nothing.
#[test]
fn a_stale_confirmation_applies_nothing() {
    let (mut app, calls, _) = app_with(Ok(two_item_plan()), Vec::new());
    app.run_action(&Action::SetDefaultHandlers);
    app.command_palette.start_confirm("other", "Apply", "Cancel", Vec::new());
    app.command_palette.move_selection_up();
    press(&mut app, NamedKey::Enter);
    assert!(calls.lock().unwrap().applied.is_empty());
}

/// Contract: without a poster or event loop, Apply refuses rather than starting a run
/// whose results could never be reported.
#[test]
fn apply_without_a_result_path_changes_nothing() {
    let (mut app, calls, _) = app_with(Ok(two_item_plan()), Vec::new());
    app.default_handlers.poster_override = None;
    app.run_action(&Action::SetDefaultHandlers);
    press(&mut app, NamedKey::ArrowUp);
    press(&mut app, NamedKey::Enter);
    assert!(calls.lock().unwrap().applied.is_empty());
    assert!(app
        .__test_main_notification_message()
        .unwrap()
        .starts_with("Default handlers were not"));
}

/// Contract: selecting the palette entry routes through the same confirmation page.
#[test]
fn the_palette_entry_opens_the_confirmation_page() {
    let (mut app, calls, _) = app_with(Ok(two_item_plan()), Vec::new());
    let main = app.main_window_id.unwrap();
    app.run_action_for_window(&Action::OpenCommandPalette, main);
    app.command_palette.set_query("default handler");
    let position = app
        .command_palette
        .visible()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                sonicterm_ui::command_palette::PaletteEntry::Command(Action::SetDefaultHandlers)
            )
        })
        .expect("the command is searchable by its keywords");
    assert!(app.command_palette.select_visible_index(position));
    press(&mut app, NamedKey::Enter);
    assert_eq!(app.command_palette.mode(), CommandPaletteMode::Confirm);
    assert_eq!(calls.lock().unwrap().plans, 1);
    assert!(calls.lock().unwrap().applied.is_empty());
}
