//! Parser-guard custody and frame dispatch timing for the frame counters.
//!
//! A counting frame takes one [`GuardCustody`] and one [`DispatchClock`] right after its first parser guard
//! is acquired. The custody token travels with the guards and records first-guard-to-last-release when it
//! drops after them; the dispatch clock records first-guard-to-render-return for a rendered frame, or the
//! interval to its release for a frame that never rendered, which is its own population. A frame whose
//! window does not count takes neither, so the only cost left is the branch that declines them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Sums and counts of every counting frame's guard custody and dispatch, App-wide.
#[derive(Debug, Default)]
pub(crate) struct CustodyTotals {
    /// Nanoseconds from the first parser guard acquired to the last released, over every frame.
    pub(crate) custody_ns: AtomicU64,
    /// Frames that held at least one parser guard.
    pub(crate) custodies: AtomicU64,
    /// Nanoseconds from the first guard to the return of `render_releasing`, over rendered frames.
    pub(crate) dispatch_ns: AtomicU64,
    /// Frames that reached `render_releasing`.
    pub(crate) dispatches: AtomicU64,
    /// Nanoseconds from the first guard to the release, over frames that failed before render.
    pub(crate) dispatch_failed_ns: AtomicU64,
    /// Frames that held a guard but never rendered.
    pub(crate) dispatches_failed: AtomicU64,
}

/// One frame's guard custody; it records when it drops, which its owner arranges right after the last guard.
// A seam for span correlation: a per-pane record of this interval, with a sequence id, would be taken here.
#[derive(Debug)]
pub(crate) struct GuardCustody {
    totals: Arc<CustodyTotals>,
    first_guard: Instant,
}

/// One frame's dispatch interval; `rendered` closes it in the rendered population, a drop in the failed one.
#[derive(Debug)]
pub(crate) struct DispatchClock {
    totals: Arc<CustodyTotals>,
    first_guard: Instant,
    /// Set by `rendered`, so the drop records the rendered population instead of the failed one.
    rendered: bool,
    /// The renderer's return, when the caller read it once for every recorder; else the drop reads now.
    ended: Option<Instant>,
}

/// Start a counting frame's custody and dispatch at its first guard, now.
pub(crate) fn start(totals: &Arc<CustodyTotals>) -> (GuardCustody, DispatchClock) {
    start_at(totals, Instant::now())
}

/// Start them at `first_guard`, so a test can place the first guard in the past.
pub(crate) fn start_at(
    totals: &Arc<CustodyTotals>,
    first_guard: Instant,
) -> (GuardCustody, DispatchClock) {
    (
        GuardCustody { totals: Arc::clone(totals), first_guard },
        DispatchClock { totals: Arc::clone(totals), first_guard, rendered: false, ended: None },
    )
}

impl DispatchClock {
    /// Close the interval at the return of `render_releasing`, read now, in the rendered population.
    #[cfg(test)]
    pub(crate) fn rendered(mut self) {
        self.rendered = true;
    }

    /// Close the interval at `returned_at`, the renderer's return as read once by the adapter, so a
    /// diagnostic recorder working after that read never extends the interval.
    pub(crate) fn rendered_at(mut self, returned_at: Instant) {
        self.rendered = true;
        self.ended = Some(returned_at);
    }
}

/// Add one interval from `first_guard` to `ended`, or to now, into `sum_ns`, and one to `count`.
// Ordering: sum_ns and count use Relaxed; they are statistics a later snapshot reads, ordering nothing.
fn add_interval(
    sum_ns: &AtomicU64,
    count: &AtomicU64,
    first_guard: Instant,
    ended: Option<Instant>,
) {
    let ended = ended.unwrap_or_else(Instant::now);
    let elapsed = ended.saturating_duration_since(first_guard);
    let elapsed_ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
    sum_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
    count.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
thread_local! {
    /// A test's probe, run as a custody records, so it can check which parser guards are still held then.
    pub(crate) static CUSTODY_DROP_PROBE: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

// Lifecycle: dropping GuardCustody, after the frame's last parser guard, calls add_interval once for its custody.
impl Drop for GuardCustody {
    fn drop(&mut self) {
        #[cfg(test)]
        CUSTODY_DROP_PROBE.with(|probe| {
            if let Some(probe) = probe.borrow_mut().as_mut() {
                probe();
            }
        });
        add_interval(&self.totals.custody_ns, &self.totals.custodies, self.first_guard, None);
    }
}

// Lifecycle: dropping DispatchClock calls add_interval once, failed unless `rendered` was set.
impl Drop for DispatchClock {
    fn drop(&mut self) {
        let totals = &self.totals;
        if self.rendered {
            add_interval(&totals.dispatch_ns, &totals.dispatches, self.first_guard, self.ended);
        } else {
            // When: `rendered` is false, the frame held a guard but never reached the renderer.
            let first_guard = self.first_guard;
            add_interval(&totals.dispatch_failed_ns, &totals.dispatches_failed, first_guard, None);
        }
    }
}

#[cfg(test)]
#[path = "guard_custody_tests.rs"]
mod guard_custody_tests;
