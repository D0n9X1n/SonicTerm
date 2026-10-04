use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_gpu::core::SurfaceRetryReason;
use winit::window::WindowId;

use crate::app::frame_counters::DeferRule;
use crate::app::redraw::{DueCause, FrameSettlement, RedrawCause};
use crate::app::spawn_pane::{pack_sync_deadline, read_published_sync, sync_word_of};
use crate::app::{App, PaneState};
use sonicterm_vt::vt::SyncState;

/// A counting App with a main window and a child whose tab 0 is active and tab 1 hidden. Both
/// windows have presented once, have old pacing clocks and no unacknowledged output, so only the
/// synchronized-output rule can hold them; `base` is a test instant well after the shared deadline origin.
fn held_owners() -> (App, WindowId, WindowId, Instant) {
    let base = crate::app::sync_clock::origin() + Duration::from_secs(5);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    for id in [main, child] {
        let window = app.windows.get_mut(&id).unwrap();
        window.last_render = base - Duration::from_secs(1);
        window.stream_clock = base - Duration::from_secs(1);
        window.redraw.last_present = Some(base - Duration::from_secs(1));
        // Acknowledge the seeded output, so streaming pacing never holds these windows.
        for pane in window.panes.values_mut() {
            pane.observed_output_generation = pane.output_generation.load(Ordering::Acquire);
        }
    }
    (app, main, child, base)
}

/// The pane shown by tab `tab_index` of window `id`.
fn pane_of(app: &App, id: WindowId, tab_index: usize) -> u64 {
    app.windows[&id].tab_states[tab_index].active_pane
}

/// Split window `id`'s active tab and add a parser-only pane; returns its id.
fn add_visible_pane(app: &mut App, id: WindowId) -> u64 {
    let parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(80, 24));
    let new_id = crate::app::next_pane_id();
    let window = app.windows.get_mut(&id).unwrap();
    let tab = window.tabs.active_index();
    let focus = window.tab_states[tab].active_pane;
    assert!(window.tab_states[tab].tree.split(
        focus,
        sonicterm_cfg::keymap::Direction::Right,
        new_id
    ));
    window.panes.insert(new_id, PaneState::new(Arc::new(Mutex::new(parser)), None));
    new_id
}

/// Publish an open update of `epoch` on the pane, holding until `deadline`, as its worker would:
/// the tagged deadline first, then the word with the pane's current reset count.
fn hold(app: &App, id: WindowId, pane: u64, epoch: u64, deadline: Instant) {
    let pane = &app.windows[&id].panes[&pane];
    let resets = pane.sync_resets.load(Ordering::Relaxed);
    pane.sync_deadline_word.store(pack_sync_deadline(epoch, deadline), Ordering::Relaxed);
    pane.sync_word.store(sync_word_of(SyncState { set: true, epoch, resets }), Ordering::Release);
}

/// Publish the end of the pane's open update: the set bit clears and one reset is counted.
fn release(app: &App, id: WindowId, pane: u64) {
    let pane = &app.windows[&id].panes[&pane];
    let resets = pane.sync_resets.fetch_add(1, Ordering::Relaxed) + 1;
    let epoch = read_published_sync(&pane.sync_word, &pane.sync_deadline_word).epoch;
    pane.sync_word.store(sync_word_of(SyncState { set: false, epoch, resets }), Ordering::Release);
}

/// Mark `cause` and run admission at `at`; an admitted attempt is settled with `outcome`, and
/// the pacing clocks then move back so streaming pacing never holds the next attempt.
fn attempt(
    app: &mut App,
    id: WindowId,
    cause: RedrawCause,
    at: Instant,
    outcome: FrameSettlement,
) -> bool {
    app.mark_window_redraw(id, cause);
    let admitted = app.begin_window_redraw(id, at);
    if admitted {
        let snapshot = app.snapshot_window_redraw_at(id, at).unwrap();
        app.finish_window_redraw(id, &snapshot, outcome, at);
        // Streaming pacing allows one frame per period from this attempt; it is not under test, so
        // the clocks move back and only the synchronized-output rule can hold the next attempt.
        let window = app.windows.get_mut(&id).unwrap();
        window.last_render = at - Duration::from_secs(1);
        window.stream_clock = at - Duration::from_secs(1);
    }
    admitted
}

