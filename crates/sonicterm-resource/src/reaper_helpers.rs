#[cfg(test)]
use super::FAIL_HELPER_SPAWN;
use super::{
    GrantWorkerSlot, HelperClaim, HelperGrant, HelperGrantState, HelperSlot, ReapResult, ReapTask,
    ReaperSupervisor,
};
use std::sync::Arc;

impl HelperGrant {
    /// Number of worker slots that this grant keeps reserved for its whole lifetime.
    pub fn capacity(&self) -> usize {
        self.inner.occupied.len()
    }

    /// Start a worker in one named grant slot; slot zero is reserved for the outer teardown helper.
    ///
    /// Refusal or spawn failure drops the closure, so callers retain recoverable native values in shared slots.
    pub fn spawn<T: Send + 'static>(
        &self,
        slot: usize,
        name: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<std::thread::JoinHandle<T>> {
        let Some(occupied) = self.inner.occupied.get(slot) else {
            // When: slot is outside the complete grant, refuse unaccounted helper creation.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "worker slot is outside its grant",
            ));
        };
        if occupied
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            // When: occupied is already true, retry cannot run a second worker in the same native phase slot.
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "worker grant slot is occupied",
            ));
        }
        let worker_slot = GrantWorkerSlot { grant: self.clone(), slot };
        let scoped = move || {
            let _slot = worker_slot;
            work()
        };
        #[cfg(test)]
        if FAIL_HELPER_SPAWN.get() {
            // When: FAIL_HELPER_SPAWN is active, drop the closure just as an OS refusal would, retaining the task's grant.
            drop(scoped);
            return Err(std::io::Error::other("injected helper spawn failure"));
        }
        std::thread::Builder::new().name(name.to_owned()).spawn(scoped)
    }
}

impl ReaperSupervisor {
    /// Start blocking work on a helper without waiting for it.
    ///
    /// Returns the work and its task if no helper was free, so the caller can
    /// retry the *same* call later. Asking the task for a fresh closure instead
    /// would lose the work: `next_action` has already advanced the task's
    /// state by the time it hands the closure over, so a second call reports
    /// the step as done when it never ran.
    ///
    /// Joining here rather than deferring would hold the helper slot for the
    /// call's whole duration, which makes more than one helper unreachable and
    /// lets a single hung native call stall every other owner's teardown.
    #[allow(clippy::type_complexity)]
    pub(super) fn start_on_helper(
        &self,
        work: Box<dyn FnOnce() -> ReapResult + Send>,
        mut task: Box<dyn ReapTask>,
        in_flight: &mut Vec<(std::thread::JoinHandle<ReapResult>, Box<dyn ReapTask>)>,
    ) -> Option<(Box<dyn FnOnce() -> ReapResult + Send>, Box<dyn ReapTask>)> {
        let claim = task.helper_claim();
        let demand = match claim {
            HelperClaim::PerCall => 1,
            HelperClaim::Grant(count) => count,
            HelperClaim::Held => 0,
        };
        // Admission claims a complete grant under counters, then invokes task code only after unlocking.
        {
            let mut counters = self.state.counters.lock();
            if !self.helper_claim_fits(claim, &counters) {
                // When: helper_claim_fits rejects claim, return untouched work without starting a partial unit.
                return Some((work, task));
            }
            counters.helpers += demand;
        }
        let grant = match claim {
            HelperClaim::PerCall => None,
            HelperClaim::Grant(count) => {
                let grant = HelperGrant {
                    inner: Arc::new(HelperGrantState {
                        state: self.state.clone(),
                        occupied: (0..count)
                            .map(|_| std::sync::atomic::AtomicBool::new(false))
                            .collect(),
                    }),
                };
                task.accept_helper_grant(grant.clone());
                Some(grant)
            }
            HelperClaim::Held => task.held_helper_grant(),
        };
        if let Some(grant) = grant {
            // When: grant supplies the outer slot, a retry reuses its held permits even after a failed spawn.
            return match grant.spawn(0, "sonic-reaper-helper", work) {
                Ok(handle) => {
                    in_flight.push((handle, task));
                    None
                }
                Err(_) => Some((Box::new(|| ReapResult::Failed), task)),
            };
        }
        if claim == HelperClaim::Held {
            // When: Held has no retained grant, do not start an uncounted worker for an invalid task implementation.
            return Some((work, task));
        }
        // The helper releases its own slot when the call returns, rather than
        // the loop releasing it on join. A call abandoned at the deadline is
        // never joined, so a loop-side release would keep its slot forever and
        // the pool would shrink by one on every abandonment until no blocking
        // work could start again — trading a hang for a silent, permanent
        // stall. Releasing here means the slot is held exactly as long as the
        // call actually runs.
        //
        // The release is a drop guard rather than a statement after the call,
        // because a panicking call unwinds straight past a statement. Cleanup
        // work panicking is far more reachable than a native call wedging, and
        // it would leak the slot just as permanently.
        let state = self.state.clone();
        let scoped: Box<dyn FnOnce() -> ReapResult + Send> = Box::new(move || {
            let _slot = HelperSlot { state };
            work()
        });
        // A thread the OS refuses is a resource failure, not a panic: this
        // crate exists to stay standing under exhaustion.
        let spawn = || {
            #[cfg(test)]
            if FAIL_HELPER_SPAWN.get() {
                // When: FAIL_HELPER_SPAWN is set on this test thread, discard scoped
                // like an OS spawn refusal and exercise the same error branch.
                drop(scoped);
                return Err(std::io::Error::other("injected helper spawn failure"));
            }
            std::thread::Builder::new().name("sonic-reaper-helper".to_owned()).spawn(scoped)
        };
        match spawn() {
            Ok(handle) => {
                in_flight.push((handle, task));
                None
            }
            Err(_) => {
                self.state.counters.lock().helpers -= 1;
                // The failed spawn drops the closure, not the task permit. Return
                // a synthetic Failed call with the task so retries cannot silently
                // skip its step before cancellation or settlement.
                Some((Box::new(|| ReapResult::Failed), task))
            }
        }
    }
}

#[cfg(test)]
#[path = "reaper_helpers_tests.rs"]
mod reaper_helpers_tests;
