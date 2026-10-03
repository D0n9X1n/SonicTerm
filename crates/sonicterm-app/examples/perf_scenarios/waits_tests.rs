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

#[test]
fn an_exit_before_its_pane_is_a_role_pane_still_invalidates_the_run() {
    // A role program can exit before startup records its pane, from a bad program.json or a missing
    // fixture; that exit still invalidates the run, naming the pane, instead of waiting out the deadline.
    let early = role_exit_reason(4, Some(false), &[], false, false).unwrap();
    assert!(early.contains("pane 4") && early.contains("exited uncleanly"), "{early}");
    // After startup, a pane outside the plan claims no role, so its exit alone is no reason.
    assert_eq!(role_exit_reason(9, Some(true), &[4], true, false), None);
    // The same recorded exit counts once its pane joins the role panes.
    let joined = role_exit_reason(9, Some(true), &[4, 9], true, false).unwrap();
    assert!(joined.contains("pane 9") && joined.contains("exited cleanly"), "{joined}");
    // Once the run has finished, panes exit as the session tears down.
    assert_eq!(role_exit_reason(4, Some(false), &[], false, true), None);
    assert_eq!(role_exit_reason(9, Some(true), &[4, 9], true, true), None);
}

#[test]
fn a_pointer_at_rest_under_the_opening_window_is_not_input_on_windows() {
    // A window that opens under a still pointer gets a native CursorMoved at that position, and may
    // get it again; on Windows those are dropped, while any other position is motion that voids the run.
    let at = (10.0, 20.0);
    assert_eq!(native_pointer_arrival(Host::Windows, None, at, false), PointerArrival::AtRest);
    for measuring in [false, true] {
        assert_eq!(
            native_pointer_arrival(Host::Windows, Some(at), at, measuring),
            PointerArrival::AtRest
        );
        assert_eq!(
            native_pointer_arrival(Host::Windows, Some(at), (11.0, 20.0), measuring),
            PointerArrival::Moved
        );
    }
    // macOS refuses every native CursorMoved, as before.
    for last in [None, Some(at)] {
        assert_eq!(native_pointer_arrival(Host::Posix, last, at, false), PointerArrival::Moved);
    }
}

#[test]
fn a_first_native_move_after_go_is_motion_on_windows() {
    // A pointer outside the window through startup that enters it after GO sends its first native
    // CursorMoved then: with no baseline from the window's opening, it is physical input.
    assert_eq!(
        native_pointer_arrival(Host::Windows, None, (10.0, 20.0), true),
        PointerArrival::Moved
    );
    let mut pointer = NativePointer::default();
    assert!(!pointer.arrive(Host::Windows, (10.0, 20.0), true));
    assert_eq!(pointer.rest_dropped(), 0);
}

#[test]
fn only_native_moves_set_the_pointer_baseline() {
    // The baseline is the last native position. The probe's synthetic moves, S6's sweep, go straight to
    // the App and never reach this tracker, so a native move back at the old position is still at rest.
    let mut pointer = NativePointer::default();
    assert!(
        pointer.arrive(Host::Windows, (10.0, 20.0), false),
        "the first native move before GO sets the baseline"
    );
    assert!(pointer.arrive(Host::Windows, (10.0, 20.0), true));
    // A synthetic sweep moved the App's pointer across the grid here; the tracker saw none of it.
    assert!(pointer.arrive(Host::Windows, (10.0, 20.0), true));
    assert_eq!(pointer.rest_dropped(), 3);
    assert!(
        !pointer.arrive(Host::Windows, (300.0, 40.0), true),
        "a native move elsewhere is motion"
    );
    assert_eq!(pointer.rest_dropped(), 3);
    // macOS drops none, so every native move still voids its run.
    let mut mac = NativePointer::default();
    assert!(!mac.arrive(Host::Posix, (10.0, 20.0), false));
    assert_eq!(mac.rest_dropped(), 0);
}

#[test]
fn a_barrier_counts_only_frames_presented_after_its_own_act() {
    // Each barrier captures the presented-frame count at its act; a count that rose before the act
    // (frames 3 to 7 here) does not satisfy it, only a later one does.
    let act = Instant::now();
    let mut barrier = FrameBarrier::new(act, 7, MEDIA_FREE_WAIT, false);
    barrier.observe(after(act, 10), 7, 0);
    assert_eq!(barrier.progress(after(act, 10)), BarrierProgress::Waiting);
    barrier.observe(after(act, 20), 8, 0);
    assert_eq!(barrier.progress(after(act, 20)), BarrierProgress::Done);
    assert_eq!(barrier.done_at(), Some(after(act, 20)));
}

#[test]
fn the_reshow_barrier_also_needs_an_image_atlas_item() {
    // The reshow ends at the first frame after its act whose image atlas holds an item; a frame
    // without one, and the atlas's capacity, never end it.
    let act = Instant::now();
    let mut barrier = FrameBarrier::new(act, 40, RESHOW_WAIT, true);
    barrier.observe(after(act, 16), 41, 0);
    assert_eq!(barrier.progress(after(act, 16)), BarrierProgress::Waiting);
    barrier.observe(after(act, 33), 42, 1);
    assert_eq!(barrier.progress(after(act, 33)), BarrierProgress::Done);
    let mut stale = FrameBarrier::new(act, 40, RESHOW_WAIT, true);
    stale.observe(after(act, 16), 40, 3);
    assert_eq!(stale.progress(after(act, 16)), BarrierProgress::Waiting, "no frame after the act");
}

#[test]
fn barriers_expire_at_their_own_bound_and_wake_the_harness() {
    // With no presentation the media-free barrier ends the run at 5 s and the reshow barrier at 10 s,
    // not at the whole-run timeout; each pending deadline is a wake, and a satisfied one is not.
    assert_eq!((MEDIA_FREE_WAIT, RESHOW_WAIT), (Duration::from_secs(5), Duration::from_secs(10)));
    let act = Instant::now();
    for (wait, millis) in [(MEDIA_FREE_WAIT, 5_000), (RESHOW_WAIT, 10_000)] {
        let barrier = FrameBarrier::new(act, 0, wait, wait == RESHOW_WAIT);
        assert_eq!(barrier.deadline(), Some(after(act, millis)));
        assert_eq!(barrier.progress(after(act, millis - 1)), BarrierProgress::Waiting);
        assert_eq!(barrier.progress(after(act, millis)), BarrierProgress::Expired);
        assert!(
            barrier_expired_reason("media-free", wait).contains(&format!("{} s", wait.as_secs()))
        );
    }
    let mut met = FrameBarrier::new(act, 0, MEDIA_FREE_WAIT, false);
    met.observe(after(act, 100), 1, 0);
    assert_eq!(met.deadline(), None);
    assert_eq!(
        met.progress(after(act, 60_000)),
        BarrierProgress::Done,
        "a met barrier never expires"
    );
}

#[test]
fn the_hold_counts_from_its_anchor_however_late_the_checkpoint_resolves() {
    // The 65 s hold starts at the end of the media-free barrier, not when the `switched` checkpoint
    // that follows it resolves; without an anchor it counts from its own start.
    let media_free_end = Instant::now();
    let checkpoint_resolved = after(media_free_end, 40_000);
    assert_eq!(
        anchored_hold_end(Some(media_free_end), checkpoint_resolved, 65_000),
        after(media_free_end, 65_000)
    );
    assert_eq!(
        anchored_hold_end(None, checkpoint_resolved, 65_000),
        after(checkpoint_resolved, 65_000)
    );
    assert_eq!(fresh_after_unix_s(1_000.5, Duration::from_secs(30)), 1_030.5);
}
