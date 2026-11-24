//! Shell-integration command events: bounded per-pane queues, tab status polling, and
//! long-command notifications.

use sonicterm_grid::grid::CmdlineText;
use sonicterm_vt::vt::SectionCommands;

use super::desktop_notify::DesktopNotice;
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
///
/// Dropping events marks the queue `overflowed`, so the drain resets the pane's command state
/// instead of applying a tail whose start is lost.
pub(super) fn append_bounded_command_events(
    queue: &mut CommandQueue,
    events: impl IntoIterator<Item = PaneCommandEvent>,
) {
    queue.events.extend(events);
    if queue.events.len() > MAX_PANE_COMMAND_EVENTS {
        let excess = queue.events.len() - MAX_PANE_COMMAND_EVENTS;
        queue.events.drain(0..excess);
        queue.events.shrink_to(MAX_PANE_COMMAND_EVENTS);
        queue.overflowed = true;
    }
}

impl App {
    /// Refresh every main-window tab's command status from its panes.
    ///
    /// Every tab is polled, not just the active one, so a background tab's
    /// badge reflects work that finished while it was hidden.
    #[doc(hidden)]
    pub fn poll_command_events_for_all_tabs(&mut self) {
        let tab_count = self.main_tab_states().map(|tab_states| tab_states.len()).unwrap_or(0);
        for tab_idx in 0..tab_count {
            self.poll_command_events_for_tab(tab_idx);
        }
    }

    pub(super) fn poll_command_events_for_tab(&mut self, tab_idx: usize) {
        let Some(id) = self.registry.main_window_id else {
            // When: `main_window_id` is unset, so no tab bar exists yet to carry
            // the status this poll would produce.
            return;
        };
        let Some(main) = self.registry.windows.get_mut(&id) else {
            // When: `id` no longer resolves in `windows`, so the state this poll
            // would write into is already gone.
            return;
        };
        let finished_min = main
            .alert_settings
            .filter(|settings| settings.finished)
            .map(|settings| Duration::from_secs(settings.min_secs));
        let _ = poll_command_events_for_tab_state(
            &mut main.panes,
            &mut main.tab_states,
            &mut main.tabs,
            &self.config,
            tab_idx,
            finished_min,
        );
    }
}

/// Drain one tab's pane command events into its command status and tab badge.
///
/// Events are collected across every leaf pane in the tab, so a command that
/// finished in a non-focused split still updates the tab. A finished command
/// holds its badge for a few seconds before the status lapses. Each pane's own command state
/// takes its events in parse order, after a reset when its queue overflowed. A command that ran
/// at least `finished_min` and ended while the tab was not active marks the tab for the alerts
/// segment; returns whether that mark appeared.
#[doc(hidden)]
pub fn poll_command_events_for_tab_state(
    panes: &mut crate::app::PaneMap,
    tab_states: &mut [TabState],
    tabs: &mut TabBar,
    config: &Config,
    tab_idx: usize,
    finished_min: Option<Duration>,
) -> bool {
    let Some(tab_state) = tab_states.get_mut(tab_idx) else {
        // When: `get_mut` cannot resolve `tab_idx`, so nothing exists to receive
        // the drained events and the panes are left holding them.
        return false;
    };
    let pane_ids = tab_state.tree.leaves();
    let mut events = Vec::new();
    for pane_id in pane_ids {
        if let Some(pane) = panes.get_mut(&pane_id) {
            let mut queue = pane.command_events.lock();
            if std::mem::take(&mut queue.overflowed) {
                // The queue overflowed: the lost events' state is unknown, so the retained tail starts fresh.
                pane.command_state.reset();
            }
            for event in queue.iter() {
                pane.command_state.apply(event);
            }
            events.extend(drain_paired(&mut queue));
        }
    }
    if events.is_empty() {
        // When: no pane produced `events`, so the existing status and badge
        // already describe the tab and republishing would only churn.
        return false;
    }
    let mut marked = false;
    for (pane_event, cmdline) in events {
        match pane_event.event {
            CommandEvent::CmdStart => tab_state.command = CommandStatus::Running(pane_event.at),
            CommandEvent::CmdEnd(exit) => {
                tab_state.command =
                    CommandStatus::Done { exit, until: pane_event.at + Duration::from_secs(3) };
                maybe_notify_long_command(config, pane_event.duration, exit, cmdline.as_ref());
                let ran = pane_event.duration.zip(finished_min);
                if let Some((duration, _)) = ran.filter(|(duration, min)| duration >= min) {
                    // `mark` refuses the active tab, so a command the user watched marks nothing.
                    let secs = duration.as_secs();
                    marked |= tabs.mark(tab_idx, |marks| marks.finished_secs = Some(secs));
                }
            }
            CommandEvent::PromptStart | CommandEvent::PromptEnd => {
                // When: PromptStart or PromptEnd arrives, no command execution begins; preserve the running/done status.
            }
        }
    }
    if let Some(command_status) = tab_states.get(tab_idx).map(|tab| tab.command.clone()) {
        tabs.set_command_status(tab_idx, command_status);
    }
    marked
}

