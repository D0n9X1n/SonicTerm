//! Pure two-step quit confirmation state machine for the Cmd+Q chord.
//!
//! macOS convention (Chrome/Arc-style): a single Cmd+Q press must NOT quit
//! immediately. The first press "arms" the guard and surfaces a red
//! "Press ⌘Q one more time to quit" prompt; the app only exits if the chord
//! is pressed again before [`QUIT_CONFIRM_DURATION`] elapses.
//!
//! This type owns no winit / AppKit state so it is exercised entirely by the
//! sibling unit tests without an event loop. The app layer feeds it three
//! signals — press, key-repeat status, and a timer tick — and reacts to the
//! returned [`QuitHoldAction`].

use std::time::{Duration, Instant};

/// How long the second Cmd+Q press can confirm quit.
pub const QUIT_CONFIRM_DURATION: Duration = Duration::from_secs(5);

/// Message shown in the red top-right notification while the guard is armed.
pub const QUIT_CONFIRM_PROMPT: &str = "Press ⌘Q one more time to quit";

/// What the app should do in response to a quit-chord signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitHoldAction {
    /// Nothing to do (repeat press / not armed / deadline not yet reached).
    None,
    /// Show the red confirmation prompt and schedule a wake at `deadline`.
    /// Emitted exactly once, on the transition into the armed state.
    ShowPrompt {
        /// Instant at which the app should expire the pending confirmation.
        /// Callers fold this into their event-loop `WaitUntil` schedule.
        deadline: Instant,
    },
    /// The second press confirmed quit.
    Quit,
}

/// Tracks whether the first Cmd+Q press is waiting for confirmation.
#[derive(Debug, Default, Clone, Copy)]
pub struct QuitHold {
    /// `Some(t)` while a second press should quit.
    confirm_until: Option<Instant>,
}

impl QuitHold {
    /// Fresh, disarmed guard.
    #[must_use]
    pub fn new() -> Self {
        Self { confirm_until: None }
    }

    /// Whether the guard is currently armed (second press quits).
    /// Primarily an introspection helper for tests and callers that want to
    /// branch without reading the deadline.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.confirm_until.is_some()
    }

    /// Signal that the Cmd+Q chord was pressed at `now`.
    ///
    /// The first press arms the guard and returns [`QuitHoldAction::ShowPrompt`].
    /// A second non-repeat press within the confirmation window returns
    /// [`QuitHoldAction::Quit`]. Auto-repeat is ignored so holding the original
    /// chord does not accidentally quit.
    pub fn on_press(&mut self, now: Instant, is_repeat: bool) -> QuitHoldAction {
        if is_repeat {
            return QuitHoldAction::None;
        }
        if let Some(deadline) = self.confirm_until {
            if now <= deadline {
                self.confirm_until = None;
                return QuitHoldAction::Quit;
            }
        }
        let deadline = now + QUIT_CONFIRM_DURATION;
        self.confirm_until = Some(deadline);
        QuitHoldAction::ShowPrompt { deadline }
    }

    /// Timer tick: expire a pending confirmation after its five-second window.
    ///
    /// Expiry does not dismiss the notification directly; the app's central
    /// notification timer clears the prompt on the same five-second cadence.
    pub fn on_tick(&mut self, now: Instant) -> QuitHoldAction {
        match self.confirm_until {
            Some(deadline) if now >= deadline => {
                self.confirm_until = None;
                QuitHoldAction::None
            }
            _ => QuitHoldAction::None,
        }
    }

    /// The instant at which the pending confirmation expires, if armed.
    /// Callers fold this into their event-loop wake schedule so [`Self::on_tick`]
    /// is polled at the right time even when no other input arrives.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.confirm_until
    }
}

#[cfg(test)]
#[path = "quit_hold_tests.rs"]
mod quit_hold_tests;