/// Milliseconds after `base`.
fn at_ms(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

/// The window's counted Sync deferrals.
fn defer_sync(app: &App, id: WindowId) -> u64 {
    app.windows[&id].redraw.frame_counters.as_deref().unwrap().defer_sync
}

/// The deadline of the window's armed Frame wake, if any.
fn frame_wake(app: &App, id: WindowId, now: Instant) -> Option<Instant> {
    app.frame_due_work_at(now)
        .into_iter()
        .find(|work| work.owner == Some(id) && work.cause == DueCause::Frame)
        .map(|work| work.deadline)
}

/// While a visible pane holds, output, cursor and expose frames all defer by `Sync` without
/// collecting; the Frame wake is armed at the pane deadline; one nanosecond before it still holds,
/// and at it the silent stuck update is admitted with no new byte. A reset admits at once.
#[test]
fn a_held_visible_pane_defers_every_unforced_frame_until_its_deadline() {
    for child_owner in [false, true] {
        let (mut app, main, child, base) = held_owners();
        let owner = if child_owner { child } else { main };
        let pane = pane_of(&app, owner, 0);
        hold(&app, owner, pane, 1, at_ms(base, 100));
        for cause in [RedrawCause::Output, RedrawCause::Cursor, RedrawCause::Expose] {
            assert!(
                !attempt(&mut app, owner, cause, base, FrameSettlement::Presented),
                "{cause:?}"
            );
            assert_eq!(app.windows[&owner].redraw.deferred_rule, Some(DeferRule::Sync));
            assert!(app.windows[&owner].redraw.attempt_causes.is_none(), "nothing collected");
        }
        assert_eq!(defer_sync(&app, owner), 3);
        assert_eq!(frame_wake(&app, owner, base), Some(at_ms(base, 100)));
        let early = at_ms(base, 100) - Duration::from_nanos(1);
        assert!(!attempt(&mut app, owner, RedrawCause::Expose, early, FrameSettlement::Presented));
        assert!(attempt(
            &mut app,
            owner,
            RedrawCause::Expose,
            at_ms(base, 100),
            FrameSettlement::Presented
        ));

        hold(&app, owner, pane, 2, at_ms(base, 400));
        assert!(!attempt(
            &mut app,
            owner,
            RedrawCause::Output,
            at_ms(base, 110),
            FrameSettlement::Presented
        ));
        release(&app, owner, pane);
        assert!(attempt(
            &mut app,
            owner,
            RedrawCause::Output,
            at_ms(base, 111),
            FrameSettlement::Presented
        ));
    }
}

/// A reset followed by a new set before any frame releases the pane for exactly one frame: an
/// `AtlasRetry` or surface timeout keeps the release, and only a presented frame spends it.
#[test]
fn an_unpresented_reset_releases_the_pane_until_a_frame_presents() {
    let (mut app, main, _, base) = held_owners();
    let pane = pane_of(&app, main, 0);
    hold(&app, main, pane, 1, at_ms(base, 400));
    release(&app, main, pane);
    hold(&app, main, pane, 2, at_ms(base, 400));
    let retry = FrameSettlement::Retry(RedrawCause::AtlasRetry);
    assert!(attempt(&mut app, main, RedrawCause::Output, base, retry));
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 1),
        FrameSettlement::Presented
    ));
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 2),
        FrameSettlement::Presented
    ));
}

