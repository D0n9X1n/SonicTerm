//! Pins the worker half of the renderer-waiting handshake on a fake clock and an explicit park
//! token: the store-before-unpark service contract, the fixed park deadline, and the grant count.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use super::*;
use crate::app::redraw::redraw_dispatch_tests::{fake_now, set_fake_now, test_base};

/// One scripted return of [`FakePark::park_until`].
pub(in crate::app) enum Step {
    /// A window serves `generation` while the worker parks; its unpark stores the token.
    Serve(u64),
    /// An effective advance to an older generation: it stores a token that does not serve the ask.
    StaleServe(u64),
    /// A spurious return after the duration, before the deadline.
    Spurious(Duration),
    /// Runs while the worker is parked, then the park times out at its deadline.
    Probe(Box<dyn FnMut()>),
    /// The park times out at its deadline plus the scheduler overshoot.
    Timeout(Duration),
}

/// A park clock on the shared fake dispatch clock with an explicit one-token park model.
///
/// `max_calls` bounds the parks; one more panics, so an unbounded wait fails instead of hanging.
pub(in crate::app) struct FakePark {
    /// Whether an unpark is stored and not yet consumed by a park.
    token: std::rc::Rc<Cell<bool>>,
    /// The generation the current `Serve` step is serving, read by the unpark hook.
    serving: std::rc::Rc<Cell<u64>>,
    script: VecDeque<Step>,
    /// Parks so far.
    pub(in crate::app) calls: u32,
    max_calls: u32,
    /// Each park's deadline, in call order.
    pub(in crate::app) deadlines: Vec<Instant>,
    /// Clock reads so far.
    reads: u32,
    /// Clock jumps: before read `index` returns, the fake clock moves to the instant.
    jumps: VecDeque<(u32, Instant)>,
}

impl FakePark {
    /// A park for `handshake` that follows `script` and allows `max_calls` parks.
    ///
    /// It installs this thread's unpark hook: an unpark of `handshake` asserts that the serve it
    /// belongs to is already stored, then stores the token.
    pub(in crate::app) fn new(
        handshake: &Arc<ParserYield>,
        script: impl IntoIterator<Item = Step>,
        max_calls: u32,
    ) -> Self {
        let token = std::rc::Rc::new(Cell::new(false));
        let serving = std::rc::Rc::new(Cell::new(0));
        let target = Arc::as_ptr(handshake) as usize;
        let (hook_token, hook_serving) = (std::rc::Rc::clone(&token), std::rc::Rc::clone(&serving));
        set_unpark_hook(Some(Box::new(move |unparked: &ParserYield| {
            if std::ptr::from_ref(unparked) as usize != target {
                return;
            }
            let served = unparked.served.load(std::sync::atomic::Ordering::Acquire);
            assert!(
                served >= hook_serving.get(),
                "unparked before the serve was stored: served {served} < {}",
                hook_serving.get()
            );
            hook_token.set(true);
        })));
        Self {
            token,
            serving,
            script: script.into_iter().collect(),
            calls: 0,
            max_calls,
            deadlines: Vec::new(),
            reads: 0,
            jumps: VecDeque::new(),
        }
    }

    /// Move the fake clock to `to` just before read number `read` (counted from 0) returns.
    pub(in crate::app) fn jump_at_read(mut self, read: u32, to: Instant) -> Self {
        self.jumps.push_back((read, to));
        self
    }

    /// Serve `generation` of `handshake` as a window would, through the installed hook.
    fn serve(&self, handshake_generation: u64, serve: impl FnOnce(u64)) {
        self.serving.set(handshake_generation);
        serve(handshake_generation);
    }
}

impl Drop for FakePark {
    // Lifecycle: the unpark hook captures this park's token; it is removed with the park.
    fn drop(&mut self) {
        set_unpark_hook(None);
    }
}

impl YieldClock for FakePark {
    fn now(&mut self) -> Instant {
        if self.jumps.front().is_some_and(|(read, _)| *read == self.reads) {
            let (_, to) = self.jumps.pop_front().expect("front checked");
            set_fake_now(to);
        }
        self.reads += 1;
        fake_now()
    }

    fn park_until(&mut self, deadline: Instant) {
        self.calls += 1;
        assert!(self.calls <= self.max_calls, "park {} is beyond {}", self.calls, self.max_calls);
        self.deadlines.push(deadline);
        if self.token.replace(false) {
            // A stored token returns the park at once, as `park_timeout` does.
            return;
        }
        match self.script.pop_front().expect("the park outlived its script") {
            Step::Serve(generation) | Step::StaleServe(generation) => {
                let handshake = SERVE_TARGET.with(|slot| slot.borrow().clone());
                let handshake = handshake.expect("a serving test names its handshake");
                self.serve(generation, |generation| {
                    handshake.serve_outstanding(generation);
                });
                assert!(self.token.replace(false), "an effective serve stores the token");
            }
            Step::Spurious(after) => {
                assert!(fake_now() + after < deadline, "a spurious return precedes the deadline");
                set_fake_now(fake_now() + after);
            }
            Step::Probe(mut probe) => {
                probe();
                set_fake_now(deadline);
            }
            Step::Timeout(overshoot) => set_fake_now(deadline + overshoot),
        }
    }
}

thread_local! {
    /// The handshake a `Serve` step serves; tests set it with [`serve_target`].
    static SERVE_TARGET: RefCell<Option<Arc<ParserYield>>> = const { RefCell::new(None) };
}

/// Name the handshake this thread's `Serve` steps serve.
pub(in crate::app) fn serve_target(handshake: &Arc<ParserYield>) {
    SERVE_TARGET.with(|slot| *slot.borrow_mut() = Some(Arc::clone(handshake)));
}

/// A handshake on a fake clock at a fresh base, with its serve log emptied.
fn handshake_at_base() -> (Arc<ParserYield>, Instant) {
    let base = test_base();
    set_fake_now(base);
    let handshake = Arc::new(ParserYield::new());
    serve_target(&handshake);
    let _ = take_serve_log();
    (handshake, base)
}

/// Run one `yield_step` toward target 7 and return the outcome and every send it made.
fn step(
    handshake: &ParserYield,
    granted: &mut u64,
    held: Option<Instant>,
    park: &mut FakePark,
) -> (YieldOutcome, Vec<(u8, u64, Instant)>) {
    let mut sent = Vec::new();
    let outcome = yield_step(
        handshake,
        granted,
        held,
        || Some(7u8),
        |target, generation, deadline| {
            sent.push((target, generation, deadline));
            true
        },
        park,
    );
    (outcome, sent)
}

/// S1: a collection that served `g` before the worker's check leaves the worker nothing to wait
/// for. It still sends, but never parks, and its wait sample is the fake clock's elapsed time
/// across the send, zero here. The serve log holds the one effective advance.
#[test]
fn a_request_already_served_sends_but_never_parks() {
    let (handshake, base) = handshake_at_base();
    let generation = handshake.request();
    handshake.serve_outstanding(generation);
    let mut park = FakePark::new(&handshake, [], 0);
    let (outcome, sent) = step(&handshake, &mut 0, None, &mut park);
    assert_eq!(sent, [(7, generation, base + PARSER_YIELD_PARK)]);
    assert_eq!(
        outcome,
        YieldOutcome::Sent(YieldWait {
            wait: Duration::ZERO,
            parks: 0,
            timed_out: false,
            overshoot: None
        })
    );
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!((log[0].old, log[0].new), (0, generation));
}

/// S2: a serve that lands between the worker's `served` check and its park stores the token
/// after the generation, so the one park returns through the token and the worker is served.
/// The unpark hook asserts the store came first.
#[test]
fn a_serve_between_the_check_and_the_park_returns_through_the_token() {
    let (handshake, _base) = handshake_at_base();
    let generation = handshake.request();
    let mut park = FakePark::new(&handshake, [Step::Serve(generation)], 1);
    let (outcome, sent) = step(&handshake, &mut 0, None, &mut park);
    assert_eq!(sent.len(), 1);
    assert_eq!(park.calls, 1);
    assert_eq!(
        outcome,
        YieldOutcome::Sent(YieldWait {
            wait: Duration::ZERO,
            parks: 1,
            timed_out: false,
            overshoot: None
        })
    );
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].unparked, "an effective serve unparks the worker");
}

