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
                let chrome = self.command_maintenance(owner, now);
                let visible =
                    self.windows.get(&owner).is_some_and(|window| window.visible_output_advanced());
                if visible {
                    self.request_owner_redraw(owner, RedrawCause::Output);
                } else if chrome {
                    // When: only tab-bar command chrome changed, the frame repaints chrome, not output.
                    self.request_owner_redraw(owner, RedrawCause::Chrome);
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

#[cfg(test)]
#[path = "output_event_tests.rs"]
mod output_event_tests;
