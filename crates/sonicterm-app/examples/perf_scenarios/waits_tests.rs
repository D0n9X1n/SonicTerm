//! Pins the bounded waits: checkpoint expiry, the first-present bound and S11's image frame.

use super::*;
use crate::scenarios::Host;

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
        let reason = first_present_missing_reason(FIRST_PRESENT_WAIT, 0, native, Host::Posix);
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

/// The reason a blocked verdict gives; any other verdict fails the test.
fn blocked_reason(verdict: ImageVerdict) -> String {
    match verdict {
        ImageVerdict::Blocked(reason) => reason,
        other => panic!("not blocked: {other:?}"),
    }
}

#[test]
fn a_role_exit_before_the_run_finishes_invalidates_it() {
    // A role program that exits before the run ends leaves its pane without its workload, so the
    // run is invalid, and the reason names the pane and how it exited.
    let clean = pane_exit_reason(false, 7, Some(true)).unwrap();
    assert!(clean.contains("pane 7") && clean.contains("exited cleanly"), "{clean}");
    let unclean = pane_exit_reason(false, 7, Some(false)).unwrap();
    assert!(unclean.contains("pane 7") && unclean.contains("exited uncleanly"), "{unclean}");
    let unknown = pane_exit_reason(false, 7, None).unwrap();
    assert!(unknown.contains("pane 7") && unknown.contains("exit status unknown"), "{unknown}");
    // Once the run has finished, panes exit as the session tears down; that is no reason.
    assert_eq!(pane_exit_reason(true, 7, Some(false)), None);
}

#[test]
fn an_image_never_registered_is_blocked_on_windows() {
    // ConPTY can drop an OSC 1337 image; with nothing registered 10 s after the image phase starts
    // the Windows run is blocked, while macOS keeps waiting up to its run deadline as before.
    assert_eq!(IMAGE_REGISTER_WAIT, FIRST_PRESENT_WAIT);
    let start = Instant::now();
    let due = start + IMAGE_REGISTER_WAIT;
    let early = after(start, 9_000);
    let waiting = ImageProgress::Waiting;
    assert_eq!(
        image_verdict(Host::Windows, false, due, None, waiting, early),
        ImageVerdict::Waiting
    );
    let reason = blocked_reason(image_verdict(Host::Windows, false, due, None, waiting, due));
    assert!(reason.starts_with("OSC 1337 image not registered"), "{reason}");
    let late = after(start, 60_000);
    assert_eq!(image_verdict(Host::Posix, false, due, None, waiting, late), ImageVerdict::Waiting);
}

#[test]
fn a_registered_image_that_is_not_promoted_is_blocked() {
    // On Windows an image the atlas never took is blocked even when no frame presented in time, so
    // that check comes before the expiry's invalid; macOS never reads the atlas.
    let start = Instant::now();
    let due = start + IMAGE_REGISTER_WAIT;
    let now = after(start, 3_000);
    for progress in [ImageProgress::Presented, ImageProgress::Expired] {
        let reason =
            blocked_reason(image_verdict(Host::Windows, true, due, Some(false), progress, now));
        assert!(reason.starts_with("OSC 1337 image not promoted"), "{progress:?}: {reason}");
    }
    let presented = ImageProgress::Presented;
    let expired = ImageProgress::Expired;
    assert_eq!(
        image_verdict(Host::Windows, true, due, Some(true), presented, now),
        ImageVerdict::Valid
    );
    assert!(matches!(
        image_verdict(Host::Windows, true, due, Some(true), expired, now),
        ImageVerdict::Invalid(_)
    ));
    // Before a frame decides the phase, the atlas is not judged yet.
    let waiting = ImageProgress::Waiting;
    assert_eq!(
        image_verdict(Host::Windows, true, due, Some(false), waiting, now),
        ImageVerdict::Waiting
    );
    assert!(matches!(
        image_verdict(Host::Posix, true, due, Some(false), expired, now),
        ImageVerdict::Invalid(_)
    ));
    assert_eq!(
        image_verdict(Host::Posix, true, due, Some(false), presented, now),
        ImageVerdict::Valid
    );
}

#[test]
fn windows_cover_arms_no_synthetic_occlusion() {
    // winit reports no occlusion on Windows, so the cover arms no synthetic state there and
    // uncover_ms stays null; macOS keeps its 2 s fallback.
    assert!(occlusion_wait_applies(Host::Posix));
    assert!(!occlusion_wait_applies(Host::Windows));
}

#[test]
fn missing_first_frame_on_windows_names_a_locked_session() {
    // Windows has no hidden Space; a window that never presents most likely sits in a locked or
    // disconnected session. The reason still says suspected occlusion, which the smoke retries.
    let reason = first_present_missing_reason(FIRST_PRESENT_WAIT, 0, None, Host::Windows);
    assert!(reason.contains("suspected occlusion"), "{reason}");
    assert!(reason.contains("locked or disconnected session"), "{reason}");
    assert!(!reason.contains("hidden Space"), "{reason}");
}
