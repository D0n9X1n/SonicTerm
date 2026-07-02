//! Pure hold-to-quit state machine for the Cmd+Q chord.
//!
//! macOS convention (Chrome/Arc-style): a single Cmd+Q press must NOT quit
//! immediately. The first press "arms" the guard and surfaces a red
//! "Hold ⌘Q to quit the app" prompt; the app only exits once the chord has
//! been held continuously for [`QUIT_HOLD_DURATION`]. Releasing the chord
//! (Cmd up, or focus loss) before the deadline cancels the pending quit.
//!
//! This type owns no winit / AppKit state so it is exercised entirely by the
//! sibling unit tests without an event loop. The app layer feeds it three
//! signals — press, release, and a timer tick — and reacts to the returned
//! [`QuitHoldAction`].

use std::time::{Duration, Instant};

/// How long Cmd+Q must be held before the app actually quits.
pub const QUIT_HOLD_DURATION: Duration = Duration::from_millis(800);

/// Message shown in the red top-right notification while the guard is armed.
pub const QUIT_HOLD_PROMPT: &str = "Hold ⌘Q to quit the app";

/// What the app should do in response to a quit-chord signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitHoldAction {
    /// Nothing to do (already armed / not armed / deadline not yet reached).
    None,
    /// Show the red "hold to quit" prompt and schedule a wake at `deadline`.
    /// Emitted exactly once, on the transition into the armed state.
    ShowPrompt {
        /// Instant at which the app should re-check the hold (and quit if the
        /// chord is still down). Callers fold this into their event-loop
        /// `WaitUntil` schedule.
        deadline: Instant,
    },
    /// The hold completed: quit the application now.
    Quit,
    /// The pending quit was cancelled; dismiss the prompt if it is showing.
    Dismiss,
}

/// Tracks whether the Cmd+Q chord is currently held and since when.
#[derive(Debug, Default, Clone, Copy)]
pub struct QuitHold {
    /// `Some(t)` while the chord is held, where `t` is the arm instant.
    armed_at: Option<Instant>,
}

impl QuitHold {
    /// Fresh, disarmed guard.
    #[must_use]
    pub fn new() -> Self {
        Self { armed_at: None }
    }

    /// Whether the guard is currently armed (chord held, quit pending).
    /// Primarily an introspection helper for tests and callers that want to
    /// branch without reading the deadline.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed_at.is_some()
    }

    /// Signal that the Cmd+Q chord was pressed at `now`.
    ///
    /// The first press arms the guard and returns [`QuitHoldAction::ShowPrompt`]
    /// with the hold deadline. Auto-repeat presses while already armed return
    /// [`QuitHoldAction::None`] so the prompt is not re-emitted on every repeat
    /// — the timer tick is what completes the quit.
    pub fn on_press(&mut self, now: Instant) -> QuitHoldAction {
        if self.armed_at.is_some() {
            return QuitHoldAction::None;
        }
        self.armed_at = Some(now);
        QuitHoldAction::ShowPrompt { deadline: now + QUIT_HOLD_DURATION }
    }

    /// Signal that the chord was released (Cmd up, `q` up, or focus lost).
    ///
    /// Returns [`QuitHoldAction::Dismiss`] when this actually disarms a pending
    /// quit (so the caller can clear the prompt), otherwise
    /// [`QuitHoldAction::None`].
    pub fn on_release(&mut self) -> QuitHoldAction {
        if self.armed_at.take().is_some() {
            QuitHoldAction::Dismiss
        } else {
            QuitHoldAction::None
        }
    }

    /// Timer tick: has the chord been held long enough to quit?
    ///
    /// Returns [`QuitHoldAction::Quit`] once `now` reaches the deadline while
    /// still armed (the guard disarms itself so quit fires at most once).
    /// Before the deadline it returns [`QuitHoldAction::None`] and leaves the
    /// guard armed.
    pub fn on_tick(&mut self, now: Instant) -> QuitHoldAction {
        match self.armed_at {
            Some(start) if now.duration_since(start) >= QUIT_HOLD_DURATION => {
                self.armed_at = None;
                QuitHoldAction::Quit
            }
            _ => QuitHoldAction::None,
        }
    }

    /// The instant at which the pending quit fires, if armed. Callers fold this
    /// into their event-loop wake schedule so [`Self::on_tick`] is polled at
    /// the right time even when no other input arrives.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.armed_at.map(|start| start + QUIT_HOLD_DURATION)
    }
}

#[cfg(test)]
#[path = "quit_hold/tests.rs"]
mod tests;
