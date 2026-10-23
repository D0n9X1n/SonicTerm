//! Program-reported state outside the grid: `OSC 9;4` progress mirrored to the platform progress
//! surfaces, and OSC 9 / OSC 777 notifications shown as one coalescing toast.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use sonicterm_cfg::config::Osc9Delivery;
use sonicterm_ui::status_line::{
    ellipsize_middle_bytes, sanitize_display_text, SanitizeLimits, SanitizePolicy,
};
use winit::window::WindowId;

use super::{App, FrontmostKind};
use crate::taskbar_progress::{ProgressReport, ProgressSurface, TaskbarProgressBackend};

/// At most one delivery per surface per interval; the last value of a burst lands at its end.
pub(super) const PROGRESS_DELIVERY_INTERVAL: Duration = Duration::from_millis(250);
/// How long a program toast stays up.
pub(super) const TOAST_DURATION: Duration = Duration::from_secs(5);
/// A notification from any pane this soon after a toast appears joins it.
const TOAST_ANY_PANE_WINDOW: Duration = Duration::from_secs(1);
/// Deliveries the perf harness's counting surface keeps; older ones are dropped first.
const RECORDED_DELIVERIES_MAX: usize = 1024;
/// What the perf harness's counting surface received: when, and the percent or `None` for a clear.
type RecordedDeliveries = std::rc::Rc<std::cell::RefCell<Vec<(Instant, Option<u8>)>>>;
/// The `+N more` count stops here and reads `+99 more`.
pub(super) const TOAST_MAX_MORE: u32 = 99;
/// Display caps after sanitizing, middle-ellipsized on a grapheme boundary.
const TITLE_MAX_BYTES: usize = 256;
const BODY_MAX_BYTES: usize = 1024;

/// The toast on screen. It keeps the first notification and a count, never the coalesced text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProgramToast {
    /// The window showing it.
    pub(super) window_id: WindowId,
    /// The pane whose notification opened it.
    pub(super) pane_id: u64,
    /// When it appeared.
    pub(super) shown_at: Instant,
    /// When it closes.
    pub(super) expires_at: Instant,
    /// The origin cue, `⟨tab N⟩`, whose brackets the sanitizer removes from all program text.
    pub(super) origin: String,
    /// The first notification's sanitized title; empty for OSC 9.
    pub(super) title: String,
    /// The first notification's sanitized body.
    pub(super) body: String,
    /// Notifications coalesced into it, saturating at [`TOAST_MAX_MORE`].
    pub(super) more: u32,
    /// The bubble text last shown, so a replaced bubble is not mistaken for this toast.
    pub(super) message: String,
}

/// What one progress surface wants and what it last received.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Slot {
    desired: Option<ProgressReport>,
    delivered: Option<ProgressReport>,
}

/// The App's program-reported state.
#[derive(Default)]
pub(crate) struct ProgramState {
    /// Each pane's active report and the update sequence that set it; the newest wins app-wide.
    reports: HashMap<u64, (ProgressReport, u64)>,
    next_sequence: u64,
    slots: HashMap<ProgressSurface, Slot>,
    /// Each live surface's last delivery, kept across a clear until its interval passes, so a set
    /// right after a clear still waits for the rate limit.
    last_delivery: HashMap<ProgressSurface, Instant>,
    /// Values delivered to any surface, cumulative; the frame counters read its growth.
    delivered: u64,
    pub(super) toast: Option<ProgramToast>,
    pub(super) backend: Option<Box<dyn TaskbarProgressBackend>>,
    /// The perf harness's counting surface's record, when it is installed.
    recorded: Option<RecordedDeliveries>,
}

impl std::fmt::Debug for ProgramState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgramState")
            .field("reports", &self.reports.len())
            .field("slots", &self.slots)
            .field("toast", &self.toast)
            .field("backend", &self.backend.is_some())
            .finish()
    }
}

/// One terminal window as progress selection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WindowProgress {
    /// The native window handle, when the platform has one.
    pub(super) native: Option<isize>,
    /// The focused pane of its active tab.
    pub(super) active_pane: Option<u64>,
    /// Every pane it holds.
    pub(super) panes: Vec<u64>,
}