/// S2, real clock: an unpark stored before the park makes `park_until` return at once; the
/// production clock waits on the thread's park token, not on a sleep to the deadline.
#[test]
fn the_thread_clock_returns_on_a_token_stored_before_the_park() {
    let handshake = ParserYield::new();
    handshake.register_worker();
    let generation = handshake.request();
    assert!(handshake.serve_outstanding(generation));
    let started = Instant::now();
    ThreadYieldClock.park_until(started + Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(1), "the stored token ends the park");
}

/// S3: a spurious return re-parks to the same deadline; the wait ends there with one timeout.
#[test]
fn a_spurious_wake_never_extends_the_deadline() {
    let (handshake, base) = handshake_at_base();
    handshake.request();
    let script = [Step::Spurious(Duration::from_micros(500)), Step::Timeout(Duration::ZERO)];
    let mut park = FakePark::new(&handshake, script, 2);
    let (outcome, _) = step(&handshake, &mut 0, None, &mut park);
    let deadline = base + PARSER_YIELD_PARK;
    assert_eq!(park.deadlines, [deadline, deadline]);
    assert_eq!(
        outcome,
        YieldOutcome::Sent(YieldWait {
            wait: PARSER_YIELD_PARK,
            parks: 2,
            timed_out: true,
            overshoot: None
        })
    );
}

/// S4: a stale token from an effective advance to `g − 1` returns the park, the worker sees
/// `served < g`, re-parks to the same deadline, and times out.
#[test]
fn a_stale_token_is_rechecked_and_reparked() {
    let (handshake, base) = handshake_at_base();
    let older = handshake.request();
    handshake.request();
    let script = [Step::StaleServe(older), Step::Timeout(Duration::ZERO)];
    let mut park = FakePark::new(&handshake, script, 2);
    let (outcome, _) = step(&handshake, &mut 0, None, &mut park);
    let deadline = base + PARSER_YIELD_PARK;
    assert_eq!(park.deadlines, [deadline, deadline]);
    assert_eq!(
        outcome,
        YieldOutcome::Sent(YieldWait {
            wait: PARSER_YIELD_PARK,
            parks: 2,
            timed_out: true,
            overshoot: None
        })
    );
}

/// §5.1 3: with no serve the wait ends at the fixed deadline: one timeout, a 2,000 µs sample
/// with no overshoot, or the sample plus a positive overshoot when the park returns late. The
/// grant is counted once: the same generation never sends or parks again.
#[test]
fn an_unserved_request_waits_to_its_fixed_deadline_once() {
    for (overshoot, recorded) in
        [(Duration::ZERO, None), (Duration::from_micros(300), Some(Duration::from_micros(300)))]
    {
        let (handshake, _base) = handshake_at_base();
        handshake.request();
        let mut park = FakePark::new(&handshake, [Step::Timeout(overshoot)], 1);
        let mut granted = 0;
        let (outcome, sent) = step(&handshake, &mut granted, None, &mut park);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            outcome,
            YieldOutcome::Sent(YieldWait {
                wait: PARSER_YIELD_PARK + overshoot,
                parks: 1,
                timed_out: true,
                overshoot: recorded
            })
        );
        let (again, resent) = step(&handshake, &mut granted, None, &mut park);
        assert_eq!((again, resent.len(), park.calls), (YieldOutcome::NoRequest, 0, 1));
    }
}

/// §5.1 4: a worker with no request neither reads its target, sends, nor parks, over 1,000
/// batches.
#[test]
fn no_request_never_sends_or_parks() {
    let (handshake, _base) = handshake_at_base();
    let mut park = FakePark::new(&handshake, [], 0);
    let mut granted = 0;
    for _ in 0..1_000 {
        let outcome = yield_step(
            &handshake,
            &mut granted,
            None,
            || -> Option<u8> { panic!("no request reads no target") },
            |_, _, _| panic!("no request sends nothing"),
            &mut park,
        );
        assert_eq!(outcome, YieldOutcome::NoRequest);
    }
    assert_eq!(park.calls, 0);
}

/// An open update's stored deadline caps the park, and a hold already due sends nothing and
/// parks nothing.
#[test]
fn an_open_update_caps_the_park_deadline() {
    let (handshake, base) = handshake_at_base();
    handshake.request();
    let held = base + Duration::from_micros(700);
    let mut park = FakePark::new(&handshake, [Step::Timeout(Duration::ZERO)], 1);
    let (_, sent) = step(&handshake, &mut 0, Some(held), &mut park);
    assert_eq!(sent[0].2, held, "the event carries the capped deadline");
    assert_eq!(park.deadlines, [held]);
    handshake.request();
    let (outcome, sent) =
        step(&handshake, &mut handshake.served.load(Ordering::Acquire), Some(base), &mut park);
    assert_eq!((outcome, sent.len(), park.calls), (YieldOutcome::Expired, 0, 1));
}

/// A request with no redraw target, or whose send fails, parks nothing.
#[test]
fn no_target_or_a_failed_send_never_parks() {
    let (handshake, _base) = handshake_at_base();
    let mut park = FakePark::new(&handshake, [], 0);
    let mut granted = 0;
    handshake.request();
    let outcome =
        yield_step(&handshake, &mut granted, None, || None::<u8>, |_, _, _| true, &mut park);
    assert_eq!(outcome, YieldOutcome::NoTarget);
    handshake.request();
    let outcome =
        yield_step(&handshake, &mut granted, None, || Some(7u8), |_, _, _| false, &mut park);
    assert_eq!(outcome, YieldOutcome::SendFailed);
    assert_eq!(park.calls, 0);
}

/// Serving is idempotent and monotonic: an already-served or older generation neither advances
/// `served` nor unparks, so the log holds only effective advances.
#[test]
fn serving_records_only_effective_advances() {
    let (handshake, _base) = handshake_at_base();
    let first = handshake.request();
    let second = handshake.request();
    assert!(handshake.serve_outstanding(second));
    assert!(!handshake.serve_outstanding(second));
    assert!(!handshake.serve_outstanding(first));
    assert_eq!(handshake.served.load(Ordering::Acquire), second);
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!((log[0].old, log[0].new), (0, second));
}

/// §5.1 11: the handshake parks on a token and wakes by a conditional unpark; it uses no
/// blocking primitive, so nothing on the render path can wait in it.
#[test]
fn the_handshake_module_has_no_blocking_primitive() {
    let source = include_str!("parser_yield.rs").replace("\r\n", "\n");
    for forbidden in ["Mutex", "Condvar", "RwLock", ".lock()"] {
        assert!(!source.contains(forbidden), "parser_yield.rs mentions {forbidden}");
    }
}

// ---------------------------------------------------------------------------------------------
// The window half: the real App on the shared fake dispatch clock, with private pools.
// ---------------------------------------------------------------------------------------------

use crate::app::{
    frame_counters::DeferRule,
    redraw::{redraw_dispatch_tests::publish_output, FrameSettlement, RedrawCause},
    App,
};
use winit::window::WindowId;

/// The monitor period every fixture window paces at, in microseconds: `p = 16.67 ms`.
const PERIOD_US: i64 = 16_670;

/// The pane geometry every fixture window lays out in: 80 by 24 cells of 8 by 16 pixels.
fn viewport() -> sonicterm_ui::pane::Rect {
    sonicterm_ui::pane::Rect::new(0.0, 0.0, 640.0, 384.0)
}

/// Whose lock a collection finds busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Free,
    Parser(u64),
    Images(u64),
}

/// What one redraw attempt did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attempt {
    /// Admission deferred it by the named rule, or refused it before any rule.
    Deferred(Option<DeferRule>),
    /// Every visible lock was taken and the frame completed.
    Collected,
    /// A visible lock was busy, or the layout could not be collected.
    Missed,
    /// The guarded synchronized-output recheck abandoned the frame.
    Abandoned,
}

/// A counting or uncounting App with one main window on the fake dispatch clock at `t0`.
struct Fixture {
    app: App,
    main: WindowId,
    start_at: Instant,
}

