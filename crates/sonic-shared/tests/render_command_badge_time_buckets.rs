use std::time::{Duration, Instant};

use sonic_shared::render::command_status_hash;
use sonic_ui::tabs::CommandStatus;

#[test]
fn running_hash_changes_after_badge_threshold() {
    let t0 = Instant::now();
    let status = CommandStatus::Running(t0);

    let before = command_status_hash(&status, t0);
    let after = command_status_hash(&status, t0 + Duration::from_secs(6));

    assert_ne!(before, after);
}

#[test]
fn done_hash_changes_after_expiry() {
    let t0 = Instant::now();
    let status = CommandStatus::Done { exit: Some(0), until: t0 + Duration::from_secs(5) };

    let before = command_status_hash(&status, t0);
    let after = command_status_hash(&status, t0 + Duration::from_secs(6));

    assert_ne!(before, after);
}
