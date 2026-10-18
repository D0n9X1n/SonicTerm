use super::*;
use std::sync::Mutex;

/// A change plan for two content types and one scheme.
fn change_plan() -> HandlerPlan {
    HandlerPlan {
        mode: PlanMode::Change,
        items: vec![
            HandlerItem::new(HandlerKind::ContentType, "public.shell-script"),
            HandlerItem::new(HandlerKind::ContentType, "public.unix-executable"),
            HandlerItem::new(HandlerKind::UrlScheme, "ssh"),
        ],
        confirm_notes: Vec::new(),
        report_notes: vec!["note".to_string()],
    }
}

/// A run of `plan` with `outcomes` recorded in item order.
fn run_with(plan: HandlerPlan, outcomes: Vec<HandlerOutcome>) -> HandlerRun {
    let mut run = HandlerRun::new(7, plan);
    for (index, outcome) in outcomes.into_iter().enumerate() {
        assert!(run.record(index, outcome));
    }
    run
}

/// Contract: every item described names its kind and identifier, so the confirmation
/// page and the report show exactly what changes.
#[test]
fn items_describe_their_kind_and_name() {
    assert_eq!(
        HandlerItem::new(HandlerKind::ContentType, "public.shell-script").describe(),
        "Content type public.shell-script"
    );
    assert_eq!(HandlerItem::new(HandlerKind::UrlScheme, "ssh").describe(), "URL scheme ssh:");
    assert_eq!(
        HandlerItem::new(HandlerKind::MimeType, "text/x-shellscript").describe(),
        "MIME type text/x-shellscript"
    );
    assert_eq!(
        HandlerItem::new(HandlerKind::SettingsPage, "Default apps").describe(),
        "Settings page Default apps"
    );
}

/// Contract: all items changed reports success at Info level with one line per item and the notes.
#[test]
fn all_changed_reports_success() {
    let run = run_with(change_plan(), vec![HandlerOutcome::Changed; 3]);
    assert!(run.is_complete());
    let report = run.report();
    assert_eq!(report.level, NotificationLevel::Info);
    assert_eq!(report.counts.changed, 3);
    assert_eq!(
        report.text,
        "Defaults changed: all 3 now open in SonicTerm.\n\
         • Content type public.shell-script: changed\n\
         • Content type public.unix-executable: changed\n\
         • URL scheme ssh:: changed\n\
         note"
    );
}

/// Contract: a mix of changed, denied and failed is reported as partial, with each item's
/// own outcome and the error domain and code of the failure.
#[test]
fn partial_success_is_reported_as_partial() {
    let run = run_with(
        change_plan(),
        vec![
            HandlerOutcome::Changed,
            HandlerOutcome::Denied,
            HandlerOutcome::Failed("NSOSStatusErrorDomain -10814".to_string()),
        ],
    );
    let report = run.report();
    assert_eq!(report.level, NotificationLevel::Warning);
    assert!(report.text.starts_with("Partly applied: 1 of 3 changed."));
    assert!(report.text.contains("public.unix-executable: denied"));
    assert!(report.text.contains("ssh:: failed (NSOSStatusErrorDomain -10814)"));
    assert_eq!(
        report.counts,
        HandlerCounts { changed: 1, denied: 1, failed: 1, ..HandlerCounts::default() }
    );
}

/// Contract: a declined or cancelled prompt is not an error: nothing changed, at Warning level.
#[test]
fn denied_and_cancelled_change_nothing_without_an_error() {
    let report = run_with(
        change_plan(),
        vec![HandlerOutcome::Denied, HandlerOutcome::Cancelled, HandlerOutcome::Denied],
    )
    .report();
    assert_eq!(report.level, NotificationLevel::Warning);
    assert!(report.text.starts_with("No defaults changed."));
    assert!(report.text.contains("public.unix-executable: cancelled"));
    assert_eq!(report.counts.denied, 2);
    assert_eq!(report.counts.cancelled, 1);
}