impl Fixture {
    /// The App, its seeded main window settled one second before `t0`, and an empty serve log.
    fn new(counting: bool) -> Self {
        let mut app = App::new(
            sonicterm_cfg::theme::Theme::default(),
            sonicterm_cfg::config::Config::default(),
            sonicterm_cfg::keymap::Keymap::default(),
        )
        .with_capture_staging_pool(sonicterm_vt::vt::CaptureStagingPool::new())
        .with_inline_media_pool(crate::app::media::InlineMediaPool::new());
        if counting {
            app.force_frame_counters_on().expect("no window exists yet");
        }
        app.__test_set_dispatch_clock(fake_now);
        let start_at = test_base() + Duration::from_secs(2);
        set_fake_now(start_at - Duration::from_secs(1));
        app.__test_seed_tab("a");
        let main = app.main_window_id.expect("seeded main");
        app.test_viewport_override = Some((viewport(), 8.0, 16.0));
        let mut fixture = Self { app, main, start_at };
        fixture.prepare(main);
        let _ = take_serve_log();
        fixture
    }

    /// `offset_us` microseconds after `t0`; negative is before.
    fn at(&self, offset_us: i64) -> Instant {
        if offset_us >= 0 {
            self.start_at + Duration::from_micros(offset_us.unsigned_abs())
        } else {
            // When: `offset_us` is negative, the instant precedes `t0`.
            self.start_at - Duration::from_micros(offset_us.unsigned_abs())
        }
    }

    /// The contention floor a miss at `t0` arms: `F = t0 + p`.
    fn floor(&self) -> Instant {
        self.at(PERIOD_US)
    }

    /// Give `id` the fixture period and geometry, settle its first frame, and put both pacing
    /// clocks at `t0 − 6 ms`, so Timer streaming is premature until `t0 + 10.67 ms`.
    fn prepare(&mut self, id: WindowId) {
        let settled_at = self.start_at - Duration::from_secs(1);
        let streamed_at = self.at(-6_000);
        let window = self.app.windows.get_mut(&id).expect("live window");
        window.redraw.monitor_period = Duration::from_micros(PERIOD_US.unsigned_abs());
        window.test_pane_viewport = Some((viewport(), 8.0, 16.0));
        let snapshot = window.redraw.snapshot();
        window.redraw.settle(snapshot, FrameSettlement::Presented, settled_at);
        window.last_render = streamed_at;
        window.stream_clock = streamed_at;
    }

    /// A seeded, prepared child window with one tab, and its pane.
    fn child(&mut self) -> (WindowId, u64) {
        let child = self.app.__test_seed_child_window(&["child"]);
        self.app.windows.get_mut(&child).expect("child").tabs.activate(0);
        self.prepare(child);
        (child, self.active_pane(child))
    }

    /// The pane `id` shows in its active tab.
    fn active_pane(&self, id: WindowId) -> u64 {
        let window = &self.app.windows[&id];
        window.tab_states[window.tabs.active_index()].active_pane
    }

    /// `pane`'s handshake in `id`.
    fn handshake(&self, id: WindowId, pane: u64) -> Arc<ParserYield> {
        Arc::clone(&self.app.windows[&id].panes[&pane].parser_yield)
    }

    /// Admit `id` at `admit_us` and, if admitted, collect at `collect_us` with `busy` held.
    fn attempt(&mut self, id: WindowId, admit_us: i64, collect_us: i64, busy: Busy) -> Attempt {
        set_fake_now(self.at(admit_us));
        if !self.app.admit_window_redraw(id) {
            return Attempt::Deferred(self.app.windows[&id].redraw.deferred_rule);
        }
        set_fake_now(self.at(collect_us));
        self.collect(id, busy)
    }

    /// One collection of `id` at the current fake instant through the production collector,
    /// recheck, reconciliation and completion the redraw adapters call.
    fn collect(&mut self, id: WindowId, busy: Busy) -> Attempt {
        let window = &self.app.windows[&id];
        let parser_lock = match busy {
            Busy::Parser(pane) => Some(Arc::clone(&window.panes[&pane].parser)),
            _ => None,
        };
        let images_lock = match busy {
            Busy::Images(pane) => Some(Arc::clone(&window.panes[&pane].inline_images)),
            _ => None,
        };
        let _parser_guard = parser_lock.as_ref().map(|lock| lock.lock());
        let _images_guard = images_lock.as_ref().map(|lock| lock.lock());
        let app = &mut self.app;
        let sources = match app.child_visible_frame_sources(id, viewport()) {
            Ok(sources) => sources,
            Err(why) => {
                let now = app.dispatch_now();
                app.visible_frame_unavailable(id, why, false, now);
                return Attempt::Missed;
            }
        };
        let held = match sources.try_collect(|| app.snapshot_window_redraw(id)) {
            Ok(held) => held,
            Err(why) => {
                let now = app.dispatch_now();
                app.visible_frame_unavailable(id, why, false, now);
                return Attempt::Missed;
            }
        };
        let crate::app::visible_frame::HeldVisibleFrame { snapshot, mut guards, images } = held;
        let states: Vec<_> =
            guards.iter().map(|(pane, parser, _)| (*pane, parser.synchronized_output())).collect();
        let now = app.dispatch_now();
        if app.abandon_synchronized_frame(id, &states, now) {
            return Attempt::Abandoned;
        }
        let window = app.windows.get_mut(&id).expect("live window");
        if let Err(why) = sources.reconcile_and_apply_receipts(window, &mut guards) {
            drop(guards);
            drop(images);
            let now = app.dispatch_now();
            app.visible_frame_unavailable(id, why, false, now);
            return Attempt::Missed;
        }
        window.coherent_frame_collected();
        drop(guards);
        drop(images);
        app.complete_window_redraw(id, &snapshot.expect("live owner"), FrameSettlement::Presented);
        Attempt::Collected
    }

    /// Deliver `ParserYielded` for `pane`'s `generation` to `id` at `at_us`.
    fn deliver(&mut self, id: WindowId, pane: u64, generation: u64, at_us: i64, deadline_us: i64) {
        set_fake_now(self.at(at_us));
        let park_deadline = self.at(deadline_us);
        self.app.handle_parser_yielded(id, pane, generation, park_deadline);
    }

    /// `id`'s (requests, wakes, rejected, frames, lost).
    fn counts(&self, id: WindowId) -> (u64, u64, u64, u64, u64) {
        let counters =
            self.app.windows[&id].redraw.frame_counters.as_deref().expect("counting fixture");
        (
            counters.parser_yield_requests,
            counters.parser_yield_wakes,
            counters.parser_yield_rejected,
            counters.parser_yield_frames,
            counters.parser_yield_lost,
        )
    }

    /// The stage of `id`'s token, if it has one.
    fn token_stage(&self, id: WindowId) -> Option<TokenStage> {
        self.app.windows[&id].redraw.yield_token.as_ref().map(|token| token.stage)
    }

    /// A miss at `t0` on `id`'s active pane, admitted just before: it publishes request 1 and
    /// arms the floor `F`.
    fn missed_at_t0(&mut self, id: WindowId) -> u64 {
        let pane = self.active_pane(id);
        self.app.mark_window_redraw(id, RedrawCause::Input);
        assert_eq!(self.attempt(id, -300, 0, Busy::Parser(pane)), Attempt::Missed);
        assert_eq!(self.app.windows[&id].retry_not_before, Some(self.floor()));
        assert_eq!(self.handshake(id, pane).requested.load(Ordering::Acquire), 1);
        pane
    }

    /// The miss at `t0`, new output, and the grant for generation 1 with `park_deadline = t0+5`,
    /// accepted at `t0+3.3`. Acceptance serves nothing.
    fn granted(&mut self, id: WindowId) -> u64 {
        let pane = self.missed_at_t0(id);
        publish_output(&self.app, id);
        self.deliver(id, pane, 1, 3_300, 5_000);
        assert_eq!(self.token_stage(id), Some(TokenStage::Granted));
        assert!(take_serve_log().is_empty(), "acceptance serves nothing");
        pane
    }

