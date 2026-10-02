//! Pins the bounded waits: checkpoint expiry, the first-present bound and S11's image frame.

use super::*;

/// `start` plus `millis` milliseconds.
fn after(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
}

#[test]
fn checkpoint_answered_before_its_deadline_records_whether_the_footprint_exists() {
    // `.done` ends the wait; the footprint file beside it is recorded only when it exists.
    let start = Instant::now();
    let deadline = after(start, 60_000);
    assert_eq!(checkpoint_progress(false, false, start, deadline), CheckpointProgress::Waiting);
    assert_eq!(
        checkpoint_progress(true, true, after(start, 500), deadline),
        CheckpointProgress::Answered { footprint: true }
    );
    assert_eq!(
        checkpoint_progress(true, false, after(start, 500), deadline),
        CheckpointProgress::Answered { footprint: false }
    );
    // A `.done` first polled after the deadline was written in time; only the poll was late.
    assert_eq!(
        checkpoint_progress(true, true, after(start, 60_040), deadline),
        CheckpointProgress::Answered { footprint: true }
    );
}

#[test]
fn checkpoint_without_done_by_its_deadline_expires() {
    // The comparison needs every footprint, so an unanswered checkpoint ends the run; the
    // next phase never starts.
    let start = Instant::now();
    let deadline = after(start, 60_000);
    assert_eq!(checkpoint_progress(false, false, deadline, deadline), CheckpointProgress::Expired);
    assert_eq!(
        checkpoint_progress(false, false, after(start, 61_000), deadline),
        CheckpointProgress::Expired
    );
}

#[test]
fn checkpoint_expiry_reason_names_the_checkpoint_and_the_wait() {
    // The reason says which checkpoint went unanswered; it is not an occlusion, so the smoke
    // does not retry it.
    let reason = checkpoint_expired_reason("2-covered", Duration::from_secs(60));
    assert!(reason.contains("checkpoint 2-covered"), "{reason}");
    assert!(reason.contains("60 s"), "{reason}");
    assert!(!reason.to_lowercase().contains("occlu"), "{reason}");
}

#[test]
fn startup_wakes_at_the_first_present_bound_not_the_run_deadline() {
    // An idle App asks for no wake, so the bound itself must wake the loop.
    let start = Instant::now();
    let run_deadline = after(start, 80_000);
    let mut bound = FirstPresentBound::default();
    assert_eq!(bound.wake(run_deadline), run_deadline, "no bound before the window opens");
    bound.window_opened(start);
    assert_eq!(bound.wake(run_deadline), start + FIRST_PRESENT_WAIT);
    // The bound never extends the run's own deadline.
    assert_eq!(bound.wake(after(start, 3_000)), after(start, 3_000));
}

#[test]
fn startup_without_a_present_by_the_bound_expires() {
    // A window that presents no frame ends Startup at the bound, not 80 s later.
    let start = Instant::now();
    let mut bound = FirstPresentBound::default();
    assert!(!bound.expired(after(start, 20_000), false), "not armed before the window opens");
    bound.window_opened(start);
    assert!(!bound.expired(after(start, 9_999), false));
    assert!(bound.expired(start + FIRST_PRESENT_WAIT, false));
    assert!(!bound.expired(after(start, 20_000), true), "a presented frame never expires");
}

#[test]
fn missing_first_frame_is_a_suspected_occlusion_with_its_likely_cause() {
    // No present does not prove occlusion, even after Occluded(false), so the reason only
    // suspects one; it still contains "occlu", which perf-compare retries.
    for native in [None, Some(true), Some(false)] {
        let reason = first_present_missing_reason(FIRST_PRESENT_WAIT, 0, native);
        assert!(reason.contains("suspected occlusion"), "{reason}");
        assert!(!reason.contains("window was occluded"), "{reason}");
        assert!(reason.contains("full-screen app"), "{reason}");
        assert!(reason.contains("10 s"), "{reason}");
    }
}

#[test]
fn image_phase_ends_at_the_first_present_after_the_scan_saw_the_image() {
    // A frame that presented in the scan's own dispatch may predate the registration, so only a
    // later one counts.
    let start = Instant::now();
    let mut image = ImagePresent::default();
    image.observe_frames(5);
    assert_eq!(image.progress(start), ImageProgress::Waiting, "nothing seen yet");
    image.saw_registration(start, 5);
    image.observe_frames(5);
    assert_eq!(image.progress(after(start, 10)), ImageProgress::Waiting);
    image.observe_frames(6);
    assert_eq!(image.progress(after(start, 20)), ImageProgress::Presented);
    // A present before the bound is decisive even when the phase is checked after it.
    assert_eq!(image.progress(after(start, 5_000)), ImageProgress::Presented);
}

#[test]
fn image_phase_without_a_present_by_the_bound_expires() {
    // A surface timeout or a hidden window draws nothing, so the phase cannot end valid.
    let start = Instant::now();
    let mut image = ImagePresent::default();
    image.saw_registration(start, 5);
    assert_eq!(image.deadline(), Some(start + IMAGE_PRESENT_WAIT));
    image.observe_frames(5);
    assert_eq!(image.progress(after(start, 999)), ImageProgress::Waiting);
    assert_eq!(image.progress(start + IMAGE_PRESENT_WAIT), ImageProgress::Expired);
}

#[test]
fn seeing_the_image_requests_exactly_one_redraw() {
    // The frame that drew the image may have presented before a throttled scan saw it, so one
    // redraw guarantees a later present; repeated sightings ask for nothing more.
    let start = Instant::now();
    let mut image = ImagePresent::default();
    assert!(!image.take_redraw_request(), "nothing owed before the image is seen");
    image.saw_registration(start, 5);
    assert!(image.seen());
    assert!(image.take_redraw_request());
    assert!(!image.take_redraw_request());
    image.saw_registration(after(start, 20), 6);
    assert!(!image.take_redraw_request());
    assert_eq!(image.deadline(), Some(start + IMAGE_PRESENT_WAIT), "the first sighting counts");
}
