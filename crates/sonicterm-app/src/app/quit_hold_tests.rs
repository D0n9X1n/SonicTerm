//! Unit tests for the [`QuitHold`] two-step quit confirmation state machine.

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
    let action = hold.on_press(now, false);
    assert_eq!(action, QuitHoldAction::ShowPrompt { deadline: now + QUIT_CONFIRM_DURATION });
    assert!(hold.is_armed());
    assert_eq!(hold.deadline(), Some(now + QUIT_CONFIRM_DURATION));
}

#[test]
fn repeat_press_while_armed_is_noop() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now, false);
    // Auto-repeat: same chord fires again a bit later — must not re-emit the
    // prompt nor quit.
    let repeat = hold.on_press(now + Duration::from_millis(50), true);
    assert_eq!(repeat, QuitHoldAction::None);
    assert_eq!(hold.deadline(), Some(now + QUIT_CONFIRM_DURATION));
}

#[test]
fn second_non_repeat_press_before_deadline_quits() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now, false);
    let quit = hold.on_press(now + Duration::from_millis(50), false);
    assert_eq!(quit, QuitHoldAction::Quit);
    assert!(!hold.is_armed(), "guard disarms after confirmed quit");
}

#[test]
fn tick_at_deadline_expires_without_quitting() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now, false);
    let expired = hold.on_tick(now + QUIT_CONFIRM_DURATION);
    assert_eq!(expired, QuitHoldAction::None);
    assert!(!hold.is_armed(), "guard disarms after the confirmation expires");
    let again = hold.on_tick(now + QUIT_CONFIRM_DURATION + Duration::from_secs(1));
    assert_eq!(again, QuitHoldAction::None);
}

#[test]
fn press_after_expiry_starts_a_fresh_confirmation() {
    let mut hold = QuitHold::new();
    let now = t0();
    let _ = hold.on_press(now, false);
    let _ = hold.on_tick(now + QUIT_CONFIRM_DURATION);
    let later = now + QUIT_CONFIRM_DURATION + Duration::from_secs(1);
    assert_eq!(
        hold.on_press(later, false),
        QuitHoldAction::ShowPrompt { deadline: later + QUIT_CONFIRM_DURATION }
    );
    assert!(hold.is_armed());
}
