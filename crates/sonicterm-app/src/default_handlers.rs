//! Choosing SonicTerm as the default handler for the file types and links it supports.
//!
//! One palette command reaches this module, and nothing else does: installing,
//! starting, or reloading configuration never asks a backend for anything. The
//! command asks the platform backend for a [`HandlerPlan`], shows it on a
//! confirmation page, and only Apply calls [`DefaultHandlerBackend::apply`].
//! Each planned item then reports one [`HandlerOutcome`] through a
//! [`HandlerResultSink`]; [`HandlerRun`] gathers them into one report, and an
//! item that never reports by the deadline is reported as pending, never as
//! changed.

use std::sync::Arc;
use std::time::Duration;

use sonicterm_ui::overlays::NotificationLevel;

/// How long a run waits for every item before reporting the missing ones as pending.
///
/// Consent prompts wait for the user, so this is generous; it only bounds the report.
pub const RESULT_DEADLINE: Duration = Duration::from_secs(120);

/// What kind of system association an item names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerKind {
    /// A macOS Uniform Type Identifier, such as `public.shell-script`.
    ContentType,
    /// A URL scheme, such as `ssh`.
    UrlScheme,
    /// A freedesktop MIME type, such as `application/x-shellscript`.
    MimeType,
    /// A system settings page the user finishes the choice in.
    SettingsPage,
}

/// One association the command changes, or one page it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerItem {
    /// What the name identifies.
    pub kind: HandlerKind,
    /// The identifier itself; it names a type or scheme, never user data.
    pub name: String,
}

impl HandlerItem {
    /// An item of `kind` named `name`.
    #[must_use]
    pub fn new(kind: HandlerKind, name: impl Into<String>) -> Self {
        Self { kind, name: name.into() }
    }

    /// The line the confirmation page and the report show for this item.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.kind {
            HandlerKind::ContentType => format!("Content type {}", self.name),
            HandlerKind::UrlScheme => format!("URL scheme {}:", self.name),
            HandlerKind::MimeType => format!("MIME type {}", self.name),
            HandlerKind::SettingsPage => format!("Settings page {}", self.name),
        }
    }
}

/// Whether Apply changes associations itself or only opens the system's own settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMode {
    /// SonicTerm changes each item after the user confirms.
    Change,
    /// SonicTerm opens a settings page; the user makes the change there.
    OpenSettings,
}

/// What Apply would do, shown in full before anything changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerPlan {
    /// Whether Apply changes associations or opens settings.
    pub mode: PlanMode,
    /// The items Apply acts on; each reports exactly one outcome.
    pub items: Vec<HandlerItem>,
    /// Extra lines the confirmation page shows below the items.
    pub confirm_notes: Vec<String>,
    /// Lines the report shows after the item outcomes.
    pub report_notes: Vec<String>,
}

/// The result one item reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// The system now opens this item with SonicTerm.
    Changed,
    /// The settings page opened; the detail names which one.
    Opened(String),
    /// The user declined the system's consent prompt.
    Denied,
    /// The request was cancelled before it finished.
    Cancelled,
    /// The system refused or the request failed; the detail carries the error domain and code.
    Failed(String),
    /// No result arrived before the deadline.
    Pending,
}

impl HandlerOutcome {
    /// The word or phrase the report shows for this outcome.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Changed => "changed".to_string(),
            Self::Opened(detail) => format!("opened ({detail})"),
            Self::Denied => "denied".to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::Failed(detail) => format!("failed ({detail})"),
            Self::Pending => "pending".to_string(),
        }
    }
}

/// Delivers one item's outcome for one request; production posts it to the event loop.
pub type ResultPoster = Arc<dyn Fn(u64, usize, HandlerOutcome) + Send + Sync>;

/// Where a backend reports each item's outcome; it may be used from any thread.
#[derive(Clone)]
pub struct HandlerResultSink {
    request: u64,
    poster: ResultPoster,
}

impl HandlerResultSink {
    /// A sink that tags every outcome with `request` before handing it to `poster`.
    #[must_use]
    pub fn new(request: u64, poster: ResultPoster) -> Self {
        Self { request, poster }
    }

    /// Report the outcome of the plan's item at `index`.
    pub fn report(&self, index: usize, outcome: HandlerOutcome) {
        (self.poster)(self.request, index, outcome);
    }
}

impl std::fmt::Debug for HandlerResultSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HandlerResultSink").field("request", &self.request).finish()
    }
}

/// The platform side of the command: what it can change, and changing it.
pub trait DefaultHandlerBackend {
    /// Which OS branch this is, for logs.
    fn platform(&self) -> &'static str;

    /// What Apply would change, or the reason nothing can be changed.
    ///
    /// Called only when the user selects the command; it must change nothing.
    fn plan(&mut self) -> Result<HandlerPlan, String>;

