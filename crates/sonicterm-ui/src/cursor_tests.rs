use super::*;

/// The blink starts on, alternates every phase, and the boundaries fall exactly on phase
/// multiples, so activity always shows the cursor first.
#[test]
fn the_blink_starts_on_and_alternates_each_phase() {
    let epoch = Instant::now();
    let at = |millis: u64| epoch + Duration::from_millis(millis);
    assert!(blink_on(epoch, epoch));
    assert!(blink_on(epoch, at(529)));
    assert!(!blink_on(epoch, at(530)));
    assert!(!blink_on(epoch, at(1_059)));
    assert!(blink_on(epoch, at(1_060)));
    // A clock read before the epoch counts as the epoch itself.
    assert!(blink_on(at(10), epoch));
}

/// From the rest on, the cursor is solid and no blink frame is due, however long the idle.
#[test]
fn the_cursor_rests_solid_after_the_rest_period() {
    let epoch = Instant::now();
    for idle in [BLINK_REST_AFTER, BLINK_REST_AFTER + BLINK_PHASE, Duration::from_secs(3_600)] {
        assert!(blink_on(epoch, epoch + idle));
        assert_eq!(next_blink_deadline(epoch, epoch + idle), None);
    }
}

/// Each deadline is the next phase boundary, the last one is the rest itself, and a frame at that
/// rest draws the cursor on even when the boundary before it was an off phase.
#[test]
fn deadlines_follow_phase_boundaries_up_to_the_rest() {
    let epoch = Instant::now();
    assert_eq!(next_blink_deadline(epoch, epoch), Some(epoch + BLINK_PHASE));
    assert_eq!(
        next_blink_deadline(epoch, epoch + Duration::from_millis(531)),
        Some(epoch + BLINK_PHASE * 2)
    );
    let last_phase_start = BLINK_REST_AFTER - Duration::from_millis(1);
    assert_eq!(
        next_blink_deadline(epoch, epoch + last_phase_start),
        Some(epoch + BLINK_REST_AFTER)
    );
    assert!(blink_on(epoch, epoch + BLINK_REST_AFTER));
    // Walk every deadline from the epoch: about 28 frames, then none, the last one solid.
    let mut now = epoch;
    let mut frames = 0;
    while let Some(deadline) = next_blink_deadline(epoch, now) {
        assert!(deadline > now, "a deadline never repeats or goes back");
        now = deadline;
        frames += 1;
    }
    assert_eq!(frames, 29, "28 phase boundaries and the rest");
    assert!(blink_on(epoch, now), "the last frame shows the cursor");
}