/// Refresh every tab of a torn-out child window from its panes.
///
/// A child runs its own tab bar, so it polls independently of the main window
/// rather than inheriting the main window's sweep.
/// Returns whether a finished-command mark appeared.
#[doc(hidden)]
pub fn poll_command_events_for_child_window(child: &mut WindowState, config: &Config) -> bool {
    let finished_min = child
        .alert_settings
        .filter(|settings| settings.finished)
        .map(|settings| Duration::from_secs(settings.min_secs));
    let mut marked = false;
    for tab_idx in 0..child.tab_states.len() {
        marked |= poll_command_events_for_tab_state(
            &mut child.panes,
            &mut child.tab_states,
            &mut child.tabs,
            config,
            tab_idx,
            finished_min,
        );
    }
    marked
}

/// Keep `queue`'s one command-text slot in step with `events`, in parse order. Only the section's
/// last C holds text: its own primary-screen text from `section` (taken under the parser lock);
/// an earlier C holds none. A D after a run of a second or more stamps the held text with its
/// instant for the drain, and any other D drops an unstamped text. A section with an
/// alternate-screen C or D holds no text and drops an unstamped one, whose owner is ambiguous.
/// Only a run that long can pass the notification threshold, so short commands hold no text.
pub(super) fn track_cmdline(
    queue: &mut CommandQueue,
    events: &[PaneCommandEvent],
    section: SectionCommands,
) {
    if section.alternate && matches!(queue.cmdline, Some((_, None))) {
        // An alternate-screen section makes an unstamped held text's owner ambiguous, so it goes.
        queue.cmdline = None;
    }
    let mut cmdline = section.cmdline.filter(|_| !section.alternate);
    let mut starts_left =
        events.iter().filter(|event| matches!(event.event, CommandEvent::CmdStart)).count();
    for event in events {
        match event.event {
            CommandEvent::CmdStart => {
                starts_left -= 1;
                // Only the section's last C carried the recorded text.
                let text = (starts_left == 0).then(|| cmdline.take()).flatten();
                queue.cmdline = text.map(|text| (text, None));
            }
            CommandEvent::CmdEnd(_) => {
                let unstamped = matches!(queue.cmdline, Some((_, None)));
                let long = event.duration.is_some_and(|ran| ran >= Duration::from_secs(1));
                if let Some((_, ended)) = queue.cmdline.as_mut().filter(|_| unstamped && long) {
                    *ended = Some(event.at);
                } else if unstamped {
                    // When: the held text is `unstamped` and this D ended a short run (or none),
                    // no notification can follow, so the text is dropped now.
                    queue.cmdline = None;
                }
            }
            CommandEvent::PromptStart | CommandEvent::PromptEnd => {
                // When: the event is a prompt mark, it neither starts nor ends a command.
            }
        }
    }
}

