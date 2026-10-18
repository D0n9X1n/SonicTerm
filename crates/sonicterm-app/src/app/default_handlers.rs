//! The default-handler command inside the app: confirmation page, run, and report.
//!
//! [`App::start_default_handlers`] is reached only from `Action::SetDefaultHandlers`.
//! It asks the installed backend for a plan and shows it on a palette
//! confirmation page; only Apply on that same page calls the backend's `apply`.
//! Results arrive as `UserEvent::DefaultHandlerResult` and are reported once,
//! through the existing notification, when every item has answered or the
//! deadline has passed.

use std::sync::{Arc, Mutex};

use winit::window::WindowId;

use crate::app::{App, FrontmostKind, UserEvent};
use crate::default_handlers::{
    DefaultHandlerBackend, HandlerOutcome, HandlerPlan, HandlerResultSink, HandlerRun, PlanMode,
    ResultPoster, RESULT_DEADLINE,
};
use sonicterm_ui::overlays::NotificationLevel;

/// A plan shown on the confirmation page, waiting for Apply or Cancel.
pub(super) struct PendingConfirmation {
    /// The palette session that shows it; Apply from any other session is ignored.
    session: u64,
    /// The window the command came from, which receives the report.
    window: Option<WindowId>,
    /// What Apply acts on.
    plan: HandlerPlan,
}

/// A plan being applied, waiting for each item's outcome.
pub(super) struct ActiveRun {
    run: HandlerRun,
    window: Option<WindowId>,
}

/// Everything the default-handler command keeps between events.
#[derive(Default)]
pub(in crate::app) struct DefaultHandlersState {
    /// The platform backend; `None` in builds without one, such as tests and the perf harness.
    pub(super) backend: Option<Box<dyn DefaultHandlerBackend>>,
    pub(super) pending: Option<PendingConfirmation>,
    pub(super) active: Option<ActiveRun>,
    /// Last request id handed out; results carry it so stale ones are ignored.
    pub(super) next_request: u64,
    /// Replaces the event-loop poster in tests, which have no event loop.
    pub(super) poster_override: Option<ResultPoster>,
}

impl App {
    /// Install the platform backend the default-handler command uses.
    pub(crate) fn set_default_handler_backend(&mut self, backend: Box<dyn DefaultHandlerBackend>) {
        self.default_handlers.backend = Some(backend);
    }

    /// The notification target for `window`, or the frontmost window when it has closed.
    fn default_handlers_kind(&self, window: Option<WindowId>) -> FrontmostKind {
        match window.filter(|id| self.windows.contains_key(id)) {
            Some(id) => self.kind_for(id),
            None => self.frontmost_kind(),
        }
    }