/// What every live surface should show. Per window (Windows): each window's focused pane.
/// App-wide: the most recently updated active report in any terminal window, so the next most
/// recent shows when it ends, and nothing once none is left or no terminal window remains.
pub(super) fn desired_progress(
    enabled: bool,
    per_window: bool,
    windows: &[WindowProgress],
    reports: &HashMap<u64, (ProgressReport, u64)>,
) -> HashMap<ProgressSurface, Option<ProgressReport>> {
    let report_of = |pane: &u64| reports.get(pane).filter(|_| enabled).copied();
    if per_window {
        // When: per_window, each window's taskbar button shows its own focused pane.
        return windows
            .iter()
            .filter_map(|window| {
                let native = window.native?;
                let report = window.active_pane.as_ref().and_then(report_of);
                Some((ProgressSurface::Window(native), report.map(|(report, _)| report)))
            })
            .collect();
    }
    let newest = windows
        .iter()
        .flat_map(|window| window.panes.iter())
        .filter_map(report_of)
        .max_by_key(|(_, sequence)| *sequence)
        .map(|(report, _)| report);
    HashMap::from([(ProgressSurface::App, newest)])
}

impl ProgramState {
    /// Record `pane_id`'s report, or forget it on `None`.
    pub(super) fn note_progress(&mut self, pane_id: u64, report: Option<ProgressReport>) {
        match report {
            Some(report) => {
                self.next_sequence += 1;
                self.reports.insert(pane_id, (report, self.next_sequence));
            }
            None => {
                self.reports.remove(&pane_id);
            }
        }
    }

    /// Whether nothing is reported and nothing is shown, so a sync has no work.
    fn idle(&self) -> bool {
        self.reports.is_empty() && self.slots.is_empty()
    }

    /// Apply `desired` at `now` and deliver every surface whose value changed and whose interval
    /// passed (`immediate` skips the interval, for a disabling reload). A surface absent from
    /// `desired` belonged to a closed window and is dropped undelivered. Returns the next
    /// trailing-edge delivery.
    pub(super) fn deliver_due(
        &mut self,
        desired: HashMap<ProgressSurface, Option<ProgressReport>>,
        now: Instant,
        immediate: bool,
        backend: &mut dyn TaskbarProgressBackend,
    ) -> Option<Instant> {
        self.slots.retain(|surface, _| desired.contains_key(surface));
        self.last_delivery.retain(|surface, at| {
            desired.contains_key(surface) && now < *at + PROGRESS_DELIVERY_INTERVAL
        });
        for (surface, report) in desired {
            self.slots.entry(surface).or_default().desired = report;
        }
        let mut next = None::<Instant>;
        for (surface, slot) in &mut self.slots {
            if slot.desired == slot.delivered {
                // When: the surface already shows desired, there is nothing to send.
                continue;
            }
            let ready_at = match self.last_delivery.get(surface) {
                Some(at) if !immediate => *at + PROGRESS_DELIVERY_INTERVAL,
                _ => now,
            };
            if now < ready_at {
                // When: now is before ready_at, the newest value waits for its trailing edge.
                next = Some(next.map_or(ready_at, |next| next.min(ready_at)));
                continue;
            }
            backend.deliver(*surface, slot.desired);
            self.delivered += 1;
            slot.delivered = slot.desired;
            self.last_delivery.insert(*surface, now);
        }
        // A surface that shows nothing and wants nothing needs no slot; its last delivery time
        // stays in `last_delivery` until the interval passes.
        self.slots.retain(|_, slot| slot.desired.is_some() || slot.delivered.is_some());
        next
    }
}

/// Whether a notification should also raise a desktop notification: only `desktop` on Windows,
/// and only while its window is not focused.
pub(super) fn wants_desktop(delivery: Osc9Delivery, on_windows: bool, focused: bool) -> bool {
    delivery == Osc9Delivery::Desktop && on_windows && !focused
}

