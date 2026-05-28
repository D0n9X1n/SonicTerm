use std::time::{Duration, Instant};

use sonic_ui::tabs::CommandStatus;

#[test]
fn running_background_command_after_five_seconds_shows_badge() {
    let now = Instant::now();
    let status = CommandStatus::Running(now - Duration::from_secs(6));
    assert_eq!(status.badge(now, false), Some("…"));
}

#[test]
fn running_active_or_idle_tab_has_no_badge() {
    let now = Instant::now();
    assert_eq!(CommandStatus::Running(now - Duration::from_secs(6)).badge(now, true), None);
    assert_eq!(CommandStatus::Idle.badge(now, false), None);
}

#[test]
fn done_status_shows_success_or_failure_until_deadline() {
    let now = Instant::now();
    assert_eq!(
        CommandStatus::Done { exit: Some(0), until: now + Duration::from_secs(3) }
            .badge(now, false),
        Some("✓")
    );
    assert_eq!(
        CommandStatus::Done { exit: Some(1), until: now + Duration::from_secs(3) }.badge(now, true),
        Some("✗")
    );
    assert_eq!(CommandStatus::Done { exit: Some(0), until: now }.badge(now, false), None);
}