    /// Open a synchronized update on `pane` of `id` as its worker publishes it, at `at_us`.
    fn open_update(&mut self, id: WindowId, pane: u64, at_us: i64) {
        let handles = crate::app::spawn_pane::PaneVtHandles::from_pane_state(
            &self.app.windows[&id].panes[&pane],
        );
        let opened_at = self.at(at_us);
        crate::app::spawn_pane::publish_pane_vt_batch_with(
            &handles,
            b"\x1b[?2026h",
            &mut None,
            &mut crate::app::spawn_pane::SyncLatch::default(),
            |_| None,
            |_| {},
            || opened_at,
            |_| {},
        );
    }

    /// Close the update `open_update` opened, as the worker publishes it, at `at_us`.
    fn close_update(&mut self, id: WindowId, pane: u64, at_us: i64) {
        let handles = crate::app::spawn_pane::PaneVtHandles::from_pane_state(
            &self.app.windows[&id].panes[&pane],
        );
        let closed_at = self.at(at_us);
        crate::app::spawn_pane::publish_pane_vt_batch_with(
            &handles,
            b"\x1b[?2026l",
            &mut None,
            &mut crate::app::spawn_pane::SyncLatch::default(),
            |_| None,
            |_| {},
            || closed_at,
            |_| {},
        );
    }

    /// Split `id`'s active pane right with a new seeded pane, returning it.
    fn split(&mut self, id: WindowId) -> u64 {
        let pane = crate::app::next_pane_id();
        let parser =
            Arc::new(parking_lot::Mutex::new(sonicterm_vt::vt::Parser::new_with_staging_pool(
                sonicterm_grid::grid::Grid::new(40, 24),
                None,
                Arc::clone(&self.app.capture_staging_pool),
            )));
        let state =
            crate::app::PaneState::new_with_media_pool(parser, None, &self.app.inline_media_pool);
        let window = self.app.windows.get_mut(&id).expect("live window");
        let tab = &mut window.tab_states[window.tabs.active_index()];
        let focus = tab.active_pane;
        assert!(tab.tree.split(focus, sonicterm_cfg::keymap::Direction::Right, pane));
        window.panes.insert(pane, state);
        pane
    }

    /// The App's live record for `id` in a counter snapshot.
    fn live_record(&self, id: WindowId) -> crate::app::frame_counters::CounterRecord {
        let snapshot = self.app.frame_counters_snapshot().expect("counting fixture");
        snapshot.windows.into_iter().find(|(window, _)| *window == id).expect("live").1
    }
}

/// S5 (a): a grant accepted at `t0+3.3` lets the admission at `t0+4` bypass both the floor `F`
/// and premature Timer streaming once; the collection serves the worker under the guards (the
/// only effective advance), and the token resolves as a frame.
#[test]
fn an_accepted_grant_bypasses_the_floor_and_the_carry_once() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.granted(main);
    assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Free), Attempt::Collected);
    assert_eq!(fixture.counts(main), (1, 1, 0, 1, 0));
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!((log[0].old, log[0].new), (0, 1));
    assert_eq!(fixture.token_stage(main), None);
}

/// S5 (b): the same retry misses instead, `lost(parser)`; the bypass is spent, so an attempt at
/// `t0+8` defers by Contention and the attempt at `F` admits.
#[test]
fn a_missed_retry_spends_the_bypass() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.granted(main);
    assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Parser(pane)), Attempt::Missed);
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    let floor_us = PERIOD_US;
    assert_eq!(
        fixture.attempt(main, 8_000, 8_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    assert_eq!(fixture.attempt(main, floor_us, floor_us, Busy::Free), Attempt::Collected);
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
}

/// S5 (c): a surface timeout pending at `t0+4` wins over the grant: `lost(deferred)`.
#[test]
fn a_pending_timeout_wins_over_a_grant() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.granted(main);
    let rendered_at = fixture.at(3_900);
    let window = fixture.app.windows.get_mut(&main).unwrap();
    window.redraw.timeout_pending = true;
    window.last_render = rendered_at;
    assert_eq!(
        fixture.attempt(main, 4_000, 4_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Timeout))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    assert_eq!(take_serve_log().len(), 1);
}

/// S5 (d): an update opened after acceptance holds the admission at `t0+4` by Sync:
/// `lost(deferred)`; the grant carries over neither Timeout nor Sync.
#[test]
fn an_open_update_wins_over_a_grant() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.granted(main);
    fixture.open_update(main, pane, 3_500);
    assert_eq!(
        fixture.attempt(main, 4_000, 4_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Sync))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
}

/// S5 (e): occlusion at `t0+3.6` resolves the token `lost(suppressed)` eagerly, with one
/// effective serve.
#[test]
fn occlusion_resolves_a_grant_eagerly() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.granted(main);
    set_fake_now(fixture.at(3_600));
    fixture.app.handle_window_occlusion(main, true);
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    assert_eq!(fixture.token_stage(main), None);
    assert_eq!(take_serve_log().len(), 1);
}

/// S5 (f): admission at `t0+5.2`, past the worker's deadline, resolves `lost(expired)` and the
/// floor still holds by Contention until `F`.
#[test]
fn a_grant_past_its_park_deadline_expires_at_admission() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.granted(main);
    assert_eq!(
        fixture.attempt(main, 5_200, 5_200, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    assert_eq!(fixture.attempt(main, PERIOD_US, PERIOD_US, Busy::Free), Attempt::Collected);
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
}

/// S6: an accepted wake whose admitted retry misses the parser, the images or the guarded Sync
/// recheck, or is deferred, resolves `lost` at that exit; the ordinary success that follows adds
/// no frame. Each failing variant is named.
#[test]
fn a_lost_retry_is_never_credited_by_a_later_success() {
    let variants: [(&str, fn(&mut Fixture, u64)); 4] = [
        ("parser", |fixture, pane| {
            let main = fixture.main;
            assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Parser(pane)), Attempt::Missed);
        }),
        ("images", |fixture, pane| {
            let main = fixture.main;
            assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Images(pane)), Attempt::Missed);
        }),
        ("sync", |fixture, pane| {
            let main = fixture.main;
            let parser = Arc::clone(&fixture.app.windows[&main].panes[&pane].parser);
            parser.lock().advance(b"\x1b[?2026h");
            assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Free), Attempt::Abandoned);
            parser.lock().advance(b"\x1b[?2026l");
        }),
        ("deferred", |fixture, _pane| {
            let main = fixture.main;
            let rendered_at = fixture.at(3_900);
            let window = fixture.app.windows.get_mut(&main).unwrap();
            window.redraw.timeout_pending = true;
            window.last_render = rendered_at;
            assert!(matches!(
                fixture.attempt(main, 4_000, 4_000, Busy::Free),
                Attempt::Deferred(_)
            ));
            fixture.app.windows.get_mut(&main).unwrap().redraw.timeout_pending = false;
        }),
    ];
    let failed: Vec<String> = variants
        .iter()
        .filter_map(|(name, retry)| {
            let outcome = std::panic::catch_unwind(|| {
                let mut fixture = Fixture::new(true);
                let main = fixture.main;
                let pane = fixture.granted(main);
                retry(&mut fixture, pane);
                assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1), "after the retry");
                assert_eq!(
                    fixture.attempt(main, PERIOD_US, PERIOD_US, Busy::Free),
                    Attempt::Collected
                );
                assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1), "after the success");
            });
            outcome.is_err().then(|| (*name).to_owned())
        })
        .collect();
    assert!(failed.is_empty(), "failed variants: {failed:?}");
}

/// S7 (a): a grant delivered while an update holds the window is declined without spending the
/// episode; the update closes, and the miss at the floor publishes generation 2.
#[test]
fn a_declined_grant_leaves_the_episode_open() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    fixture.open_update(main, pane, 1_000);
    fixture.deliver(main, pane, 1, 3_300, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    fixture.close_update(main, pane, 8_000);
    assert_eq!(fixture.attempt(main, PERIOD_US, PERIOD_US, Busy::Parser(pane)), Attempt::Missed);
    assert_eq!(fixture.counts(main), (2, 0, 1, 0, 0));
    assert_eq!(fixture.handshake(main, pane).requested.load(Ordering::Acquire), 2);
}

