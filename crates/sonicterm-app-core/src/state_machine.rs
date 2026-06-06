//! The `AppStateMachine`: the boundary the platform shell drives.
//!
//! M6a-expand-2a (THIS PR): API surface + cascade-bound guard + sort.
//! Reducer arms return an empty Effect batch for every Intent; per-
//! Intent state mutation logic lands in 2b/2c.

use smallvec::SmallVec;

use crate::app_state::AppState;
use crate::effect::AppEffect;
use crate::intent::AppIntent;

/// Maximum cascade iterations a single `handle` call may execute
/// inside `drain_pending` before the state machine considers itself
/// broken. Per spec §5: 4× the deepest known legitimate cascade
/// (close-last-tab → WindowCloseRequested → quit → flush all →
/// menubar rebuild = 4).
pub const MAX_CASCADE_DEPTH: usize = 16;

/// Pure-data state machine driven by the platform shell.
///
/// The shell calls `handle(intent)` once per Intent and consumes the
/// returned `SmallVec<[AppEffect; 4]>`. Cascaded follow-on Intents
/// reducer arms enqueue go through `pending`; `drain_pending`
/// flattens them, bounded by `MAX_CASCADE_DEPTH`.
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
    /// M6a-expand-2a stub: every Intent returns `SmallVec::new()`.
    /// Per-Intent reducer arms land in 2b/2c (see spec §9 + §3).
    pub fn handle(&mut self, intent: AppIntent) -> SmallVec<[AppEffect; 4]> {
        let mut out: SmallVec<[AppEffect; 4]> = SmallVec::new();
        crate::reducer::reduce_leaf(&mut self.state, intent, &mut out);
        // Dispatch contract: stable sort by class so downstream
        // consumers see PtyWrite < Render < OsDrag < Clipboard <
        // WindowOp < MenubarUpdate < Log (spec §6).
        out.sort_by_key(AppEffect::effect_class);
        out
    }

    /// Drain any side-effects the reducer queued internally during
    /// cascade. The state machine never accumulates pending events
    /// across `handle` calls in M6a-expand-2a (the stub reducer
    /// pushes nothing); this method exists so the boundary the shell
    /// integrates against is stable for 2b/2c.
    ///
    /// Bounded by `MAX_CASCADE_DEPTH`. Debug builds panic on
    /// overflow; release builds log at `error!` + truncate.
    pub fn drain_pending(&mut self) -> Vec<AppEffect> {
        let mut out: Vec<AppEffect> = Vec::with_capacity(self.pending.len());
        let mut depth: usize = 0;
        while let Some(effect) = self.pending.pop() {
            depth = depth.saturating_add(1);
            if depth > MAX_CASCADE_DEPTH {
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
                    return out;
                }
            }
            out.push(effect);
        }
        out.sort_by_key(AppEffect::effect_class);
        out
    }
}
