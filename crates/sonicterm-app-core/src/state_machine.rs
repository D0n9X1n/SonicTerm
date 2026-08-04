//! The `AppStateMachine`: the boundary the platform shell drives.
//!
//! `handle` reduces one intent and returns a stable class-sorted effect batch.
//! `drain_pending` exposes the private bounded follow-on queue; the current
//! reducer and production boundary do not enqueue into it.

use smallvec::SmallVec;

use crate::app_state::AppState;
use crate::effect::AppEffect;
use crate::intent::AppIntent;

/// Maximum number of internal queued effects `drain_pending` accepts before
/// treating the queue as broken. This does not bound the normal effect batch
/// returned directly by `handle`.
pub const MAX_CASCADE_DEPTH: usize = 16;

/// Pure-data state machine driven by the platform shell.
///
/// The shell calls `handle(intent)` once per intent and consumes the returned
/// `SmallVec<[AppEffect; 4]>`. Reducer arms express normal multi-effect
/// operations in that batch. The private `pending` queue is retained as an
/// internal/tested extension point but has no production enqueue path today.
pub struct AppStateMachine {
    state: AppState,
    pending: SmallVec<[AppEffect; 8]>,
}

impl AppStateMachine {
    /// Build a fresh state machine wrapping `initial`.
    #[must_use]
    pub fn new(initial: AppState) -> Self {
        Self { state: initial, pending: SmallVec::new() }
    }

    /// Read-only access to current state.
    #[must_use]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Dispatch one Intent, returning the sorted-by-`EffectClass`
    /// Effect batch.
    ///
    pub fn handle(&mut self, intent: AppIntent) -> SmallVec<[AppEffect; 4]> {
        let mut out: SmallVec<[AppEffect; 4]> = SmallVec::new();
        crate::reducer::reduce_leaf(&mut self.state, intent, &mut out);
        // Dispatch contract: stable sort by class so downstream
        // consumers see PtyWrite < Render < OsDrag < Clipboard <
        // WindowOp < MenubarUpdate < Log (spec §6).
        out.sort_by_key(AppEffect::effect_class);
        out
    }

    /// Drain internal follow-on effects in canonical class order. The current
    /// reducer and public boundary never seed this queue, so production calls
    /// return an empty vector; in-crate tests exercise ordering and the bound.
    ///
    /// Bounded by `MAX_CASCADE_DEPTH`. Debug builds panic on overflow; release
    /// builds log at `error!` and truncate.
    pub fn drain_pending(&mut self) -> Vec<AppEffect> {
        let mut out: Vec<AppEffect> = Vec::with_capacity(self.pending.len());
        let mut depth: usize = 0;
        while let Some(effect) = self.pending.pop() {
            depth = depth.saturating_add(1);
            if depth > MAX_CASCADE_DEPTH {
                // When: `depth` exceeds `MAX_CASCADE_DEPTH`, stop the broken cascade instead of draining an unbounded queue.
                #[cfg(debug_assertions)]
                {
                    panic!("MAX_CASCADE_DEPTH ({}) exceeded in drain_pending", MAX_CASCADE_DEPTH);
                }
                #[cfg(not(debug_assertions))]
                {
                    tracing::error!(
                        target: "state_machine",
                        "drain_pending exceeded MAX_CASCADE_DEPTH ({}); truncating {} pending",
                        MAX_CASCADE_DEPTH,
                        self.pending.len() + 1
                    );
                    self.pending.clear();
                    break;
                }
            }
            out.push(effect);
        }
        out.sort_by_key(AppEffect::effect_class);
        out
    }
}

#[cfg(test)]
#[path = "state_machine_tests.rs"]
mod state_machine_tests;