/// `text` through the shared program-text sanitizer, then capped at `max_bytes`.
fn sanitized(text: &str, max_bytes: usize) -> String {
    let limits = SanitizeLimits {
        max_bytes: sonicterm_vt::vt::MAX_NOTIFY_BYTES,
        max_columns: usize::MAX,
        collapse_whitespace: true,
    };
    ellipsize_middle_bytes(&sanitize_display_text(text, SanitizePolicy::Program, limits), max_bytes)
}

/// Whether a notification keeps any text after the program-text sanitizer, so it can be shown.
pub(super) fn has_visible_text(title: Option<&str>, body: &str) -> bool {
    !sanitized(body, BODY_MAX_BYTES).is_empty()
        || title.is_some_and(|title| !sanitized(title, TITLE_MAX_BYTES).is_empty())
}

impl App {
    /// Install the platform progress surface; without one, progress reports reach no surface.
    pub(crate) fn set_taskbar_progress_backend(
        &mut self,
        backend: Box<dyn TaskbarProgressBackend>,
    ) {
        self.program.backend = Some(backend);
    }

    /// Perf-harness seam: install a progress surface that shows nothing, so a run without a
    /// platform shell still syncs, counts and records every delivery. It is app-wide on every
    /// platform, so a window without a native handle still records on Windows; per-window
    /// selection has its own unit test. [`Self::__take_taskbar_progress_deliveries`] reads it.
    #[doc(hidden)]
    pub fn __install_counting_taskbar_progress(&mut self) {
        /// Records every value it is asked to show and shows none.
        struct Counting(RecordedDeliveries);
        impl TaskbarProgressBackend for Counting {
            fn per_window(&self) -> bool {
                false
            }

            fn deliver(&mut self, _surface: ProgressSurface, report: Option<ProgressReport>) {
                let mut recorded = self.0.borrow_mut();
                // A full record drops its oldest delivery to make room for this one.
                if recorded.len() == RECORDED_DELIVERIES_MAX {
                    recorded.remove(0);
                }
                recorded.push((Instant::now(), report.map(|report| report.percent)));
            }
        }
        let recorded = RecordedDeliveries::default();
        self.program.recorded = Some(std::rc::Rc::clone(&recorded));
        self.program.backend = Some(Box::new(Counting(recorded)));
    }

    /// Perf-harness seam: take what the counting surface received since the last take, in order,
    /// as (when, percent or `None` for a clear); empty when it is not installed.
    #[doc(hidden)]
    pub fn __take_taskbar_progress_deliveries(&mut self) -> Vec<(Instant, Option<u8>)> {
        self.program.recorded.as_ref().map(|recorded| recorded.take()).unwrap_or_default()
    }

    /// Apply one program-state event a VT worker sent; any other event is ignored.
    pub(super) fn handle_program_event(&mut self, event: super::UserEvent) {
        match event {
            super::UserEvent::ProgramProgress { pane_id, report } => {
                self.note_program_progress(pane_id, report);
            }
            super::UserEvent::ProgramNotify { pane_id, title, body, more } => {
                let now = self.dispatch_now();
                self.handle_program_notify(pane_id, title.as_deref(), &body, more, now);
            }
            _ => {
                // When: event is not a program-state event, the event loop routes it elsewhere.
            }
        }
    }

    /// Record a pane's progress change from its VT worker.
    pub(super) fn note_program_progress(&mut self, pane_id: u64, report: Option<ProgressReport>) {
        self.program.note_progress(pane_id, report);
        self.note_status_line(|counters| counters.program_progress += 1);
    }

