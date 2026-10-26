#[cfg(doc)]
use super::ReapTask;
use super::{ReaperSupervisor, SupervisorState};

/// Returns task permits only after removed retained tasks and their fields finish destruction.
struct ReleaseTaskPermits<'a> {
    state: &'a SupervisorState,
    count: usize,
}

// Lifecycle: ReleaseTaskPermits decrements tasks and notifies slot_released after retained destruction, including unwind.
impl Drop for ReleaseTaskPermits<'_> {
    fn drop(&mut self) {
        self.state.counters.lock().tasks -= self.count;
        self.state.slot_released.notify_all();
    }
}

impl ReaperSupervisor {
    /// Tasks retained because they finished without settling.
    ///
    /// Charges remain attributed until opted-in whole-task completion is collected,
    /// [`Self::release_retained`] ends terminal custody, or the supervisor is dropped.
    /// Each retained task also holds its admission permit and
    /// is included in [`Self::live_tasks`]. A non-zero count in a healthy process
    /// means something never gave its resources back.
    pub fn retained_tasks(&self) -> usize {
        self.state.retained.lock().len()
    }

    /// Collect opted-in whole-task completions after all retained worker handles have finished.
    ///
    /// Predicate checks happen under retained custody; joins and task destruction happen after that lock is released.
    /// Every opted-in task must own a unique transport owner: collection retracts its unresolved-owner record.
    /// Shared-owner tasks leave [`ReapTask::is_collectable`] at its default `false`.
    // Lock order: counters admission read ends before retained; retained releases before later counters updates and task destruction.
    pub fn collect_settled_retained(&self) -> usize {
        if !self.is_admitting() {
            // When: is_admitting is false, leave late completed custody and its diagnosis for terminal release_retained.
            return 0;
        }
        // Declare the permit guard first so removed task destructors run before counts return even during unwind.
        let mut permits = ReleaseTaskPermits { state: &self.state, count: 0 };
        let mut completed = Vec::new();
        {
            let mut retained = self.state.retained.lock();
            let mut index = 0;
            while index < retained.len() {
                if retained[index].is_collectable() {
                    completed.push(retained.swap_remove(index));
                    permits.count += 1;
                } else {
                    // When: is_collectable is false, incomplete native custody or an unfinished thread must remain retained.
                    index += 1;
                }
            }
        }
        let count = completed.len();
        for task in &mut completed {
            task.join_finished_helpers();
            let owner = task.owner();
            self.state.counters.lock().unresolved.retain(|unresolved| *unresolved != owner);
        }
        drop(completed);
        count
    }

    /// Surrender and destroy retained tasks, returning their admission permits.
    ///
    /// Returns the number of tasks released and wakes waiting reservers.
    ///
    /// Retention keeps an unsettled charge visible, but it also keeps the
    /// owner from closing, and a closed owner's parent from closing after it.
    /// Without this, one wedged transport pins its whole window subtree until
    /// the process exits. This is the terminal cleanup that unwinds it, so a
    /// caller can reclaim a stuck subtree instead of restarting.
    ///
    /// Permits return only after task destruction, including when surrender or a
    /// destructor unwinds. An ordinary `PerCall` helper keeps its one slot, while
    /// a grant worker's clone keeps the entire grant reserved. This method does
    /// not wait for either, and a later worker return does not settle the task.
    /// Owners already reported unresolved stay reported: surrender does not retract
    /// the diagnosis.
    // Release the retained guard before ReleaseTaskPermits locks counters;
    // settlement holds counters before retained, so these locks must not overlap here.
    pub fn release_retained(&self) -> usize {
        // Declared before retained so its drop runs after every task on unwind.
        let mut permits = ReleaseTaskPermits { state: &self.state, count: 0 };
        let mut retained = std::mem::take(&mut *self.state.retained.lock());
        let count = retained.len();
        permits.count = count;
        for task in &mut retained {
            task.surrender_charges();
        }
        drop(retained);
        count
    }
}

#[cfg(test)]
#[path = "reaper_retained_tests.rs"]
mod reaper_retained_tests;