/// Drain `queue` in parse order, pairing a stamped command text with the D it was stamped by.
/// The stamped text is taken either way, so it never outlives the drain.
pub(super) fn drain_paired(
    queue: &mut CommandQueue,
) -> Vec<(PaneCommandEvent, Option<CmdlineText>)> {
    let mut stamped = queue.cmdline.take_if(|slot| slot.1.is_some());
    queue
        .events
        .drain(..)
        .map(|event| {
            let owns = matches!(event.event, CommandEvent::CmdEnd(_))
                && stamped.as_ref().is_some_and(|(_, ended)| *ended == Some(event.at));
            let text = owns.then(|| stamped.take()).flatten().map(|(text, _)| text);
            (event, text)
        })
        .collect()
}

fn maybe_notify_long_command(
    config: &Config,
    duration: Option<Duration>,
    exit: Option<u8>,
    cmdline: Option<&CmdlineText>,
) {
    if let Some(body) = long_command_message(config, duration, exit, cmdline) {
        notify_command_done(body);
    }
}

/// The desktop notification a finished command raises: only with `long_command` on (whatever
/// `osc9` is) and after it ran past `threshold_secs`. Its `cmdline` is appended only with
/// `long_command_cmdline` on, sanitized and cut to [`CMDLINE_NOTICE_CHARS`].
pub(super) fn long_command_message(
    config: &Config,
    duration: Option<Duration>,
    exit: Option<u8>,
    cmdline: Option<&CmdlineText>,
) -> Option<String> {
    let duration = duration?;
    if !config.notifications.long_command {
        // When: `config` disables long_command notifications, so a finished
        // command stays silent however long it ran.
        return None;
    }
    if duration.as_secs() <= config.notifications.threshold_secs {
        // When: `duration` sits within threshold_secs, so the user was not
        // waiting long enough for a desktop interruption to be welcome.
        return None;
    }
    let message = match exit {
        Some(0) => format!("Command completed successfully after {}s", duration.as_secs()),
        Some(code) => format!("Command failed with exit code {code}"),
        None => format!("Command completed after {}s", duration.as_secs()),
    };
    let shown = cmdline.filter(|_| config.notifications.long_command_cmdline);
    Some(match shown {
        Some(cmdline) => format!("{message}: {}", cmdline_display(cmdline.as_str())),
        None => message,
    })
}

/// The most characters of a command's text a long-command notification shows.
const CMDLINE_NOTICE_CHARS: usize = 120;

/// Command `text` for display: control and format characters (Unicode Cc, Cf) become spaces, so
/// it stays one line and cannot reorder or hide the message, then it is cut to
/// [`CMDLINE_NOTICE_CHARS`]. The text is only shown, never evaluated.
fn cmdline_display(text: &str) -> String {
    use unicode_general_category::{get_general_category, GeneralCategory};
    text.chars()
        .map(|character| match get_general_category(character) {
            GeneralCategory::Control | GeneralCategory::Format => ' ',
            _ => character,
        })
        .take(CMDLINE_NOTICE_CHARS)
        .collect()
}

/// Raise a desktop notification for a finished command, on every platform; it has no source
/// window, so no focus rule applies.
pub(super) fn notify_command_done(body: String) {
    let notice = DesktopNotice { title: "Command done".into(), body, native_id: None };
    super::desktop_notify::send_desktop(notice);
}

/// Raise a desktop notification for a program notification, replacing an earlier one with the
/// same `native_id`.
pub(super) fn notify_program(title: &str, body: &str, native_id: Option<String>) {
    let notice = DesktopNotice { title: title.into(), body: body.into(), native_id };
    super::desktop_notify::send_desktop(notice);
}

#[cfg(test)]
#[path = "command_events_tests.rs"]
mod command_events_tests;