    /// Bring every platform progress surface to what it should show, rate-limited per surface.
    /// Returns the next trailing-edge delivery, if one is waiting.
    pub(super) fn sync_taskbar_progress(&mut self, now: Instant) -> Option<Instant> {
        if self.program.idle() || self.program.backend.is_none() {
            // When: nothing is reported or shown, or no backend exists, there is nothing to do.
            return None;
        }
        let live: HashSet<u64> =
            self.windows.values().flat_map(|window| window.panes.keys().copied()).collect();
        self.program.reports.retain(|pane_id, _| live.contains(pane_id));
        let windows: Vec<WindowProgress> = self
            .windows
            .values()
            .filter(|window| window.role == super::WindowRole::Terminal)
            .map(|window| WindowProgress {
                native: window.window.as_deref().and_then(native_handle),
                active_pane: window
                    .tab_states
                    .get(window.tabs.active_index())
                    .map(|tab| tab.active_pane),
                panes: window.panes.keys().copied().collect(),
            })
            .collect();
        let enabled = self.config.notifications.taskbar_progress;
        let mut backend = self.program.backend.take()?;
        let desired =
            desired_progress(enabled, backend.per_window(), &windows, &self.program.reports);
        let delivered_before = self.program.delivered;
        let next = self.program.deliver_due(desired, now, !enabled, backend.as_mut());
        self.program.backend = Some(backend);
        let delivered = self.program.delivered - delivered_before;
        self.note_status_line(|counters| counters.taskbar_deliveries += delivered);
        next
    }

    /// Clear every surface that shows progress and release the backend, at App shutdown.
    pub(super) fn shutdown_taskbar_progress(&mut self) {
        let Some(mut backend) = self.program.backend.take() else {
            // When: no backend was installed, no surface ever showed anything.
            return;
        };
        for (surface, slot) in self.program.slots.drain() {
            if slot.delivered.is_some() {
                backend.deliver(surface, None);
            }
        }
        self.program.reports.clear();
        backend.shutdown();
    }

    /// Show a batch of program notifications from `pane_id`: the first one and `more` after it.
    pub(super) fn handle_program_notify(
        &mut self,
        pane_id: u64,
        title: Option<&str>,
        body: &str,
        more: u32,
        now: Instant,
    ) {
        self.note_status_line(|counters| counters.program_notifies += 1);
        let delivery = self.config.notifications.osc9;
        if delivery == Osc9Delivery::Off {
            // When: delivery is Osc9Delivery::Off, program notifications are ignored.
            return;
        }
        let title = title.map(|title| sanitized(title, TITLE_MAX_BYTES)).unwrap_or_default();
        let body = sanitized(body, BODY_MAX_BYTES);
        if title.is_empty() && body.is_empty() {
            // When: sanitizing left neither a title nor a body, there is nothing to show.
            return;
        }
        let Some((window_id, tab_index)) = self.windows.iter().find_map(|(id, window)| {
            let index =
                window.tab_states.iter().position(|tab| tab.tree.leaves().contains(&pane_id))?;
            Some((*id, index))
        }) else {
            // When: no window holds pane_id, the pane closed before its notification arrived.
            return;
        };
        self.mark_program_notify(window_id, tab_index);
        let coalesce = self.program.toast.as_ref().is_some_and(|toast| {
            let shown = now < toast.expires_at
                && self.windows.get(&toast.window_id).is_some_and(|window| {
                    window
                        .notification
                        .as_ref()
                        .is_some_and(|bubble| bubble.message == toast.message)
                });
            shown && (toast.pane_id == pane_id || now < toast.shown_at + TOAST_ANY_PANE_WINDOW)
        });
        if coalesce {
            // When: coalesce holds, the visible toast only counts this batch.
            self.coalesce_program_toast(more.saturating_add(1));
            self.note_status_line(|counters| counters.program_toast_redraws += 1);
            return;
        }
        // Every program notification opens with its tab's cue, so none reads as SonicTerm's own.
        let tab = (tab_index + 1).to_string();
        let origin = self.i18n.t_args("program-notify-tab", Some(&[("tab", tab.as_str())]));
        let more = more.min(TOAST_MAX_MORE);
        if more > 0 {
            // A first batch can already hold coalesced notifications; only the count is logged.
            tracing::debug!(target: "sonicterm_app::app", pane_id, more = u64::from(more), "program notifications coalesced");
        }
        let message = self.program_toast_message(&origin, &title, &body, more);
        if let Some(previous) = self.program.toast.take() {
            // Only one program toast is visible: a stale one in another window closes.
            if let Some(window) = self.windows.get_mut(&previous.window_id) {
                if window
                    .notification
                    .as_ref()
                    .is_some_and(|bubble| bubble.message == previous.message)
                {
                    window.notification = None;
                    window.request_window_redraw();
                }
            }
        }
        let expires_at = now + TOAST_DURATION;
        let kind = if Some(window_id) == self.main_window_id {
            FrontmostKind::Main
        } else {
            // When: window_id is not the main window, the toast shows in that child window.
            FrontmostKind::Child(window_id)
        };
        self.show_notification_for_kind_until(
            kind,
            sonicterm_ui::overlays::NotificationLevel::Info,
            message.clone(),
            Some(expires_at),
        );
        // Showing the toast requested its window's redraw.
        self.note_status_line(|counters| counters.program_toast_redraws += 1);
        let focused = self.frontmost_window == Some(window_id);
        if wants_desktop(delivery, cfg!(windows), focused) {
            // When: wants_desktop holds, Windows also raises a desktop notification, its title
            // opening with the same origin cue.
            let mut desktop_title = origin.clone();
            if !title.is_empty() {
                desktop_title.push(' ');
                desktop_title.push_str(&title);
            }
            super::command_events::notify_program(&desktop_title, &body);
        }
        self.program.toast = Some(ProgramToast {
            window_id,
            pane_id,
            shown_at: now,
            expires_at,
            origin,
            title,
            body,
            more,
            message,
        });
    }