/// Overlapping updates in two visible panes (A 0–150 ms, B 100–250 ms) present at 150 ms at the
/// latest; after that frame presents, B may hold a new stretch. Alternating renewed epochs cannot
/// hold the window past 150 ms of stretch, and a failed attempt does not restart the stretch.
#[test]
fn overlapping_and_renewed_updates_are_capped_at_150_ms_of_stretch() {
    let (mut app, main, _, base) = held_owners();
    let first = pane_of(&app, main, 0);
    let second = add_visible_pane(&mut app, main);
    hold(&app, main, first, 1, at_ms(base, 150));
    assert!(!attempt(&mut app, main, RedrawCause::Output, base, FrameSettlement::Presented));
    hold(&app, main, second, 1, at_ms(base, 250));
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 149),
        FrameSettlement::Presented
    ));
    assert_eq!(frame_wake(&app, main, at_ms(base, 149)), Some(at_ms(base, 150)));
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 150),
        FrameSettlement::Presented
    ));
    assert_eq!(app.windows[&main].redraw.sync_stretch_start, None);
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 160),
        FrameSettlement::Presented
    ));

    let (mut app, main, _, base) = held_owners();
    let first = pane_of(&app, main, 0);
    let second = add_visible_pane(&mut app, main);
    hold(&app, main, first, 1, at_ms(base, 100));
    assert!(!attempt(&mut app, main, RedrawCause::Output, base, FrameSettlement::Presented));
    for (step, renew_ms) in [(2_u64, 60_u64), (3, 120)] {
        hold(&app, main, second, step, at_ms(base, renew_ms + 100));
        hold(&app, main, first, step, at_ms(base, renew_ms + 100));
        assert!(!attempt(
            &mut app,
            main,
            RedrawCause::Output,
            at_ms(base, renew_ms),
            FrameSettlement::Presented
        ));
    }
    // A failed attempt at the cap keeps the stretch, so the retry is admitted too.
    let failed = FrameSettlement::Retry(RedrawCause::AtlasRetry);
    assert!(attempt(&mut app, main, RedrawCause::Output, at_ms(base, 150), failed));
    assert_eq!(app.windows[&main].redraw.sync_stretch_start, Some(base));
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 151),
        FrameSettlement::Presented
    ));
}