    /// Start acting on every item of `plan`, reporting each outcome through `sink`.
    ///
    /// Called only after the user chose Apply. It must return without waiting for
    /// the system: slow work runs on a worker or completes through a callback.
    fn apply(&mut self, plan: &HandlerPlan, sink: HandlerResultSink);
}

/// How many items ended in each outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HandlerCounts {
    /// Items now opened by SonicTerm.
    pub changed: usize,
    /// Settings pages opened.
    pub opened: usize,
    /// Items whose consent prompt was declined.
    pub denied: usize,
    /// Items cancelled before finishing.
    pub cancelled: usize,
    /// Items that failed.
    pub failed: usize,
    /// Items with no result by the deadline.
    pub pending: usize,
}

/// The one notification a finished run shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerReport {
    /// Info when everything succeeded, Error when nothing did, Warning otherwise.
    pub level: NotificationLevel,
    /// A one-line summary, one line per item, then the plan's report notes.
    pub text: String,
    /// The outcome tally, for logs and tests.
    pub counts: HandlerCounts,
}

/// One applied plan, collecting each item's outcome until all arrive or the deadline passes.
#[derive(Debug, Clone)]
pub struct HandlerRun {
    request: u64,
    plan: HandlerPlan,
    outcomes: Vec<Option<HandlerOutcome>>,
}

impl HandlerRun {
    /// A run of `plan` whose outcomes arrive tagged with `request`.
    #[must_use]
    pub fn new(request: u64, plan: HandlerPlan) -> Self {
        let outcomes = vec![None; plan.items.len()];
        Self { request, plan, outcomes }
    }

    /// The request id outcomes for this run carry.
    #[must_use]
    pub fn request(&self) -> u64 {
        self.request
    }

    /// The plan being applied.
    #[must_use]
    pub fn plan(&self) -> &HandlerPlan {
        &self.plan
    }

    /// Record the outcome for item `index`; false for an unknown index or a repeat.
    pub fn record(&mut self, index: usize, outcome: HandlerOutcome) -> bool {
        match self.outcomes.get_mut(index) {
            Some(slot @ None) => {
                *slot = Some(outcome);
                true
            }
            _ => {
                // An out-of-range or already reported `index` keeps the first answer.
                false
            }
        }
    }

    /// Whether every item has reported.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.outcomes.iter().all(Option::is_some)
    }

    /// Each item with its outcome; an item that has not reported is pending.
    #[must_use]
    pub fn results(&self) -> Vec<(&HandlerItem, HandlerOutcome)> {
        self.plan
            .items
            .iter()
            .zip(&self.outcomes)
            .map(|(item, outcome)| (item, outcome.clone().unwrap_or(HandlerOutcome::Pending)))
            .collect()
    }

    /// Build the report from what has arrived; missing items count as pending.
    #[must_use]
    pub fn report(&self) -> HandlerReport {
        let results = self.results();
        let mut counts = HandlerCounts::default();
        for (_, outcome) in &results {
            match outcome {
                HandlerOutcome::Changed => counts.changed += 1,
                HandlerOutcome::Opened(_) => counts.opened += 1,
                HandlerOutcome::Denied => counts.denied += 1,
                HandlerOutcome::Cancelled => counts.cancelled += 1,
                HandlerOutcome::Failed(_) => counts.failed += 1,
                HandlerOutcome::Pending => counts.pending += 1,
            }
        }
        let total = results.len();
        let succeeded = counts.changed + counts.opened;
        let summary = match self.plan.mode {
            PlanMode::OpenSettings if counts.opened > 0 => {
                "Opened Settings › Default apps.".to_string()
            }
            PlanMode::OpenSettings => "Could not open Settings › Default apps.".to_string(),
            PlanMode::Change if total > 0 && counts.changed == total => {
                format!("Defaults changed: all {total} now open in SonicTerm.")
            }
            PlanMode::Change if counts.changed == 0 => "No defaults changed.".to_string(),
            PlanMode::Change => {
                format!("Partly applied: {} of {total} changed.", counts.changed)
            }
        };
        let level = if total > 0 && succeeded == total {
            NotificationLevel::Info
        } else if counts.failed == total {
            // When: every item failed, nothing the user asked for happened.
            NotificationLevel::Error
        } else {
            // When: some items were denied, cancelled, pending or failed, the result is partial.
            NotificationLevel::Warning
        };
        let mut lines = vec![summary];
        lines.extend(
            results
                .iter()
                .map(|(item, outcome)| format!("• {}: {}", item.describe(), outcome.label())),
        );
        lines.extend(self.plan.report_notes.iter().cloned());
        HandlerReport { level, text: lines.join("\n"), counts }
    }
}

#[cfg(test)]
#[path = "default_handlers_tests.rs"]
mod default_handlers_tests;
