//! Unit tests for the [`QuitHold`] hold-to-quit state machine.

use super::*;

fn t0() -> Instant {
    // A fixed base instant; all cases work in offsets from here so they never
    // touch the wall clock (scripts/tests must be deterministic).
    Instant::now()
}

#[test]
fn first_press_arms_and_requests_prompt() {
    let mut hold = QuitHold::new();
    let now = t0();
    let action = hold.on_press(now);
    assert_eq!(action, QuitHoldAction::ShowPrompt { deadline: now + QUIT_HOLD_DURATION });
    assert!(hold.is_armed());
    assert_eq!(hold.deadline(), Some(now + QUIT_HOLD_DURATION));
}

#[test]
fn repeat_press_while_armed_is_noop() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now);
    // Auto-repeat: same chord fires again a bit later — must not re-emit the
    // prompt nor move the deadline (the original arm instant governs the hold).
    let repeat = hold.on_press(now + Duration::from_millis(50));
    assert_eq!(repeat, QuitHoldAction::None);
    assert_eq!(hold.deadline(), Some(now + QUIT_HOLD_DURATION));
}

#[test]
fn tick_before_deadline_does_not_quit() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now);
    let early = hold.on_tick(now + QUIT_HOLD_DURATION - Duration::from_millis(1));
    assert_eq!(early, QuitHoldAction::None);
    assert!(hold.is_armed(), "still armed before the deadline");
}

#[test]
fn tick_at_deadline_quits_once() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now);
    let quit = hold.on_tick(now + QUIT_HOLD_DURATION);
    assert_eq!(quit, QuitHoldAction::Quit);
    assert!(!hold.is_armed(), "guard disarms after firing quit");
    // A second tick must not quit again.
    let again = hold.on_tick(now + QUIT_HOLD_DURATION + Duration::from_secs(1));
    assert_eq!(again, QuitHoldAction::None);
}

#[test]
fn release_before_deadline_cancels_and_dismisses() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now);
    let released = hold.on_release();
    assert_eq!(released, QuitHoldAction::Dismiss);
    assert!(!hold.is_armed());
    // A tick after an early release must never quit.
    let tick = hold.on_tick(now + QUIT_HOLD_DURATION + Duration::from_secs(1));
    assert_eq!(tick, QuitHoldAction::None);
}

#[test]
fn release_when_not_armed_is_noop() {
    let mut hold = QuitHold::new();
    assert_eq!(hold.on_release(), QuitHoldAction::None);
    assert_eq!(hold.deadline(), None);
}

#[test]
fn rearm_after_quit_starts_a_fresh_hold() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now);
    assert_eq!(hold.on_tick(now + QUIT_HOLD_DURATION), QuitHoldAction::Quit);
    // A later, separate Cmd+Q press must arm again from scratch.
    let later = now + Duration::from_secs(5);
    assert_eq!(
        hold.on_press(later),
        QuitHoldAction::ShowPrompt { deadline: later + QUIT_HOLD_DURATION }
    );
    assert!(hold.is_armed());
}