/// The first frame, a pending `Visibility`, `DeviceRecovered`, an armed resize and a surface
/// recovery all present the live grid while a pane holds; focus-style chrome, a topology change
/// and an atlas retry are held. Resize and surface-recovery obligations survive failed attempts
/// and clear only when a frame presents; a surface timeout arms nothing.
#[test]
fn only_the_must_run_causes_are_forced_through_a_hold() {
    let forced: [(&str, fn(&mut App, WindowId)); 5] = [
        ("first frame", |app, id| app.windows.get_mut(&id).unwrap().redraw.last_present = None),
        ("visibility", |app, id| app.mark_window_redraw(id, RedrawCause::Visibility)),
        ("device recovered", |app, id| app.mark_window_redraw(id, RedrawCause::DeviceRecovered)),
        ("resize", |app, id| app.windows.get_mut(&id).unwrap().redraw.resize_pending = true),
        ("surface recovery", |app, id| {
            app.windows.get_mut(&id).unwrap().redraw.surface_recovery_pending = true;
        }),
    ];
    for (name, force) in forced {
        let (mut app, main, _, base) = held_owners();
        hold(&app, main, pane_of(&app, main, 0), 1, at_ms(base, 100));
        force(&mut app, main);
        assert!(
            attempt(&mut app, main, RedrawCause::Expose, base, FrameSettlement::Presented),
            "{name}"
        );
    }
    for cause in [RedrawCause::Chrome, RedrawCause::Topology, RedrawCause::AtlasRetry] {
        let (mut app, main, _, base) = held_owners();
        hold(&app, main, pane_of(&app, main, 0), 1, at_ms(base, 100));
        assert!(!attempt(&mut app, main, cause, base, FrameSettlement::Presented), "{cause:?}");
    }

    let (mut app, main, _, base) = held_owners();
    hold(&app, main, pane_of(&app, main, 0), 1, at_ms(base, 100));
    app.windows.get_mut(&main).unwrap().redraw.resize_pending = true;
    let failures = [
        FrameSettlement::Retry(RedrawCause::AtlasRetry),
        FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        FrameSettlement::Failed,
    ];
    for (step, outcome) in failures.into_iter().enumerate() {
        let at = at_ms(base, 40 * step as u64);
        app.windows.get_mut(&main).unwrap().redraw.timeout_pending = false;
        assert!(attempt(&mut app, main, RedrawCause::Expose, at, outcome), "{outcome:?}");
        assert!(app.windows[&main].redraw.resize_pending, "{outcome:?} keeps the resize");
    }
    app.windows.get_mut(&main).unwrap().redraw.timeout_pending = false;
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Expose,
        at_ms(base, 130),
        FrameSettlement::Presented
    ));
    assert!(!app.windows[&main].redraw.resize_pending);

    for reason in [
        SurfaceRetryReason::Outdated,
        SurfaceRetryReason::Suboptimal,
        SurfaceRetryReason::SurfaceLost,
    ] {
        let (mut app, main, _, base) = held_owners();
        app.windows.get_mut(&main).unwrap().redraw.last_present = None;
        assert!(attempt(
            &mut app,
            main,
            RedrawCause::Expose,
            base,
            FrameSettlement::SurfaceRetry(reason)
        ));
        app.windows.get_mut(&main).unwrap().redraw.last_present = Some(base);
        hold(&app, main, pane_of(&app, main, 0), 1, at_ms(base, 100));
        assert!(app.windows[&main].redraw.surface_recovery_pending, "{reason:?}");
        assert!(attempt(
            &mut app,
            main,
            RedrawCause::Expose,
            at_ms(base, 1),
            FrameSettlement::Presented
        ));
        assert!(!app.windows[&main].redraw.surface_recovery_pending, "{reason:?}");
    }
    let (mut app, main, _, base) = held_owners();
    app.windows.get_mut(&main).unwrap().redraw.last_present = None;
    let timeout = FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout);
    assert!(attempt(&mut app, main, RedrawCause::Expose, base, timeout));
    assert!(!app.windows[&main].redraw.surface_recovery_pending, "a surface timeout is not forced");
}

/// A held pane in a background tab neither holds its window (visible output is admitted) nor,
/// once it ends, requests a frame through the visible-output filter.
#[test]
fn a_held_background_pane_neither_holds_nor_wakes_its_window() {
    let (mut app, _, child, base) = held_owners();
    let background = pane_of(&app, child, 1);
    hold(&app, child, background, 1, at_ms(base, 100));
    assert!(attempt(&mut app, child, RedrawCause::Output, base, FrameSettlement::Presented));
    *app.windows[&child].panes[&background].redraw_target.lock() = Some(child);
    release(&app, child, background);
    app.windows[&child].panes[&background].output_generation.fetch_add(1, Ordering::Release);
    let before = crate::app::window_state::window_redraw_requests();
    app.service_output_event(
        crate::app::output_event::OutputEvent::Pane { window_id: child, pane_id: background },
        base,
    );
    assert_eq!(crate::app::window_state::window_redraw_requests(), before);
}

/// A keystroke during an update, with a second pane holding an overlapping update, is admitted
/// at most 150 ms after the stretch's first Sync deferral, and that attempt consumes its input.
#[test]
fn typed_echo_inside_an_update_waits_at_most_150_ms() {
    let (mut app, main, _, base) = held_owners();
    let first = pane_of(&app, main, 0);
    let second = add_visible_pane(&mut app, main);
    hold(&app, main, first, 1, at_ms(base, 400));
    hold(&app, main, second, 1, at_ms(base, 500));
    assert!(!attempt(&mut app, main, RedrawCause::Input, base, FrameSettlement::Settled));
    assert!(app.windows[&main].redraw.input_pending());
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Input,
        at_ms(base, 149),
        FrameSettlement::Settled
    ));
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Input,
        at_ms(base, 150),
        FrameSettlement::Settled
    ));
    assert!(!app.windows[&main].redraw.input_pending(), "the admitted attempt consumed the input");
}

