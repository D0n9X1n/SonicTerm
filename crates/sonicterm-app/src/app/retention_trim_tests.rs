//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::retention_tests::*;
use super::*;

/// The age rule: 29 s of cover is too recent, 30 s passes every rule up to the renderer, and the
/// hook's rule ignores the age entirely. A covered window with a live device is eligible.
#[test]
fn a_window_covered_for_thirty_seconds_becomes_eligible_and_not_before() {
    let base = test_base();
    let redraw = covered_since(base);
    let at = |secs| base + Duration::from_secs(secs);
    assert_eq!(
        trim_eligibility(&redraw, Some(true), at(29), TrimRule::After30s),
        Err(TrimSkip::TooRecent)
    );
    assert_eq!(trim_eligibility(&redraw, Some(true), at(30), TrimRule::After30s), Ok(()));
    assert_eq!(trim_eligibility(&redraw, Some(true), at(0), TrimRule::IgnoreAge), Ok(()));
    // Without a renderer every earlier rule passed, so the renderer is the only reason left.
    assert_eq!(
        trim_eligibility(&redraw, None, at(30), TrimRule::After30s),
        Err(TrimSkip::NoRenderer)
    );
}

/// Each exclusion is reported as its own reason, whatever the age: not covered, backend-only
/// cover, already trimmed, parked, stopped and a refusing device.
#[test]
fn every_exclusion_reports_its_own_reason() {
    let base = test_base();
    let late = base + Duration::from_secs(60);
    let check = |redraw: &WindowRedrawState, accepts| {
        trim_eligibility(redraw, accepts, late, TrimRule::After30s)
    };
    assert_eq!(check(&WindowRedrawState::default(), Some(true)), Err(TrimSkip::NotOccluded));

    // Backend-only occlusion never stamps a covered stretch.
    let mut backend = WindowRedrawState::default();
    backend.backend_occluded = true;
    assert_eq!(check(&backend, Some(true)), Err(TrimSkip::NotOccluded));

    let mut trimmed = covered_since(base);
    trimmed.trimmed = true;
    assert_eq!(check(&trimmed, Some(true)), Err(TrimSkip::AlreadyTrimmed));

    let mut parked = covered_since(base);
    parked.parked = true;
    assert_eq!(check(&parked, Some(true)), Err(TrimSkip::Parked));

    let mut stopped = covered_since(base);
    stopped.stopped_generation = Some(4);
    assert_eq!(check(&stopped, Some(true)), Err(TrimSkip::DeviceUnavailable));

    assert_eq!(check(&covered_since(base), Some(false)), Err(TrimSkip::DeviceUnavailable));
}

/// A repeated cover keeps the first stamp, so a second event cannot delay the trim; returning to
/// view clears the stamp, the trimmed mark and the released figure, so a later stretch starts over.
#[test]
fn the_covered_stretch_starts_once_and_ends_on_visibility() {
    let base = test_base();
    let mut redraw = covered_since(base);
    redraw.observe_native_occlusion(true, base + Duration::from_secs(20));
    assert_eq!(redraw.occluded_since, Some(base));

    redraw.trimmed = true;
    redraw.trim_released_requested_bytes = 4_096;
    assert!(redraw.observe_native_occlusion(false, base + Duration::from_secs(40)));
    assert_eq!(
        (redraw.occluded_since, redraw.trimmed, redraw.trim_released_requested_bytes),
        (None, false, 0)
    );
}

/// The retention pass reaches the scheduler with no `memory` subscriber installed. A window covered
/// through the App's own occlusion path, on the fake dispatch clock, is too recent at 29 s, passes
/// every rule but the absent renderer at the first pass after 30 s, and a window covered for 20 s
/// then shown again is never reached.
#[test]
fn the_retention_pass_schedules_the_trim_after_thirty_seconds_of_cover() {
    let base = test_base();
    set_fake_now(base);
    let mut app = app_with_private_pools();
    app.__test_set_dispatch_clock(fake_now);
    app.__test_seed_tab("covered");
    let main = app.main_window_id.expect("the seeded main window");
    let child = app.__test_seed_child_window(&["shown again"]);
    app.handle_window_occlusion(main, true);
    app.handle_window_occlusion(child, true);
    set_fake_now(base + Duration::from_secs(20));
    app.handle_window_occlusion(child, false);

    let pass = |app: &mut App, secs: u64| {
        app.test_scheduler_trims.clear();
        app.last_retention_sample = None;
        // The return value reports only the debug walk, which no subscriber here enables; the
        // recorded decisions are what show the pass reached the scheduler.
        app.sample_pane_retention(base + Duration::from_secs(secs));
        let mut decisions = app.test_scheduler_trims.clone();
        decisions.sort_by_key(|(window_id, _)| *window_id != main);
        decisions
    };
    assert_eq!(
        pass(&mut app, 29),
        [
            (main, TrimDecision::Skipped(TrimSkip::TooRecent)),
            (child, TrimDecision::Skipped(TrimSkip::NotOccluded)),
        ]
    );
    assert_eq!(
        pass(&mut app, 30),
        [
            (main, TrimDecision::Skipped(TrimSkip::NoRenderer)),
            (child, TrimDecision::Skipped(TrimSkip::NotOccluded)),
        ]
    );
    assert_eq!(app.trim_seq, 0, "nothing was released without a renderer");
}

/// The hook and the scheduler share one body: the hook applies every rule but the age, reports each
/// exclusion as `Skipped` and releases nothing, and runs no retention pass, so the sample clock and
/// every window's panes are exactly as they were.
#[test]
fn the_trim_hook_shares_the_scheduler_rules_and_runs_no_retention_pass() {
    let base = test_base();
    set_fake_now(base);
    let mut app = app_with_private_pools();
    app.__test_set_dispatch_clock(fake_now);
    app.__test_seed_tab("covered");
    let main = app.main_window_id.expect("the seeded main window");
    let sampled_at = base - Duration::from_secs(5);
    app.last_retention_sample = Some(sampled_at);
    let panes_before: Vec<u64> = app.windows[&main].panes.keys().copied().collect();

    assert_eq!(app.__trim_covered_now(main), TrimDecision::Skipped(TrimSkip::NotOccluded));
    assert_eq!(
        app.__trim_covered_now(WindowId::from(7)),
        TrimDecision::Skipped(TrimSkip::NoWindow)
    );
    app.handle_window_occlusion(main, true);
    // Covered this instant: the hook ignores the age, so only the absent renderer remains.
    assert_eq!(app.__trim_covered_now(main), TrimDecision::Skipped(TrimSkip::NoRenderer));
    app.windows.get_mut(&main).expect("main").redraw.trimmed = true;
    assert_eq!(app.__trim_covered_now(main), TrimDecision::Skipped(TrimSkip::AlreadyTrimmed));

    assert_eq!(app.trim_seq, 0);
    assert_eq!(app.last_trim_source, None);
    assert!(app.test_scheduler_trims.is_empty(), "the hook is not the scheduler");
    assert_eq!(app.last_retention_sample, Some(sampled_at), "no retention pass ran");
    let panes_after: Vec<u64> = app.windows[&main].panes.keys().copied().collect();
    assert_eq!(panes_after, panes_before);
    assert_eq!(app.windows.len(), 1, "no window was created or removed");
}
