use super::*;

/// The easing starts at 0, ends at 1, rises monotonically and is past halfway at half time.
#[test]
fn ease_out_starts_ends_and_rises() {
    assert_eq!(ease_out_cubic(0.0), 0.0);
    assert_eq!(ease_out_cubic(1.0), 1.0);
    assert_eq!(ease_out_cubic(2.0), 1.0, "clamped");
    assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-6, "ease-out is 87.5% at half time");
    let mut previous = 0.0;
    for step in 1..=20 {
        let value = ease_out_cubic(step as f32 / 20.0);
        assert!(value > previous);
        previous = value;
    }
}

/// A presented cursor that moves glides from where it was to its target: the first sample is at
/// the start, later samples move monotonically, and from the duration on it rests at the target.
#[test]
fn a_move_glides_from_the_presented_cell_to_the_target() {
    let start = Instant::now();
    let mut glide = CursorGlide::default();
    let first = GlideCell::at(2, 3, 1);
    assert_eq!(glide.sample(first, start, true, false), None, "nothing on screen yet");
    glide.settle(true);
    let target = GlideCell::at(2, 9, 1);
    assert_eq!(glide.sample(target, start, true, false), Some(first), "starts where it was");
    assert!(glide.gliding());
    let mut previous_col = first.col;
    for millis in [10, 30, 60, 89] {
        let at = glide.sample(target, start + Duration::from_millis(millis), true, false).unwrap();
        assert!(at.col >= previous_col && at.col <= target.col, "monotone toward the target");
        previous_col = at.col;
    }
    assert_eq!(glide.sample(target, start + GLIDE_DURATION, true, false), None);
    assert!(!glide.gliding(), "the glide ended and nothing more is scheduled");
}

/// Gliding not allowed, or a snap, draws the target at once and cancels a glide in progress.
#[test]
fn a_snap_or_a_disallowed_glide_jumps() {
    let start = Instant::now();
    let mut glide = CursorGlide::default();
    glide.sample(GlideCell::at(0, 0, 1), start, true, false);
    glide.settle(true);
    assert!(glide.sample(GlideCell::at(0, 5, 1), start, true, false).is_some());
    assert_eq!(glide.sample(GlideCell::at(0, 5, 1), start, true, true), None, "snap");
    assert!(!glide.gliding());
    glide.settle(true);
    assert_eq!(glide.sample(GlideCell::at(0, 9, 1), start, false, false), None, "not allowed");
    assert!(!glide.gliding());
}

/// A move during a glide starts again from the position last presented, not from the old cell,
/// and frames that never present neither move the start nor restart the clock.
#[test]
fn a_retarget_starts_from_the_presented_position() {
    let start = Instant::now();
    let mut glide = CursorGlide::default();
    glide.sample(GlideCell::at(0, 0, 1), start, true, false);
    glide.settle(true);
    let midway = glide
        .sample(GlideCell::at(0, 8, 1), start + Duration::from_millis(30), true, false)
        .unwrap();
    glide.settle(true);
    // A failed frame keeps the committed position and the clock.
    let unpresented =
        glide.sample(GlideCell::at(0, 8, 1), start + Duration::from_millis(40), true, false);
    glide.settle(false);
    assert!(unpresented.is_some());
    assert_eq!(glide.committed(), Some(midway));
    let retarget = GlideCell::at(0, 2, 1);
    let first = glide.sample(retarget, start + Duration::from_millis(50), true, false);
    assert_eq!(first, Some(midway), "the new glide starts where the screen shows the cursor");
    // Recovery after the deadline lands exactly on the target.
    assert_eq!(glide.sample(retarget, start + Duration::from_millis(500), true, false), None);
}

/// Consecutive samples that quantize to the same position are equal, so their frames plan a no-op;
/// a hidden cursor reappears without a glide.
#[test]
fn equal_samples_key_equal_and_a_hidden_cursor_reappears_in_place() {
    let start = Instant::now();
    let mut glide = CursorGlide::default();
    glide.sample(GlideCell::at(0, 0, 1), start, true, false);
    glide.settle(true);
    let target = GlideCell::at(0, 1, 1);
    // The glide's clock starts at this sample; near its end two samples a millisecond apart
    // round to the same sixteenth of the one-column move.
    assert!(glide.sample(target, start, true, false).is_some());
    let early = glide.sample(target, start + Duration::from_millis(80), true, false);
    let later = glide.sample(target, start + Duration::from_millis(81), true, false);
    assert!(early.is_some());
    assert_eq!(early, later, "both round to the same sixteenth");
    glide.hide();
    glide.settle(true);
    assert_eq!(
        glide.sample(GlideCell::at(9, 9, 1), start, true, false),
        None,
        "no glide from hidden"
    );
}

/// A jump longer than half the pane's diagonal is long; a move of a few cells is not.
#[test]
fn long_jumps_are_measured_against_the_pane() {
    let origin = GlideCell::at(0, 0, 1);
    assert!(!origin.long_jump(GlideCell::at(1, 10, 1), 80, 24));
    assert!(origin.long_jump(GlideCell::at(23, 79, 1), 80, 24));
}

/// A glide whose landing frame fails to present does not glide again on the retry: the target is
/// unchanged, so the retry draws it at once, however many retries fail.
#[test]
fn a_failed_landing_frame_lands_on_the_retry() {
    let start = Instant::now();
    let mut glide = CursorGlide::default();
    glide.sample(GlideCell::at(0, 0, 1), start, true, false);
    glide.settle(true);
    let target = GlideCell::at(0, 8, 1);
    assert!(glide.sample(target, start, true, false).is_some());
    glide.settle(true);
    assert!(glide.sample(target, start + Duration::from_millis(30), true, false).is_some());
    glide.settle(true);
    // The landing frame is sampled and fails, then two retries fail too.
    for retry in [100, 110, 120] {
        let at = glide.sample(target, start + Duration::from_millis(retry), true, false);
        assert_eq!(at, None, "retry at {retry} ms lands on the target");
        assert!(!glide.gliding());
        glide.settle(false);
    }
    // A real move afterwards still glides, from the position last presented.
    assert!(glide
        .sample(GlideCell::at(0, 2, 1), start + Duration::from_millis(200), true, false)
        .is_some());
}