    /// Add `count` notifications to the visible toast's `+N more` and redraw it.
    fn coalesce_program_toast(&mut self, count: u32) {
        let Some(toast) = self.program.toast.as_ref() else {
            // When: no toast exists, there is nothing to add to.
            return;
        };
        let more = toast.more.saturating_add(count).min(TOAST_MAX_MORE);
        let message = self.program_toast_message(&toast.origin, &toast.title, &toast.body, more);
        let Some(toast) = self.program.toast.as_mut() else {
            // When: the toast vanished while the message was formatted, there is nothing to update.
            return;
        };
        toast.more = more;
        toast.message = message.clone();
        let window_id = toast.window_id;
        tracing::debug!(target: "sonicterm_app::app", pane_id = toast.pane_id, more = u64::from(more), "program notifications coalesced");
        if let Some(window) = self.windows.get_mut(&window_id) {
            if let Some(bubble) = window.notification.as_mut() {
                bubble.message = message;
            }
            window.request_window_redraw();
        }
    }

    /// The toast text: `⟨tab N⟩ title: body` (`⟨tab N⟩ body` without a title), and `+N more`
    /// when notifications were coalesced.
    fn program_toast_message(&self, origin: &str, title: &str, body: &str, more: u32) -> String {
        let mut message = origin.to_owned();
        let text = match (title.is_empty(), body.is_empty()) {
            (false, false) => format!("{title}: {body}"),
            (false, true) => title.to_owned(),
            _ => body.to_owned(),
        };
        message.push(' ');
        message.push_str(&text);
        if more > 0 {
            let count = more.to_string();
            message.push(' ');
            message.push_str(
                &self.i18n.t_args("program-notify-more", Some(&[("count", count.as_str())])),
            );
        }
        message
    }

    /// Mark the notified tab, when a shown strip lists `alerts` with the `notify` source.
    fn mark_program_notify(&mut self, window_id: WindowId, tab_index: usize) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: window_id closed, there is no tab to mark.
            return;
        };
        if !window.alert_settings.is_some_and(|settings| settings.notify) {
            // When: alert_settings has no notify source, notifications mark nothing.
            return;
        }
        if window.tabs.mark(tab_index, |marks| marks.notify = true) {
            window.request_window_redraw();
        }
    }
}

/// The platform's native handle for `window`: the HWND on Windows, `None` elsewhere.
fn native_handle(window: &winit::window::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "program_state_tests.rs"]
mod program_state_tests;