/// S7 (b): a forced resize during an update still yields: the worker grants under its open
/// update (its park capped at the update's deadline) and the window accepts.
#[test]
fn a_forced_resize_during_an_update_still_yields() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    fixture.open_update(main, pane, 1_000);
    fixture.app.windows.get_mut(&main).unwrap().redraw.resize_pending = true;
    let handshake = fixture.handshake(main, pane);
    set_fake_now(fixture.at(3_000));
    let mut park = FakePark::new(&handshake, [Step::Timeout(Duration::ZERO)], 1);
    let mut sent = None;
    let held = Some(fixture.at(151_000));
    yield_step(
        &handshake,
        &mut 0,
        held,
        || Some(main),
        |_, generation, deadline| {
            sent = Some((generation, deadline));
            true
        },
        &mut park,
    );
    let (generation, deadline) = sent.expect("the worker grants under an open update");
    set_fake_now(fixture.at(3_300));
    fixture.app.handle_parser_yielded(main, pane, generation, deadline);
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 0));
}

/// S8 (a): a tab torn out before the grant: main no longer shows the pane, so the grant is
/// rejected as *moved* and the worker is served once.
#[test]
fn a_pane_torn_out_before_the_grant_is_moved() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.app.__test_seed_tab("c");
    let _ = pane;
    fixture.app.windows.get_mut(&main).unwrap().tabs.activate(0);
    let torn = fixture.missed_at_t0(main);
    let (child, _) = fixture.child();
    fixture.app.transfer_tab(Some(main), 0, Some(child), 1).expect("tear-out");
    fixture.deliver(main, torn, 1, 3_300, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert!(fixture.app.windows[&main].redraw.yield_ask.is_none());
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
}

/// S8 (b): a tab switch before the grant hides the pane: rejected as *moved*, served once.
#[test]
fn a_tab_switch_before_the_grant_is_moved() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.app.__test_seed_tab("c");
    fixture.app.windows.get_mut(&main).unwrap().tabs.activate(0);
    let pane = fixture.missed_at_t0(main);
    fixture.app.windows.get_mut(&main).unwrap().tabs.activate(1);
    fixture.deliver(main, pane, 1, 3_300, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert_eq!(take_serve_log().len(), 1);
}

/// S8 (c): asks are per window. A grant naming another window's pane is rejected and does not
/// wake this window; each window accepts only its own pane and generation, and W1's floor is
/// never bypassed by W2's grant.
#[test]
fn windows_answer_only_their_own_asks() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let (child, _) = fixture.child();
    let pane_a = fixture.missed_at_t0(main);
    let pane_b = fixture.missed_at_t0(child);
    fixture.deliver(child, pane_b, 1, 3_000, 5_000);
    assert_eq!(fixture.token_stage(child), Some(TokenStage::Granted));
    fixture.deliver(main, pane_b, 1, 3_100, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert!(fixture.app.windows[&main].redraw.yield_ask.is_some(), "main's own ask stays");
    assert_eq!(fixture.token_stage(main), None);
    assert_eq!(fixture.counts(child), (1, 1, 0, 0, 0), "main's grant never reaches the child");
    publish_output(&fixture.app, main);
    assert_eq!(
        fixture.attempt(main, 3_200, 3_200, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    fixture.deliver(main, pane_a, 1, 3_300, 5_000);
    assert_eq!(fixture.token_stage(main), Some(TokenStage::Granted));
}

/// S8 (d1): the granted pane retires and the window admits before `park_deadline`: the token
/// resolves `lost(moved)` at revalidation with one effective, unparking serve, and the window
/// waits for its floor.
#[test]
fn a_retired_pane_after_acceptance_is_lost_moved_at_admission() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let other = fixture.split(main);
    let pane = fixture.active_pane(main);
    fixture.handshake(main, pane).register_worker();
    fixture.granted(main);
    let window = fixture.app.windows.get_mut(&main).unwrap();
    let tab = &mut window.tab_states[0];
    assert!(tab.tree.close(pane));
    tab.active_pane = other;
    let retired = window.remove_pane(pane).expect("pane");
    fixture.app.retire_pane(retired);
    assert!(take_serve_log().is_empty(), "retirement serves lazily");
    assert_eq!(
        fixture.attempt(main, 4_000, 4_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].unparked);
}

/// S8 (d2): the granted pane retires and the window is not admitted before `park_deadline`:
/// the worker times out with no eager service, and the window's next boundary resolves the token
/// `lost(expired)` and serves it then.
#[test]
fn a_retired_pane_not_admitted_in_time_times_the_worker_out_first() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let other = fixture.split(main);
    let pane = fixture.granted(main);
    let handshake = fixture.handshake(main, pane);
    let window = fixture.app.windows.get_mut(&main).unwrap();
    let tab = &mut window.tab_states[0];
    assert!(tab.tree.close(pane));
    tab.active_pane = other;
    let retired = window.remove_pane(pane).expect("pane");
    fixture.app.retire_pane(retired);
    set_fake_now(fixture.at(3_400));
    let mut park = FakePark::new(&handshake, [Step::Timeout(Duration::ZERO)], 1);
    let outcome = yield_step(&handshake, &mut 0, None, || Some(main), |_, _, _| true, &mut park);
    assert!(matches!(outcome, YieldOutcome::Sent(YieldWait { timed_out: true, .. })));
    assert!(take_serve_log().is_empty(), "no eager service before the boundary");
    drop(park);
    assert_eq!(
        fixture.attempt(main, 6_000, 6_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    assert_eq!(take_serve_log().len(), 1);
}

/// S8 (d3), S17 (a): a window closed after acceptance resolves its token through the real
/// removal path before its counters retire: the closed totals carry W = 1 and L = 1, never a
/// token level.
#[test]
fn closing_a_window_resolves_its_token_before_its_counters_retire() {
    let mut fixture = Fixture::new(true);
    let (child, _) = fixture.child();
    fixture.granted(child);
    assert!(fixture.app.close_child_window(child));
    let snapshot = fixture.app.frame_counters_snapshot().unwrap();
    let closed = &snapshot.closed_windows;
    assert_eq!(closed.count("parser_yield_wakes"), Some(1));
    assert_eq!(closed.count("parser_yield_lost"), Some(1));
    assert_eq!(closed.count("parser_yield_tokens"), None, "closed records carry no level");
    assert!(snapshot
        .windows
        .iter()
        .all(|(_, record)| record.count("parser_yield_tokens") == Some(0)));
    assert_eq!(take_serve_log().len(), 1);
}

/// S8 (e): a stale grant for `g − 1` while the ask is `g` is rejected without clearing the ask,
/// which is later accepted.
#[test]
fn a_stale_grant_leaves_the_current_ask() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.active_pane(main);
    fixture.handshake(main, pane).request();
    fixture.app.mark_window_redraw(main, RedrawCause::Input);
    assert_eq!(fixture.attempt(main, -300, 0, Busy::Parser(pane)), Attempt::Missed);
    fixture.deliver(main, pane, 1, 3_000, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert!(fixture.app.windows[&main].redraw.yield_ask.is_some(), "the ask for g stays");
    fixture.deliver(main, pane, 2, 3_300, 5_000);
    assert_eq!(fixture.token_stage(main), Some(TokenStage::Granted));
}

/// S8 (f): the granted pane is torn out to W2 while W1 keeps visible pane B: W1's admission
/// finds it gone, `lost(moved)`, defers by Contention, and collects B at its floor.
#[test]
fn a_pane_torn_out_after_acceptance_is_revalidated() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.app.__test_seed_tab("b");
    fixture.app.windows.get_mut(&main).unwrap().tabs.activate(0);
    fixture.granted(main);
    let (child, _) = fixture.child();
    fixture.app.transfer_tab(Some(main), 0, Some(child), 1).expect("tear-out");
    assert_eq!(
        fixture.attempt(main, 4_000, 4_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
    assert_eq!(fixture.attempt(main, PERIOD_US, PERIOD_US, Busy::Free), Attempt::Collected);
}

/// S9 (a, b): a grant delivered after the ask's floor, or once a newer floor is armed, is
/// rejected and never changes the floor.
#[test]
fn a_late_or_superseded_floor_rejects_the_grant() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    fixture.deliver(main, pane, 1, PERIOD_US + 10, PERIOD_US + 2_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert_eq!(fixture.app.windows[&main].retry_not_before, Some(fixture.floor()));

    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    let newer = fixture.at(30_000);
    fixture.app.windows.get_mut(&main).unwrap().retry_not_before = Some(newer);
    fixture.deliver(main, pane, 1, 3_300, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert_eq!(fixture.app.windows[&main].retry_not_before, Some(newer));
}

/// S9 (c, d, e): an already-served grant is rejected with no further serve; a stale grant `g − 1`
/// while `served = g − 2` advances `served` to `g − 1 < g`, so the worker for `g` still waits;
/// a stale grant `g − 2` while `served = g − 1` serves nothing.
#[test]
fn stale_or_served_grants_never_lower_or_complete_service() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    fixture.handshake(main, pane).serve_outstanding(1);
    let _ = take_serve_log();
    fixture.deliver(main, pane, 1, 3_300, 5_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
    assert!(take_serve_log().is_empty());

    for (served, stale, advances) in [(1, 2, true), (2, 1, false)] {
        let mut fixture = Fixture::new(true);
        let main = fixture.main;
        let pane = fixture.active_pane(main);
        let handshake = fixture.handshake(main, pane);
        handshake.request();
        handshake.request();
        handshake.serve_outstanding(served);
        fixture.app.mark_window_redraw(main, RedrawCause::Input);
        assert_eq!(fixture.attempt(main, -300, 0, Busy::Parser(pane)), Attempt::Missed);
        let _ = take_serve_log();
        fixture.deliver(main, pane, stale, 3_000, 5_000);
        assert_eq!(handshake.served.load(Ordering::Acquire), served.max(stale));
        assert!(handshake.served.load(Ordering::Acquire) < 3, "the worker for g still waits");
        assert_eq!(take_serve_log().len(), usize::from(advances));
        assert!(fixture.app.windows[&main].redraw.yield_ask.is_some());
    }
}

/// S10: on the software path no request is published, gate on or off, and the worker sends
/// nothing.
#[test]
fn the_software_path_publishes_no_request() {
    for counting in [true, false] {
        let mut fixture = Fixture::new(counting);
        let main = fixture.main;
        fixture.app.set_software_render_degrade(true);
        let pane = fixture.active_pane(main);
        set_fake_now(fixture.at(0));
        assert_eq!(fixture.collect(main, Busy::Parser(pane)), Attempt::Missed);
        let handshake = fixture.handshake(main, pane);
        assert_eq!(handshake.requested.load(Ordering::Acquire), 0, "counting: {counting}");
        let mut park = FakePark::new(&handshake, [], 0);
        let outcome =
            yield_step(&handshake, &mut 0, None, || Some(main), |_, _, _| true, &mut park);
        assert_eq!(outcome, YieldOutcome::NoRequest);
    }
}

/// S12: with the gate off the mechanism runs identically: the same admission and the same one
/// effective serve, and no counters.
#[test]
fn the_mechanism_runs_with_the_gate_off() {
    let mut fixture = Fixture::new(false);
    let main = fixture.main;
    fixture.granted(main);
    assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Free), Attempt::Collected);
    assert_eq!(take_serve_log().len(), 1);
    assert!(fixture.app.frame_counters_snapshot().is_none());
}

/// S13: `park_deadline = t0+2`, floor `t0+16.67`: (a) delivery at `t0+3` is rejected; (b)
/// acceptance at `t0+1` then admission at `t0+3` expires the token, and the floor holds.
#[test]
fn the_park_deadline_bounds_a_grant_before_the_floor() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    fixture.deliver(main, pane, 1, 3_000, 2_000);
    assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));

    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.missed_at_t0(main);
    fixture.deliver(main, pane, 1, 1_000, 2_000);
    assert_eq!(fixture.token_stage(main), Some(TokenStage::Granted));
    assert_eq!(fixture.app.windows[&main].retry_not_before, Some(fixture.floor()));
    assert_eq!(
        fixture.attempt(main, 3_000, 3_000, Busy::Free),
        Attempt::Deferred(Some(DeferRule::Contention))
    );
    assert_eq!(fixture.counts(main), (1, 1, 0, 0, 1));
}