/// A no-draw update (`Settled` or `Cached`) spends its reset credit, so the next update's first
/// painted row and an unrelated redraw are held; a failed outcome leaves the credit, and the pane
/// still releases.
#[test]
fn a_no_draw_update_spends_its_reset_credit() {
    for spent in [FrameSettlement::Settled, FrameSettlement::Cached] {
        let (mut app, main, _, base) = held_owners();
        let pane = pane_of(&app, main, 0);
        hold(&app, main, pane, 1, at_ms(base, 400));
        release(&app, main, pane);
        assert!(attempt(&mut app, main, RedrawCause::Output, base, spent), "{spent:?}");
        assert_eq!(app.windows[&main].panes[&pane].presented_sync_resets, 1);
        hold(&app, main, pane, 2, at_ms(base, 400));
        assert!(!attempt(
            &mut app,
            main,
            RedrawCause::Expose,
            at_ms(base, 1),
            FrameSettlement::Presented
        ));
        assert_eq!(app.windows[&main].redraw.deferred_rule, Some(DeferRule::Sync));
    }
    let (mut app, main, _, base) = held_owners();
    let pane = pane_of(&app, main, 0);
    hold(&app, main, pane, 1, at_ms(base, 400));
    release(&app, main, pane);
    hold(&app, main, pane, 2, at_ms(base, 400));
    let failed = FrameSettlement::Retry(RedrawCause::AtlasRetry);
    assert!(attempt(&mut app, main, RedrawCause::Output, base, failed));
    assert_eq!(app.windows[&main].panes[&pane].presented_sync_resets, 0);
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Expose,
        at_ms(base, 1),
        FrameSettlement::Presented
    ));
}

/// A successful no-draw outcome (`Settled`, which an offscreen-only `Noop` plan settles as) ends
/// the stretch, so the next update is held for its full stretch; a surface timeout keeps it.
#[test]
fn a_settled_outcome_ends_the_stretch_and_a_failure_keeps_it() {
    let (mut app, main, _, base) = held_owners();
    let pane = pane_of(&app, main, 0);
    hold(&app, main, pane, 1, at_ms(base, 400));
    assert!(!attempt(&mut app, main, RedrawCause::Output, base, FrameSettlement::Settled));
    let timeout = FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout);
    assert!(attempt(&mut app, main, RedrawCause::Output, at_ms(base, 150), timeout));
    assert_eq!(app.windows[&main].redraw.sync_stretch_start, Some(base), "a failure keeps it");
    app.windows.get_mut(&main).unwrap().redraw.timeout_pending = false;
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 200),
        FrameSettlement::Settled
    ));
    assert_eq!(app.windows[&main].redraw.sync_stretch_start, None);
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 210),
        FrameSettlement::Settled
    ));
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 359),
        FrameSettlement::Settled
    ));
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 360),
        FrameSettlement::Settled
    ));
}