    fn default_handlers_platform(&self) -> &'static str {
        self.default_handlers.backend.as_ref().map_or("none", |backend| backend.platform())
    }

    /// Run the palette command: show what would change, and change nothing yet.
    pub(super) fn start_default_handlers(&mut self) {
        let window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => self.main_window_id,
        };
        let kind = self.default_handlers_kind(window);
        tracing::info!(
            target: "sonicterm_app::default_handlers",
            platform = self.default_handlers_platform(),
            "default handlers command invoked"
        );
        if self.default_handlers.active.is_some() {
            // When: `default_handlers.active` holds an earlier run, a second run would mix the two reports.
            self.show_notification_for_kind(
                kind,
                NotificationLevel::Warning,
                "Still waiting for the previous default-handler change to finish.".to_string(),
            );
            return;
        }
        let Some(backend) = self.default_handlers.backend.as_mut() else {
            // When: no backend is installed, this build cannot change defaults at all.
            self.show_notification_for_kind(
                kind,
                NotificationLevel::Warning,
                "Setting default handlers is not available in this build.".to_string(),
            );
            return;
        };
        let platform = backend.platform();
        let plan = match backend.plan() {
            Ok(plan) => plan,
            Err(reason) => {
                // When: the backend refused (missing bundle, utility or entry), nothing is shown to apply.
                tracing::info!(
                    target: "sonicterm_app::default_handlers",
                    platform,
                    "default handlers refused before confirmation"
                );
                self.show_notification_for_kind(kind, NotificationLevel::Warning, reason);
                return;
            }
        };
        let (title_key, apply_key) = match plan.mode {
            PlanMode::Change => ("default-handlers-title-change", "default-handlers-apply"),
            PlanMode::OpenSettings => {
                ("default-handlers-title-windows", "default-handlers-open-settings")
            }
        };
        let items: Vec<String> = plan
            .items
            .iter()
            .map(|item| item.describe())
            .chain(plan.confirm_notes.iter().cloned())
            .collect();
        self.palette_pointer_capture = None;
        self.command_palette.start_confirm(
            self.i18n.translate(title_key),
            self.i18n.translate(apply_key),
            self.i18n.translate("default-handlers-cancel"),
            items,
        );
        self.palette_attached_window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        self.default_handlers.pending =
            Some(PendingConfirmation { session: self.command_palette.session(), window, plan });
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    /// Close the confirmation page; `apply` is true only for Apply on that page.
    pub(super) fn confirm_default_handlers(&mut self, apply: bool) {
        let session = self.command_palette.session();
        let source = self.palette_attached_window.or(self.main_window_id);
        self.command_palette.close();
        self.palette_attached_window = None;
        self.request_redraw_for_overlay(source);
        let pending =
            self.default_handlers.pending.take().filter(|pending| pending.session == session);
        let Some(pending) = pending else {
            // When: `pending` belongs to no other `session`, so there is nothing to apply.
            return;
        };
        if !apply {
            // When: `apply` is false (Cancel or Escape), nothing may change.
            tracing::info!(
                target: "sonicterm_app::default_handlers",
                platform = self.default_handlers_platform(),
                "default handlers cancelled at confirmation"
            );
            return;
        }
        let kind = self.default_handlers_kind(pending.window);
        let poster = self.default_handlers.poster_override.clone().or_else(|| {
            self.event_loop_proxy.clone().map(|proxy| {
                let proxy = Mutex::new(proxy);
                let poster: ResultPoster = Arc::new(move |request, index, outcome| {
                    if let Ok(proxy) = proxy.lock() {
                        // When: the lock is held, a closed event loop only drops this late result.
                        let _ = proxy.send_event(UserEvent::DefaultHandlerResult {
                            request,
                            index,
                            outcome,
                        });
                    }
                });
                poster
            })
        });
        let (Some(poster), Some(backend)) = (poster, self.default_handlers.backend.as_mut()) else {
            // When: no poster or backend exists, results could never be reported, so do not start.
            self.show_notification_for_kind(
                kind,
                NotificationLevel::Error,
                "Default handlers were not changed: results could not be reported.".to_string(),
            );
            return;
        };
        self.default_handlers.next_request += 1;
        let request = self.default_handlers.next_request;
        tracing::info!(
            target: "sonicterm_app::default_handlers",
            platform = backend.platform(),
            items = pending.plan.items.len(),
            "default handlers apply started"
        );
        self.default_handlers.active = Some(ActiveRun {
            run: HandlerRun::new(request, pending.plan.clone()),
            window: pending.window,
        });
        backend.apply(&pending.plan, HandlerResultSink::new(request, poster));
        if let Some(proxy) = self.event_loop_proxy.clone() {
            // When: `event_loop_proxy` exists, a detached timer bounds how long the report waits.
            let spawned = std::thread::Builder::new()
                .name("sonicterm-default-handlers-deadline".to_string())
                .spawn(move || {
                    std::thread::sleep(RESULT_DEADLINE);
                    let _ = proxy.send_event(UserEvent::DefaultHandlerDeadline { request });
                });
            if let Err(error) = spawned {
                tracing::warn!(
                    target: "sonicterm_app::default_handlers",
                    %error,
                    "default handlers deadline timer did not start"
                );
            }
        }
        if self.default_handlers.active.as_ref().is_some_and(|active| active.run.is_complete()) {
            // An empty plan, or results reported synchronously, leave nothing to wait for.
            self.finish_default_handler_run();
        } else {
            // When: `is_complete` is false, results are outstanding; say so until the report replaces it.
            self.show_notification_for_kind(
                kind,
                NotificationLevel::Info,
                "Applying default handlers…".to_string(),
            );
        }
    }

    /// Record one item's outcome; report when the run is complete.
    pub(super) fn handle_default_handler_result(
        &mut self,
        request: u64,
        index: usize,
        outcome: HandlerOutcome,
    ) {
        let platform = self.default_handlers_platform();
        let Some(active) =
            self.default_handlers.active.as_mut().filter(|active| active.run.request() == request)
        else {
            // When: the run already reported (deadline) or never existed, a late result changes nothing.
            tracing::debug!(
                target: "sonicterm_app::default_handlers",
                request,
                index,
                outcome = %outcome.label(),
                "late default handler result ignored"
            );
            return;
        };
        if let Some(item) = active.run.plan().items.get(index) {
            tracing::debug!(
                target: "sonicterm_app::default_handlers",
                platform,
                item = %item.name,
                outcome = %outcome.label(),
                "default handler item finished"
            );
            if let HandlerOutcome::Failed(detail) = &outcome {
                // A failed item's error domain and code go to the log as well as the report.
                tracing::warn!(
                    target: "sonicterm_app::default_handlers",
                    platform,
                    item = %item.name,
                    error = %detail,
                    "default handler item failed"
                );
            }
        }
        let _ = active.run.record(index, outcome);
        if active.run.is_complete() {
            self.finish_default_handler_run();
        }
    }

    /// Report a run whose deadline passed; its missing items show as pending.
    pub(super) fn handle_default_handler_deadline(&mut self, request: u64) {
        if self
            .default_handlers
            .active
            .as_ref()
            .is_some_and(|active| active.run.request() == request)
        {
            // The run that set this deadline is still waiting; report it now with pending items.
            self.finish_default_handler_run();
        }
    }

    /// Show the one report for the active run and forget it.
    fn finish_default_handler_run(&mut self) {
        let Some(active) = self.default_handlers.active.take() else {
            // When: no run is active, there is nothing to report.
            return;
        };
        let report = active.run.report();
        let counts = report.counts;
        tracing::info!(
            target: "sonicterm_app::default_handlers",
            platform = self.default_handlers_platform(),
            changed = counts.changed,
            opened = counts.opened,
            denied = counts.denied,
            cancelled = counts.cancelled,
            failed = counts.failed,
            pending = counts.pending,
            "default handlers finished"
        );
        let kind = self.default_handlers_kind(active.window);
        self.show_notification_for_kind(kind, report.level, report.text);
    }
}

#[cfg(test)]
#[path = "default_handlers_tests.rs"]
mod default_handlers_tests;
