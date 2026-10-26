use super::*;
use crate::clock::{SystemClock, TestClock};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn owner(id: u64) -> ResourceOwnerId {
    ResourceOwnerId::new(id).unwrap()
}

fn limits() -> ReaperLimits {
    ReaperLimits::new(2, 1, 2).unwrap()
}

pub(super) fn supervisor(clock: &TestClock) -> ReaperSupervisor {
    ReaperSupervisor::new(limits(), Arc::new(clock.clone()))
}

/// Never settles; always asks to be polled past any deadline.
pub(super) struct StuckTask {
    pub(super) owner: ResourceOwnerId,
    pub(super) forced: Arc<AtomicUsize>,
    pub(super) settles_on_force: bool,
}

impl ReapTask for StuckTask {
    fn owner(&self) -> ResourceOwnerId {
        self.owner
    }
    fn next_action(&mut self, now: Instant) -> ReapAction {
        ReapAction::PollAfter(now + Duration::from_secs(3600))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        self.forced.fetch_add(1, Ordering::Relaxed);
        if self.settles_on_force {
            CancelOutcome::Settled
        } else {
            CancelOutcome::TimedOut
        }
    }
}

/// An opt-in unit keeps its whole helper grant in task/closure shared custody across retries.
pub(super) struct GrantedTestTask {
    pub(super) id: u64,
    pub(super) grant: Arc<Mutex<Option<HelperGrant>>>,
    pub(super) calls: VecDeque<Box<dyn FnOnce() -> ReapResult + Send>>,
    pub(super) installed: Arc<AtomicUsize>,
    pub(super) installed_without_counters: Arc<std::sync::atomic::AtomicBool>,
    pub(super) state: Arc<SupervisorState>,
}

impl ReapTask for GrantedTestTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(self.id)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        self.calls
            .pop_front()
            .map_or(ReapAction::Complete(ReapResult::Settled), ReapAction::RunBlocking)
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
    fn helper_claim(&self) -> HelperClaim {
        if self.grant.lock().is_some() {
            HelperClaim::Held
        } else {
            HelperClaim::Grant(5)
        }
    }
    fn accept_helper_grant(&mut self, grant: HelperGrant) {
        self.installed_without_counters
            .store(self.state.counters.try_lock().is_some(), Ordering::SeqCst);
        self.installed.fetch_add(1, Ordering::SeqCst);
        *self.grant.lock() = Some(grant);
    }
    fn held_helper_grant(&self) -> Option<HelperGrant> {
        self.grant.lock().clone()
    }
}

/// Report the first actual clock wait so deadline publication crosses an already sleeping run.
pub(super) struct WaitObservedClock {
    pub(super) entered: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
}

impl Clock for WaitObservedClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wait_until(&self, deadline: Instant) {
        if let Some(entered) = self.entered.lock().take() {
            let _ = entered.try_send(());
        }
        SystemClock.wait_until(deadline);
    }
}

/// Retains an abandoned helper separately from payload state, exposing completion only after native work succeeds.
pub(super) struct CollectableTestTask {
    pub(super) id: u64,
    pub(super) release: Option<std::sync::mpsc::Receiver<()>>,
    pub(super) started: std::sync::mpsc::SyncSender<()>,
    pub(super) complete: Arc<std::sync::atomic::AtomicBool>,
    pub(super) abandoned: Arc<Mutex<Vec<std::thread::JoinHandle<ReapResult>>>>,
    pub(super) joined: Arc<AtomicUsize>,
}

impl ReapTask for CollectableTestTask {
    fn owner(&self) -> ResourceOwnerId {
        owner(self.id)
    }
    fn next_action(&mut self, _now: Instant) -> ReapAction {
        let release = self.release.take().expect("controlled task starts once");
        let complete = self.complete.clone();
        let started = self.started.clone();
        ReapAction::RunBlocking(Box::new(move || {
            let _ = started.try_send(());
            if release.recv_timeout(Duration::from_secs(5)).is_err() {
                return ReapResult::Failed;
            }
            complete.store(true, Ordering::SeqCst);
            ReapResult::Settled
        }))
    }
    fn on_completion(&mut self, _result: ReapResult) {}
    fn force_cancel(&mut self) -> CancelOutcome {
        CancelOutcome::TimedOut
    }
    fn keep_abandoned_helper(&mut self, handle: std::thread::JoinHandle<ReapResult>) {
        self.abandoned.lock().push(handle);
    }
    fn is_collectable(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
            && self.abandoned.lock().iter().all(std::thread::JoinHandle::is_finished)
    }
    fn join_finished_helpers(&mut self) {
        for handle in self.abandoned.lock().drain(..) {
            assert!(handle.is_finished(), "collector must not join an unfinished helper");
            let _ = handle.join();
            self.joined.fetch_add(1, Ordering::SeqCst);
        }
    }
}

std::thread_local! {
    // Notify only this test's first actual condition-variable wait, while counters still excludes shutdown.
    pub(super) static SLOT_WAIT_STARTED: std::cell::RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const {
        std::cell::RefCell::new(None)
    };
    // Inject the closed-admission timeout branch once; this does not pretend to reproduce a scheduling race.
    pub(super) static CLOSE_AT_SLOT_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Expose the actual wait boundary and optional branch-state injection while counters is held.
pub(super) fn slot_wait_started(counters: &mut Counters) {
    if CLOSE_AT_SLOT_WAIT.replace(false) {
        counters.admitting = false;
    }
    SLOT_WAIT_STARTED.with(|signal| {
        if let Some(signal) = signal.borrow_mut().take() {
            let _ = signal.try_send(());
        }
    });
}
