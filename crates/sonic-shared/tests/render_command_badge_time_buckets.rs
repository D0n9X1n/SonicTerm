use std::time::{Duration, Instant};

use sonic_shared::render::command_status_hash;
use sonic_ui::tabs::CommandStatus;

#[test]
fn running_hash_changes_at_badge_visibility_boundary() {
    let t0 = Instant::now();
    let status = CommandStatus::Running(t0);

    let at_five = command_status_hash(&status, t0 + Duration::from_secs(5));
    let at_six = command_status_hash(&status, t0 + Duration::from_secs(6));

    assert_ne!(at_five, at_six);
}

#[test]
fn done_hash_changes_after_expiry() {
    let t0 = Instant::now();
    let status = CommandStatus::Done { exit: Some(0), until: t0 + Duration::from_secs(5) };

    let before = command_status_hash(&status, t0);
    let after = command_status_hash(&status, t0 + Duration::from_secs(6));

    assert_ne!(before, after);
}
