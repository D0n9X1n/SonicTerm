//! Pins the worker half of the renderer-waiting handshake on a fake clock and an explicit park
//! token: the store-before-unpark service contract, the fixed park deadline, and the grant count.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
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
    /// The clock moves by the duration and the park returns.
    Advance(Duration),
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
    token: Rc<Cell<bool>>,
    /// The generation the current `Serve` step is serving, read by the unpark hook.
    serving: Rc<Cell<u64>>,
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
        let token = Rc::new(Cell::new(false));
        let serving = Rc::new(Cell::new(0));
        let target = Arc::as_ptr(handshake) as usize;
        let (hook_token, hook_serving) = (Rc::clone(&token), Rc::clone(&serving));
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
            Step::Advance(by) => set_fake_now(fake_now() + by),
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
    let (outcome, sent) = step(&handshake, &mut handshake.served.load(Ordering::Acquire), Some(base), &mut park);
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