/// S14: suppression after a grant resolves the token at each eager site and at the reachable
/// pre-rule return of admission: `lost = 1`, one effective serve, no token. Each failing site is
/// named.
#[test]
fn every_suppression_site_resolves_a_grant() {
    let sites: Vec<(&str, fn(&mut Fixture))> = vec![
        ("settle(Stopped)", |fixture| {
            let main = fixture.main;
            let redraw = &mut fixture.app.windows.get_mut(&main).unwrap().redraw;
            let snapshot = redraw.snapshot();
            redraw.settle(snapshot, FrameSettlement::Stopped(1), fake_now());
        }),
        ("settle(SurfaceRetry(Occluded))", |fixture| {
            let main = fixture.main;
            let redraw = &mut fixture.app.windows.get_mut(&main).unwrap().redraw;
            let snapshot = redraw.snapshot();
            let occluded =
                FrameSettlement::SurfaceRetry(sonicterm_gpu::core::SurfaceRetryReason::Occluded);
            redraw.settle(snapshot, occluded, fake_now());
        }),
        ("observe_native_occlusion(true)", |fixture| {
            let main = fixture.main;
            fixture.app.handle_window_occlusion(main, true);
        }),
        ("park", |fixture| {
            let main = fixture.main;
            let redraw = &mut fixture.app.windows.get_mut(&main).unwrap().redraw;
            let snapshot = redraw.snapshot();
            redraw.park(snapshot);
        }),
        ("set_software_render_degrade(true)", |fixture| {
            fixture.app.set_software_render_degrade(true);
        }),
        ("hide_main_window", |fixture| fixture.app.hide_main_window()),
        ("begin_window_redraw: !frame_deadlines_allowed", |fixture| {
            let main = fixture.main;
            fixture.app.windows.get_mut(&main).unwrap().redraw.parked = true;
            assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Free), Attempt::Deferred(None));
        }),
    ];
    let failed: Vec<String> = sites
        .iter()
        .filter_map(|(name, site)| {
            let outcome = std::panic::catch_unwind(|| {
                let mut fixture = Fixture::new(true);
                let main = fixture.main;
                fixture.granted(main);
                set_fake_now(fixture.at(3_600));
                site(&mut fixture);
                assert_eq!(fixture.counts(main).4, 1, "lost");
                assert_eq!(fixture.token_stage(main), None);
                assert_eq!(take_serve_log().len(), 1);
                assert_eq!(fixture.live_record(main).count("parser_yield_tokens"), Some(0));
            });
            outcome.is_err().then(|| (*name).to_owned())
        })
        .collect();
    assert!(failed.is_empty(), "failed sites: {failed:?}");
}

/// S15 (app side): a token accepted before a phase and resolved inside it, and one accepted
/// inside a phase and still open at its end, both satisfy `ΔW − ΔF − ΔL = end − start`, with
/// frames counted at resolution, not at the wake.
#[test]
fn tokens_crossing_a_phase_boundary_keep_the_invariant() {
    let record = |fixture: &Fixture| {
        let record = fixture.live_record(fixture.main);
        let read = |name| record.count(name).expect(name);
        (
            read("parser_yield_wakes") as i64,
            read("parser_yield_frames") as i64,
            read("parser_yield_lost") as i64,
            read("parser_yield_tokens") as i64,
        )
    };
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.granted(main);
    let start = record(&fixture);
    assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Free), Attempt::Collected);
    let end = record(&fixture);
    assert_eq!((end.0 - start.0, end.1 - start.1, start.3, end.3), (0, 1, 1, 0));
    assert_eq!(end.0 - start.0 - (end.1 - start.1) - (end.2 - start.2), end.3 - start.3);

    let mut fixture = Fixture::new(true);
    let start = record(&fixture);
    let main = fixture.main;
    fixture.granted(main);
    let end = record(&fixture);
    assert_eq!((end.0 - start.0, end.1 - start.1, end.3), (1, 0, 1));
    assert_eq!(end.0 - start.0 - (end.1 - start.1) - (end.2 - start.2), end.3 - start.3);
}

