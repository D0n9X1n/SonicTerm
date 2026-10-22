//! The one service path behind both output events: an explicit window request and a pane's
//! flushed output.

use super::{redraw::RedrawCause, *};

/// An output event the event loop services; both `do_user_event` output arms build one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputEvent {
    /// A VT worker flushed output for `pane_id`, whose target was `window_id` when it sent.
    Pane {
        /// The redraw target the worker read.
        window_id: WindowId,
        /// The pane whose output was flushed.
        pane_id: u64,
    },
    /// A harness or test asked for a frame of this window, whatever its output state.
    Explicit(WindowId),
}

impl App {
    /// Service one output event; both output arms of `do_user_event` call only this.
    ///
    /// `Explicit` counts the request, arms the Windows foreground probe, runs command
    /// maintenance and requests an `Output` frame unconditionally. `Pane` acknowledges the pane's
    /// token first, then does the same counting, arming and maintenance for the window that owns
    /// the pane now, and requests a frame only when that window shows new output (`Output`) or
    /// its tab-bar command chrome changed (`Chrome`). A pane no window holds (retired, or its
    /// window closed) is a stale event and is ignored.
    // Ordering: output_outstanding swaps AcqRel with the worker's swap, so later generation
    // reads in visible_output_advanced include every batch whose flush was suppressed.
    pub(super) fn service_output_event(&mut self, event: OutputEvent, now: Instant) {
        match event {
            OutputEvent::Explicit(window_id) => {
                self.note_user_request_redraw(window_id);
                #[cfg(windows)]
                self.arm_foreground_probe_after_output(now);
                self.output_redraw_notification(window_id, now);
            }
            OutputEvent::Pane { window_id, pane_id } => {
                // When: a worker flushed pane_id, acknowledge it and filter the frame request.
                let Some(owner) = self
                    .windows
                    .iter()
                    .find_map(|(id, window)| window.panes.contains_key(&pane_id).then_some(*id))
                else {
                    // When: no window holds pane_id, it retired after its worker read the target.
                    return;
                };
                // Acknowledge before any check, so a flush after this point sends a fresh event.
                self.windows[&owner].panes[&pane_id]
                    .output_outstanding
                    .swap(false, Ordering::AcqRel);
                #[cfg(test)]
                run_after_acknowledge_hook();
                self.note_user_request_redraw(window_id);
                #[cfg(windows)]
                self.arm_foreground_probe_after_output(now);
                let mut chrome = self.command_maintenance(owner, now);
                chrome |= self.mark_bell(owner, pane_id);
                // The watched pane is loaded explicitly first, so `any` below sees at least its generation.
                let watched =
                    self.windows.get(&owner).and_then(|window| window.watched_observation());
                let visible =
                    self.windows.get(&owner).is_some_and(|window| window.visible_output_advanced());
                // Only the Output request is the check's outcome; a chrome-only request leaves it `None`.
                let mut outcome = OutputCheckOutcomeV1::None;
                if visible {
                    outcome = self.request_owner_redraw(owner, RedrawCause::Output);
                } else if chrome {
                    // When: only tab-bar command chrome changed, the frame repaints chrome, not output.
                    self.request_owner_redraw(owner, RedrawCause::Chrome);
                }
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.redraw.timeline.note_output_service(pane_id, watched, outcome);
                }
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only pause point between a pane event's acknowledgement and its generation check.
    static AFTER_ACKNOWLEDGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: run `hook` once, on this thread, at the next pane event's pause point.
#[cfg(test)]
fn pause_after_acknowledge(hook: impl FnOnce() + 'static) {
    AFTER_ACKNOWLEDGE.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

/// Test-only: run and clear the pause-point hook, if one is set.
#[cfg(test)]
fn run_after_acknowledge_hook() {
    let hook = AFTER_ACKNOWLEDGE.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

impl App {
    /// Mark the tab of `pane_id` in `owner` when the pane rang since its tab was last visited and
    /// that tab is not active. Service never acknowledges, so the mark stays until a visit. Returns
    /// whether the tab's mark appeared.
    // Ordering: bells is Relaxed; it is compared only with this pane's own acknowledgement.
    fn mark_bell(&mut self, owner: WindowId, pane_id: u64) -> bool {
        let Some(window) = self.windows.get_mut(&owner) else {
            // When: no window has owner, there is no tab to mark.
            return false;
        };
        if !window.alert_settings.is_some_and(|settings| settings.bell) {
            // When: alert_settings has no bell source, bells mark nothing.
            return false;
        }
        let Some(pane) = window.panes.get(&pane_id) else {
            // When: the window no longer holds pane_id, nothing rang there.
            return false;
        };
        if pane.bells.load(Ordering::Relaxed) <= pane.acked_bells {
            // When: bells is not past acked_bells, every ring was seen.
            return false;
        }
        let Some(tab_idx) =
            window.tab_states.iter().position(|tab| tab.tree.leaves().contains(&pane_id))
        else {
            // When: no tab holds pane_id, it is between tabs.
            return false;
        };
        window.tabs.mark(tab_idx, |marks| marks.bell = true)
    }

    /// Acknowledge every tab each window activated or left since the last call: its panes' bells
    /// and pending command ends count as seen, so a visit never leaves a mark behind and a ring the
    /// user saw is never replayed. A log that overflowed acknowledges the whole window.
    // Ordering: bells is Relaxed; an acknowledgement is compared only with the same pane's count.
    pub(super) fn acknowledge_tab_activations(&mut self) {
        let Self { windows, config, .. } = self;
        for window in windows.values_mut() {
            let log = window.tabs.take_activation_log();
            if log.entries.is_empty() && !log.overflowed {
                // When: the log is empty, nothing was activated or left.
                continue;
            }
            let indices: Vec<usize> = if log.overflowed {
                (0..window.tab_states.len()).collect()
            } else {
                // When: the log is complete, only the tabs it names are acknowledged.
                log.entries
                    .iter()
                    .flat_map(|(before, after)| [*before, *after])
                    .flatten()
                    .filter_map(|tab_id| window.tabs.index_of(tab_id))
                    .collect()
            };
            for tab_idx in indices {
                let Some(leaves) = window.tab_states.get(tab_idx).map(|tab| tab.tree.leaves())
                else {
                    // When: tab_idx names no tab state, it closed after the log entry.
                    continue;
                };
                for pane_id in leaves {
                    if let Some(pane) = window.panes.get_mut(&pane_id) {
                        pane.acked_bells = pane.bells.load(Ordering::Relaxed);
                    }
                }
                // Command ends queued before the visit are drained as seen, never as marks.
                let _ = super::poll_command_events_for_tab_state(
                    &mut window.panes,
                    &mut window.tab_states,
                    &mut window.tabs,
                    config,
                    tab_idx,
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "output_event_tests.rs"]
mod output_event_tests;
