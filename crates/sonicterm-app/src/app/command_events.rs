//! Shell-integration command events: bounded per-pane queues, tab status polling, and
//! long-command notifications.

use super::*;

/// Append parsed command events, bounding both the entries kept and the
/// memory the queue holds.
///
/// Trimming the length is not enough on its own. `Vec::drain` lowers the
/// length and keeps the allocation, so one oversized batch — a 64 KiB parse
/// chunk of `OSC 133` prompt markers is roughly eight thousand events — leaves
/// the queue trimmed to the cap while still holding the peak buffer for as
/// long as the pane lives. Releasing the overshoot keeps the memory the class
/// records and the memory the pane holds the same figure.
///
/// The release runs only when a batch actually overshot, so the steady state,
/// where the queue sits at or below the cap, does not reallocate.
pub(super) fn append_bounded_command_events(
    queue: &mut Vec<PaneCommandEvent>,
    events: impl IntoIterator<Item = PaneCommandEvent>,
) {
    queue.extend(events);
    if queue.len() > MAX_PANE_COMMAND_EVENTS {
        let excess = queue.len() - MAX_PANE_COMMAND_EVENTS;
        queue.drain(0..excess);
        queue.shrink_to(MAX_PANE_COMMAND_EVENTS);
    }
}

impl App {
    /// Refresh every main-window tab's command status from its panes.
    ///
    /// Every tab is polled, not just the active one, so a background tab's
    /// badge reflects work that finished while it was hidden.
    #[doc(hidden)]
    pub fn poll_command_events_for_all_tabs(&mut self) {
        let n = self.main_tab_states().map(|ts| ts.len()).unwrap_or(0);
        for tab_idx in 0..n {
            self.poll_command_events_for_tab(tab_idx);
        }
    }

    pub(super) fn poll_command_events_for_tab(&mut self, tab_idx: usize) {
        let Some(id) = self.main_window_id else {
            // When: `main_window_id` is unset, so no tab bar exists yet to carry
            // the status this poll would produce.
            return;
        };
        let Some(ws) = self.windows.get_mut(&id) else {
            // When: `id` no longer resolves in `windows`, so the state this poll
            // would write into is already gone.
            return;
        };
        poll_command_events_for_tab_state(
            &ws.panes,
            &mut ws.tab_states,
            &mut ws.tabs,
            &self.config,
            tab_idx,
        );
    }
}

/// Drain one tab's pane command events into its command status and tab badge.
///
/// Events are collected across every leaf pane in the tab, so a command that
/// finished in a non-focused split still updates the tab. A finished command
/// holds its badge for a few seconds before the status lapses.
#[doc(hidden)]
pub fn poll_command_events_for_tab_state(
    panes: &HashMap<u64, PaneState>,
    tab_states: &mut [TabState],
    tabs: &mut TabBar,
    config: &Config,
    tab_idx: usize,
) {
    let Some(tab_state) = tab_states.get_mut(tab_idx) else {
        // When: `get_mut` cannot resolve `tab_idx`, so nothing exists to receive
        // the drained events and the panes are left holding them.
        return;
    };
    let pane_ids = tab_state.tree.leaves();
    let mut events = Vec::new();
    for pane_id in pane_ids {
        if let Some(pane) = panes.get(&pane_id) {
            let mut q = pane.command_events.lock();
            events.extend(q.drain(..));
        }
    }
    if events.is_empty() {
        // When: no pane produced `events`, so the existing status and badge
        // already describe the tab and republishing would only churn.
        return;
    }
    for ev in events {
        match ev.event {
            CommandEvent::CmdStart => tab_state.command = CommandStatus::Running(ev.at),
            CommandEvent::CmdEnd(exit) => {
                tab_state.command =
                    CommandStatus::Done { exit, until: ev.at + Duration::from_secs(3) };
                maybe_notify_long_command(config, ev.duration, exit);
            }
            CommandEvent::PromptStart | CommandEvent::PromptEnd => {
                // When: PromptStart or PromptEnd arrives, no command execution begins; preserve the running/done status.
            }
        }
    }
    if let Some(t) = tab_states.get(tab_idx).map(|st| st.command.clone()) {
        tabs.set_command_status(tab_idx, t);
    }
}

/// Refresh every tab of a torn-out child window from its panes.
///
/// A child runs its own tab bar, so it polls independently of the main window
/// rather than inheriting the main window's sweep.
#[doc(hidden)]
pub fn poll_command_events_for_child_window(child: &mut WindowState, config: &Config) {
    for tab_idx in 0..child.tab_states.len() {
        poll_command_events_for_tab_state(
            &child.panes,
            &mut child.tab_states,
            &mut child.tabs,
            config,
            tab_idx,
        );
    }
}

fn maybe_notify_long_command(config: &Config, duration: Option<Duration>, exit: Option<u8>) {
    let Some(duration) = duration else {
        // When: the event carries no `duration`, so elapsed time cannot be
        // compared against the threshold that makes a command "long".
        return;
    };
    if !config.notifications.long_command {
        // When: `config` disables long_command notifications, so a finished
        // command stays silent however long it ran.
        return;
    }
    if duration.as_secs() <= config.notifications.threshold_secs {
        // When: `duration` sits within threshold_secs, so the user was not
        // waiting long enough for a desktop interruption to be welcome.
        return;
    }
    let result = match exit {
        Some(0) => "completed successfully",
        Some(code) => {
            // When: `code` is nonzero, so the message names the failure rather
            // than the generic completion wording built below.
            return notify_command_done(format!("Command failed with exit code {code}"));
        }
        None => "completed",
    };
    notify_command_done(format!("Command {result} after {}s", duration.as_secs()));
}

#[cfg(target_os = "windows")]
pub(super) fn notify_command_done(body: String) {
    if let Err(err) = notify_rust::Notification::new().summary("Command done").body(&body).show() {
        tracing::debug!(target: "sonicterm_app::app", ?err, "desktop notification failed");
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) fn notify_command_done(_body: String) {}

#[cfg(test)]
#[path = "command_events_tests.rs"]
mod command_events_tests;