/// S17 (a): an accepted token at each real removal path is resolved `lost` before the window's
/// counters retire: the closed totals carry W and the new L, live occupancy is 0, the worker is
/// served once, and the gate off resolves and serves the same.
#[test]
fn every_removal_path_resolves_before_retirement() {
    let paths: Vec<(&str, fn(&mut Fixture) -> WindowId)> = vec![
        ("close_child_window", |fixture| {
            let (child, _) = fixture.child();
            fixture.granted(child);
            assert!(fixture.app.close_child_window(child));
            child
        }),
        ("reap_empty_child", |fixture| {
            let (child, _) = fixture.child();
            fixture.granted(child);
            let main = fixture.main;
            fixture.app.transfer_tab(Some(child), 0, Some(main), 1).expect("merge");
            child
        }),
        ("retire_previous_main", |fixture| {
            let main = fixture.main;
            fixture.granted(main);
            fixture.app.retire_previous_main();
            main
        }),
    ];
    let mut failed = Vec::new();
    for counting in [true, false] {
        for (name, remove) in &paths {
            let outcome = std::panic::catch_unwind(|| {
                let mut fixture = Fixture::new(counting);
                let removed = remove(&mut fixture);
                assert!(!fixture.app.windows.contains_key(&removed));
                assert_eq!(take_serve_log().len(), 1, "served once");
                if counting {
                    let snapshot = fixture.app.frame_counters_snapshot().unwrap();
                    assert_eq!(snapshot.closed_windows.count("parser_yield_wakes"), Some(1));
                    assert_eq!(snapshot.closed_windows.count("parser_yield_lost"), Some(1));
                    assert!(snapshot
                        .windows
                        .iter()
                        .all(|(_, record)| record.count("parser_yield_tokens") == Some(0)));
                }
            });
            if outcome.is_err() {
                failed.push(format!("{name} (counting: {counting})"));
            }
        }
    }
    assert!(failed.is_empty(), "failed paths: {failed:?}");
}

/// S17 (b): an Admitted token at each post-admission exit the adapters reach headlessly
/// (parser and image misses, no layout, structural invalidity, guarded Sync abandonment, no
/// renderer) is resolved `lost` on the still-live window's record; `closed_windows` is unchanged.
#[test]
fn every_adapter_exit_resolves_an_admitted_token() {
    use crate::app::visible_frame::{FrameUnavailable, LayoutInvalid};
    let exits: Vec<(&str, fn(&mut Fixture, u64))> = vec![
        ("contended parser", |fixture, pane| {
            let why = FrameUnavailable::Contended { pane_id: pane, images: false };
            let (main, now) = (fixture.main, fake_now());
            fixture.app.visible_frame_unavailable(main, why, false, now);
        }),
        ("contended images", |fixture, pane| {
            let why = FrameUnavailable::Contended { pane_id: pane, images: true };
            let (main, now) = (fixture.main, fake_now());
            fixture.app.visible_frame_unavailable(main, why, false, now);
        }),
        ("no layout", |fixture, _| {
            let (main, now) = (fixture.main, fake_now());
            fixture.app.visible_frame_unavailable(main, FrameUnavailable::NoLayout, false, now);
        }),
        ("structural invalid", |fixture, _| {
            let why = FrameUnavailable::StructuralInvalid(LayoutInvalid::MissingTabState);
            let (main, now) = (fixture.main, fake_now());
            fixture.app.visible_frame_unavailable(main, why, false, now);
        }),
        ("guarded sync abandonment", |fixture, pane| {
            let main = fixture.main;
            let parser = Arc::clone(&fixture.app.windows[&main].panes[&pane].parser);
            parser.lock().advance(b"\x1b[?2026h");
            assert_eq!(fixture.collect(main, Busy::Free), Attempt::Abandoned);
        }),
        ("no renderer", |fixture, _| {
            let main = fixture.main;
            fixture.app.finish_yield_attempt(main, YieldLoss::Invalid);
        }),
    ];
    let failed: Vec<String> = exits
        .iter()
        .filter_map(|(name, exit)| {
            let outcome = std::panic::catch_unwind(|| {
                let mut fixture = Fixture::new(true);
                let main = fixture.main;
                let pane = fixture.granted(main);
                set_fake_now(fixture.at(4_000));
                assert!(fixture.app.admit_window_redraw(main));
                assert_eq!(fixture.token_stage(main), Some(TokenStage::Admitted));
                let closed_before = fixture.app.frame_counters_snapshot().unwrap().closed_windows;
                exit(&mut fixture, pane);
                let record = fixture.live_record(main);
                assert_eq!(record.count("parser_yield_wakes"), Some(1));
                assert_eq!(record.count("parser_yield_lost"), Some(1));
                assert_eq!(record.count("parser_yield_tokens"), Some(0));
                let closed = fixture.app.frame_counters_snapshot().unwrap().closed_windows;
                assert_eq!(
                    closed.count("parser_yield_lost"),
                    closed_before.count("parser_yield_lost")
                );
                assert_eq!(take_serve_log().len(), 1);
            });
            outcome.is_err().then(|| (*name).to_owned())
        })
        .collect();
    assert!(failed.is_empty(), "failed exits: {failed:?}");
}

/// S17 (b), source: in both redraw adapters every return between admission and the coherent
/// collection resolves the token, or is a missing-owner exit whose removal already resolved it.
#[test]
fn every_adapter_return_after_admission_resolves_the_token() {
    let resolvers = [
        "visible_frame_unavailable(",
        "abandon_synchronized_frame(",
        "finish_yield_attempt(",
        "finish_admitted_yield(",
    ];
    for (name, source, start, end) in [
        (
            "main",
            include_str!("window_event.rs"),
            "if !self.admit_window_redraw(win_id) {",
            "window.coherent_frame_collected();",
        ),
        (
            "child",
            include_str!("child_window_redraw.rs"),
            "pub(super) fn handle_child_redraw_requested(",
            "child.coherent_frame_collected();",
        ),
    ] {
        let source = source.replace("\r\n", "\n");
        let from = source.find(start).expect(start);
        let to = from + source[from..].find(end).expect(end);
        let lines: Vec<&str> = source[from..to].lines().collect();
        let mut returns = 0;
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "return;" {
                continue;
            }
            returns += 1;
            let context = lines[index.saturating_sub(10)..index].join("\n");
            let resolved = resolvers.iter().any(|call| context.contains(call));
            let missing_owner =
                context.contains("no longer") || context.contains("admit_window_redraw` refuses");
            assert!(resolved || missing_owner, "{name}: unresolved return after\n{context}");
        }
        assert!(returns >= 5, "{name}: found {returns} returns");
    }
}

/// S17 (b) backstop: an Admitted token that reaches the next admission means an adapter return
/// skipped its resolution; the debug assertion catches it in tests.
#[test]
#[should_panic(expected = "admitted yield token")]
fn an_unresolved_admitted_token_fails_the_backstop() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    fixture.granted(main);
    set_fake_now(fixture.at(4_000));
    assert!(fixture.app.admit_window_redraw(main));
    let _ = fixture.app.admit_window_redraw(main);
}

/// §5.1 1: a miss requests only the missed pane; an image miss publishes nothing.
#[test]
fn a_miss_requests_only_the_missed_pane() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane_a = fixture.active_pane(main);
    let pane_b = fixture.split(main);
    fixture.app.mark_window_redraw(main, RedrawCause::Input);
    assert_eq!(fixture.attempt(main, -300, 0, Busy::Parser(pane_b)), Attempt::Missed);
    assert_eq!(fixture.handshake(main, pane_a).requested.load(Ordering::Acquire), 0);
    assert_eq!(fixture.handshake(main, pane_b).requested.load(Ordering::Acquire), 1);
    assert_eq!(fixture.counts(main).0, 1);

    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.active_pane(main);
    fixture.app.mark_window_redraw(main, RedrawCause::Input);
    assert_eq!(fixture.attempt(main, -300, 0, Busy::Images(pane)), Attempt::Missed);
    assert_eq!(fixture.handshake(main, pane).requested.load(Ordering::Acquire), 0);
    assert_eq!(fixture.counts(main).0, 0);
}