/// A DECSET that lands between admission and collection abandons the collected frame under its
/// guards: no settle, no clock or retry-floor change, pending causes and dirt kept, `defer_sync`
/// counted and `deferred_rule == Sync`. Both adapters run this before applying receipts.
#[test]
fn the_recheck_under_the_guards_abandons_a_newly_held_frame() {
    let (mut app, main, _, base) = held_owners();
    let pane = pane_of(&app, main, 0);
    app.mark_window_redraw(main, RedrawCause::Output);
    assert!(app.begin_window_redraw(main, base));
    let _snapshot = app.snapshot_window_redraw_at(main, base).unwrap();
    let parser = app.windows[&main].panes[&pane].parser.clone();
    parser.lock().advance(b"\x1b[?2026hrow");
    hold(&app, main, pane, 1, at_ms(base, 100));
    let (last_render, stream_clock, retry) = {
        let window = &app.windows[&main];
        (window.last_render, window.stream_clock, window.retry_not_before)
    };
    let states = vec![(pane, parser.lock().synchronized_output())];
    assert!(app.abandon_synchronized_frame(main, &states, at_ms(base, 1)));
    let window = &app.windows[&main];
    assert_eq!(
        (window.last_render, window.stream_clock, window.retry_not_before),
        (last_render, stream_clock, retry)
    );
    assert!(window.redraw.has_pending(), "nothing was settled");
    assert!(window.redraw.deferred && !window.redraw.request_in_flight);
    assert_eq!(window.redraw.deferred_rule, Some(DeferRule::Sync));
    assert_eq!(defer_sync(&app, main), 1);

    let scan = |source: &str| {
        let source = source.replace("\r\n", "\n");
        let collect = source.find("sources.try_collect(").expect("collects");
        let recheck = source.find("abandon_synchronized_frame(").expect("rechecks");
        let receipts = source.find("reconcile_and_apply_receipts(").expect("applies receipts");
        assert!(collect < recheck && recheck < receipts);
    };
    scan(include_str!("window_event.rs"));
    scan(include_str!("child_window_redraw.rs"));
}

/// Only a changed surface size arms `resize_pending`: a `Resized` at the configured size
/// (`Unchanged`) or a rejected one leaves a held window held, and a changed one forces its frame.
/// The main and child `Resized` handlers both classify the resize, then note it.
#[test]
fn only_a_changed_resize_forces_a_frame_through_a_hold() {
    use sonicterm_gpu::core::ResizeOutcome;
    let cases = [
        (ResizeOutcome::Unchanged, false),
        (ResizeOutcome::Rejected, false),
        (ResizeOutcome::Changed, true),
    ];
    for (outcome, forced) in cases {
        let (mut app, main, _, base) = held_owners();
        hold(&app, main, pane_of(&app, main, 0), 1, at_ms(base, 100));
        app.windows.get_mut(&main).unwrap().redraw.note_resize(outcome);
        let admitted =
            attempt(&mut app, main, RedrawCause::Expose, base, FrameSettlement::Presented);
        assert_eq!(admitted, forced, "{outcome:?}");
    }
    for source in [include_str!("window_event.rs"), include_str!("child_window.rs")] {
        let source = source.replace("\r\n", "\n");
        let classify = source.find("try_resize_outcome(").expect("the handler classifies");
        let note = source.find("redraw.note_resize(").expect("the handler notes the outcome");
        assert!(classify < note);
    }
}

/// The worker hook test 12 drives runs the production publisher and the worker's decision: bytes
/// inside an update queue no output event and publish a held word with its own deadline, and the
/// reset queues exactly one event for the pane's window.
#[test]
fn the_pane_worker_hook_holds_an_update_and_releases_it_once() {
    let (app, main, _, base) = held_owners();
    let pane = pane_of(&app, main, 0);
    let mut worker = app.__test_pane_worker(main, pane).expect("the pane exists");
    assert!(worker.batch(b"\x1b[?2026h", base).is_empty());
    assert!(worker.batch(b"row", at_ms(base, 1)).is_empty());
    let published = {
        let pane = &app.windows[&main].panes[&pane];
        read_published_sync(&pane.sync_word, &pane.sync_deadline_word)
    };
    assert!(published.set);
    assert_eq!(published.deadline, Some(base + crate::app::spawn_pane::SYNC_OUTPUT_TIMEOUT));
    assert_eq!(worker.batch(b"\x1b[?2026l", at_ms(base, 2)), [main]);
}