/// Contract: every item failing is an Error report.
#[test]
fn all_failed_is_an_error() {
    let report =
        run_with(change_plan(), vec![HandlerOutcome::Failed("domain 1".to_string()); 3]).report();
    assert_eq!(report.level, NotificationLevel::Error);
    assert!(report.text.starts_with("No defaults changed."));
}

/// Contract: an item that never reports shows as pending, never as changed, and the run is
/// incomplete until it does.
#[test]
fn a_missing_result_is_pending_not_changed() {
    let mut run = HandlerRun::new(3, change_plan());
    assert!(run.record(0, HandlerOutcome::Changed));
    assert!(run.record(2, HandlerOutcome::Changed));
    assert!(!run.is_complete());
    let report = run.report();
    assert_eq!(report.counts.pending, 1);
    assert_eq!(report.counts.changed, 2);
    assert_eq!(report.level, NotificationLevel::Warning);
    assert!(report.text.contains("public.unix-executable: pending"));
}

/// Contract: the first outcome for an item stands; repeats and unknown indexes are ignored.
#[test]
fn record_keeps_the_first_outcome_and_rejects_unknown_items() {
    let mut run = HandlerRun::new(1, change_plan());
    assert!(run.record(1, HandlerOutcome::Denied));
    assert!(!run.record(1, HandlerOutcome::Changed));
    assert!(!run.record(9, HandlerOutcome::Changed));
    assert_eq!(run.results()[1].1, HandlerOutcome::Denied);
    assert_eq!(run.request(), 1);
    assert_eq!(run.plan().items.len(), 3);
}

/// Contract: a settings-page plan reports the page it opened, or that it could not open one.
#[test]
fn settings_plans_report_the_page_or_its_failure() {
    let plan = HandlerPlan {
        mode: PlanMode::OpenSettings,
        items: vec![HandlerItem::new(HandlerKind::SettingsPage, "Default apps")],
        confirm_notes: Vec::new(),
        report_notes: vec!["Choose SonicTerm for .sh.".to_string()],
    };
    let opened = run_with(plan.clone(), vec![HandlerOutcome::Opened("SonicTerm page".into())]);
    let report = opened.report();
    assert_eq!(report.level, NotificationLevel::Info);
    assert!(report.text.starts_with("Opened Settings › Default apps."));
    assert!(report.text.contains("opened (SonicTerm page)"));
    assert!(report.text.ends_with("Choose SonicTerm for .sh."));
    let failed = run_with(plan, vec![HandlerOutcome::Failed("launch refused".into())]).report();
    assert_eq!(failed.level, NotificationLevel::Error);
    assert!(failed.text.starts_with("Could not open Settings › Default apps."));
}

/// Contract: a sink tags each outcome with its request before handing it on, from any thread.
#[test]
fn sink_tags_outcomes_with_the_request() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&received);
    let sink = HandlerResultSink::new(
        42,
        Arc::new(move |request, index, outcome| {
            store.lock().unwrap().push((request, index, outcome));
        }),
    );
    let worker_sink = sink.clone();
    std::thread::spawn(move || worker_sink.report(1, HandlerOutcome::Cancelled)).join().unwrap();
    sink.report(0, HandlerOutcome::Changed);
    assert_eq!(
        *received.lock().unwrap(),
        vec![(42, 1, HandlerOutcome::Cancelled), (42, 0, HandlerOutcome::Changed)]
    );
    assert!(format!("{sink:?}").contains("42"));
}

/// Contract: each outcome has the label the issue's report wording requires.
#[test]
fn outcome_labels_match_the_report_wording() {
    assert_eq!(HandlerOutcome::Changed.label(), "changed");
    assert_eq!(HandlerOutcome::Denied.label(), "denied");
    assert_eq!(HandlerOutcome::Cancelled.label(), "cancelled");
    assert_eq!(HandlerOutcome::Pending.label(), "pending");
    assert_eq!(HandlerOutcome::Failed("domain 3".into()).label(), "failed (domain 3)");
    assert_eq!(HandlerOutcome::Opened("page".into()).label(), "opened (page)");
}