/// §5.1 5: a valid grant the window cannot use (pane hidden, a surface timeout pending, an open
/// update, the software path) is declined with one effective serve, the floor kept, no wake.
#[test]
fn declined_grants_serve_once_and_keep_the_floor() {
    let declines: Vec<(&str, fn(&mut Fixture, u64))> = vec![
        ("not visible", |fixture, _| {
            let main = fixture.main;
            fixture.app.__test_seed_tab("c");
            fixture.app.windows.get_mut(&main).unwrap().tabs.activate(1);
        }),
        ("timeout pending", |fixture, _| {
            let main = fixture.main;
            fixture.app.windows.get_mut(&main).unwrap().redraw.timeout_pending = true;
        }),
        ("open update", |fixture, pane| {
            let main = fixture.main;
            fixture.open_update(main, pane, 1_000);
        }),
        ("software", |fixture, _| fixture.app.software_render_degrade = true),
    ];
    let failed: Vec<String> = declines
        .iter()
        .filter_map(|(name, decline)| {
            let outcome = std::panic::catch_unwind(|| {
                let mut fixture = Fixture::new(true);
                let main = fixture.main;
                let pane = fixture.missed_at_t0(main);
                decline(&mut fixture, pane);
                fixture.deliver(main, pane, 1, 3_300, 5_000);
                assert_eq!(fixture.counts(main), (1, 0, 1, 0, 0));
                assert_eq!(take_serve_log().len(), 1);
                assert_eq!(fixture.app.windows[&main].retry_not_before, Some(fixture.floor()));
                assert!(!fixture.app.windows[&main].redraw.yield_episode_spent);
            });
            outcome.is_err().then(|| (*name).to_owned())
        })
        .collect();
    assert!(failed.is_empty(), "failed declines: {failed:?}");
}

/// §5.1 7: end to end through the worker step: the worker sends and parks; while it is parked
/// the window accepts, admits at `t0+4` and collects. `wakes = 1`, `frames = 1`, `lost = 0`,
/// one effective advance (at collection) and one unpark, which ends the park.
#[test]
fn a_grant_round_trip_wakes_the_parked_worker_once() {
    let fixture = std::rc::Rc::new(RefCell::new(Fixture::new(true)));
    let (main, pane, handshake) = {
        let mut fixture = fixture.borrow_mut();
        let main = fixture.main;
        let pane = fixture.missed_at_t0(main);
        publish_output(&fixture.app, main);
        (main, pane, fixture.handshake(main, pane))
    };
    let sent = std::rc::Rc::new(Cell::new(None));
    let (in_park, sent_in_park) = (std::rc::Rc::clone(&fixture), std::rc::Rc::clone(&sent));
    let probe = Step::Probe(Box::new(move || {
        let mut fixture = in_park.borrow_mut();
        let (generation, deadline): (u64, Instant) = sent_in_park.get().expect("sent first");
        set_fake_now(fixture.at(3_300));
        fixture.app.handle_parser_yielded(main, pane, generation, deadline);
        assert_eq!(fixture.attempt(main, 4_000, 4_000, Busy::Free), Attempt::Collected);
    }));
    set_fake_now(fixture.borrow().at(3_000));
    let mut park = FakePark::new(&handshake, [probe], 1);
    let outcome = yield_step(
        &handshake,
        &mut 0,
        None,
        || Some(main),
        |_, generation, deadline| {
            sent.set(Some((generation, deadline)));
            true
        },
        &mut park,
    );
    assert!(matches!(outcome, YieldOutcome::Sent(YieldWait { parks: 1, timed_out: false, .. })));
    assert_eq!(fixture.borrow().counts(main), (1, 1, 0, 1, 0));
    let log = take_serve_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].unparked);
}

/// §5.1 8: a collection serves only once every visible parser guard is held, before images: a
/// parser miss serves nothing, an image miss has already served.
#[test]
fn a_collection_serves_under_the_parser_guards_before_images() {
    let mut fixture = Fixture::new(true);
    let main = fixture.main;
    let pane = fixture.active_pane(main);
    let handshake = fixture.handshake(main, pane);
    let generation = handshake.request();
    set_fake_now(fixture.at(0));
    assert_eq!(fixture.collect(main, Busy::Parser(pane)), Attempt::Missed);
    assert_eq!(handshake.served.load(Ordering::Acquire), 0, "no serve without the guard");
    assert_eq!(fixture.collect(main, Busy::Images(pane)), Attempt::Missed);
    assert!(handshake.served.load(Ordering::Acquire) >= generation, "served before images");
}

/// §5.1 9: a batch parsed by a worker that then yields leaves the same grid, OSC 7 directory,
/// title, modes, replies, events and media as one parsed by a worker with no request.
#[test]
fn a_yield_changes_nothing_the_batch_produced() {
    let bytes: &[u8] = b"\x1b]7;file://host/tmp/dir\x1b\\\x1b]2;title\x07\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\x1b[?1000h\x1b[c\x1b]133;A\x07\x1b_Gf=100,a=T;image\x1b\\tail";
    let run = |yielding: bool| {
        let media_pool = crate::app::media::InlineMediaPool::new();
        let parser = sonicterm_vt::vt::Parser::new_with_staging_pool(
            sonicterm_grid::grid::Grid::new(80, 24),
            None,
            sonicterm_vt::vt::CaptureStagingPool::new(),
        );
        let pane = crate::app::PaneState::new_with_media_pool(
            Arc::new(parking_lot::Mutex::new(parser)),
            None,
            &media_pool,
        );
        let handles = crate::app::spawn_pane::PaneVtHandles::from_pane_state(&pane);
        *pane.redraw_target.lock() = Some(WindowId::from(9));
        set_fake_now(test_base());
        if yielding {
            pane.parser_yield.request();
        }
        let script = if yielding { vec![Step::Timeout(Duration::ZERO)] } else { Vec::new() };
        let mut park = FakePark::new(&pane.parser_yield, script, u32::from(yielding));
        let mut flush = crate::app::spawn_pane::OutputFlush::new(1, &handles);
        let (mut replies, mut events) = (Vec::new(), 0);
        let outcome = crate::app::spawn_pane::worker_batch(
            &handles,
            &mut flush,
            bytes.len(),
            |latch, now| {
                crate::app::spawn_pane::publish_pane_vt_batch_with(
                    &handles,
                    bytes,
                    &mut None,
                    latch,
                    |media| {
                        Some(sonicterm_render_model::InlineImage {
                            id: 1,
                            row: media.row,
                            col: media.col,
                            width: 1,
                            height: 1,
                            bgra: Arc::from([1, 2, 3, 255]),
                        })
                    },
                    |_| events += 1,
                    || now,
                    |reply| replies.extend(reply),
                );
            },
            &mut 0,
            |_, _, _| true,
            &|| {},
            &mut park,
        );
        assert_eq!(matches!(outcome, YieldOutcome::Sent(_)), yielding);
        let image_count = pane.inline_images.lock().len();
        let command_count = pane.command_events.lock().len();
        let parser = pane.parser.lock();
        // Hyperlink ids come from a process-wide allocator, so the dump compares them by
        // position; the registry's size compares what they name.
        let grid = format!("{:?}", parser.grid());
        let mut normalized = String::with_capacity(grid.len());
        let mut rest = grid.as_str();
        while let Some(start) = rest.find("HyperlinkId(") {
            normalized.push_str(&rest[..start + "HyperlinkId(".len()]);
            rest = &rest[start + "HyperlinkId(".len()..];
            rest = rest.trim_start_matches(|digit: char| digit.is_ascii_digit());
        }
        normalized.push_str(rest);
        let produced = (
            normalized,
            parser.hyperlinks().len(),
            parser.cwd().map(str::to_owned),
            parser.title().map(str::to_owned),
            parser.keyboard_input_snapshot(),
            parser.pointer_input_snapshot(),
            replies,
            events,
            image_count,
            command_count,
            pane.output_generation.load(Ordering::Acquire),
        );
        produced
    };
    assert_eq!(run(true), run(false));
}