/// The lock-free deadline read is approximate across 65,536 epochs: a later epoch's deadline whose
/// 16-bit tag aliases the word's epoch is accepted. A later epoch's deadline is never earlier, so
/// the alias only extends a hold, and the window cap still bounds it: the frame is held past the
/// true deadline and admitted at the cap, never released early.
#[test]
fn an_aliased_deadline_extends_a_hold_only_up_to_the_cap() {
    let (mut app, main, _, base) = held_owners();
    let pane = pane_of(&app, main, 0);
    hold(&app, main, pane, 1, at_ms(base, 100));
    let aliased = pack_sync_deadline(1 + (1 << 16), at_ms(base, 400));
    app.windows[&main].panes[&pane].sync_deadline_word.store(aliased, Ordering::Relaxed);
    assert!(!attempt(&mut app, main, RedrawCause::Output, base, FrameSettlement::Presented));
    let past_true_deadline = at_ms(base, 120);
    assert!(!attempt(
        &mut app,
        main,
        RedrawCause::Output,
        past_true_deadline,
        FrameSettlement::Presented
    ));
    assert!(attempt(
        &mut app,
        main,
        RedrawCause::Output,
        at_ms(base, 150),
        FrameSettlement::Presented
    ));
}

/// Every production source file under `src/`, CRLF-normalized, with its path relative to `src/`.
fn production_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && !path.to_string_lossy().ends_with("_tests.rs")
            {
                let name = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.push((name, std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n")));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

/// No production path commits a renderer size through the bool `try_resize`, which cannot say
/// whether the surface changed: every one goes through `try_resize_outcome`. The DPI transition
/// records its outcome as an obligation, which a later same-size `Resized` cannot clear.
#[test]
fn every_renderer_resize_reports_whether_the_surface_changed() {
    let sources = production_sources();
    assert!(sources.len() > 50, "the production sources were found");
    for (name, source) in &sources {
        // `retention.rs` resizes a resource reservation, not a renderer surface.
        if name.ends_with("app/retention.rs") {
            continue;
        }
        let bypasses = source.replace("try_resize_outcome(", "").matches("try_resize(").count();
        assert_eq!(bypasses, 0, "{name} commits a renderer size without its outcome");
    }
    let setup = &sources.iter().find(|(name, _)| name.ends_with("app/window_setup.rs")).unwrap().1;
    let dpi = setup.split("fn apply_window_dpi_transition(").nth(1).unwrap();
    let dpi = &dpi[..dpi.find("\n}\n").unwrap()];
    let resize = dpi.find("try_resize_outcome(").expect("the DPI transition classifies its resize");
    let note = dpi.find("redraw.note_resize(").expect("and records the obligation");
    assert!(resize < note, "{dpi}");
    let minimum = setup.split("fn apply_window_state_minimum(").nth(1).unwrap();
    assert!(minimum[..minimum.find("\n}\n").unwrap()].contains("redraw.note_resize("));
}

/// A size committed by a DPI change keeps its obligation through the `Resized` that follows at the
/// committed size (`Unchanged`), so the next frame is forced while a pane holds, on both roles.
#[test]
fn a_dpi_resize_keeps_its_obligation_through_a_same_size_resized() {
    use sonicterm_gpu::core::ResizeOutcome;
    for child_owner in [false, true] {
        let (mut app, main, child, base) = held_owners();
        let owner = if child_owner { child } else { main };
        hold(&app, owner, pane_of(&app, owner, 0), 1, at_ms(base, 100));
        let redraw = &mut app.windows.get_mut(&owner).unwrap().redraw;
        redraw.note_resize(ResizeOutcome::Changed);
        redraw.note_resize(ResizeOutcome::Unchanged);
        assert!(redraw.resize_pending, "child={child_owner}");
        let forced =
            attempt(&mut app, owner, RedrawCause::Expose, base, FrameSettlement::Presented);
        assert!(forced, "child={child_owner}: the committed size is presented through the hold");
    }
}
