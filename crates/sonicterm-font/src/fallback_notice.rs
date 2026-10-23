//! The notice a fallback worker completes when it publishes faces for a configuration.
//!
//! Frame shaping never waits for fallback discovery, so a renderer learns about a new face
//! through this notice instead: the worker bumps its generation after it releases the
//! pending-handle lock, and at most one wake event per notice is ever undelivered.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::rangeset::RangeSet;

/// Codepoints each of the unresolved and lookup-failed sets keeps before it only flags overflow.
pub const UNRESOLVED_CAP: usize = 4096;

/// Called with the notice id when a completion must reach the event loop.
pub type FallbackWaker = Arc<dyn Fn(u64) + Send + Sync>;

static NEXT_NOTICE_ID: AtomicU64 = AtomicU64::new(1);

/// Who is told about a completion, and whether a told event is still undelivered.
#[derive(Default)]
struct Delivery {
    waker: Option<FallbackWaker>,
    /// An event was handed to the waker and its handler has not acknowledged it yet.
    posted: bool,
    /// A completion arrived before any waker was installed.
    owed: bool,
}

/// One configuration's fallback publication counter and its wake bookkeeping.
///
/// The frame path reads only `generation`; completions, installation and acknowledgement
/// take the `delivery` lock, which serializes them so no completion is lost.
pub struct FallbackNotice {
    id: u64,
    generation: AtomicU64,
    delivery: Mutex<Delivery>,
    /// Bumped whenever `unresolved` changes; readers lock it only when this moved.
    unresolved_revision: AtomicU64,
    /// The configuration's codepoints that no font covers, and those whose lookup failed.
    unresolved: Mutex<UnresolvedState>,
}

/// Codepoints a configuration's fallback search could not cover. It nests with no other lock.
#[derive(Debug, Default)]
struct UnresolvedState {
    /// Results from a face epoch before this one are discarded.
    reset_epoch: u64,
    /// Every lookup stage ran without error and none covered these.
    codepoints: RangeSet<u32>,
    /// A lookup stage failed for these, so they are not known to be uncovered.
    lookup_failed: RangeSet<u32>,
    /// A set reached [`UNRESOLVED_CAP`] and dropped a codepoint.
    overflow: bool,
}

/// One fallback request's outcome, published for the configuration's face epoch.
#[derive(Debug, Default)]
pub struct UnresolvedPublication {
    /// The face epoch the request was scheduled under.
    pub epoch: u64,
    /// Codepoints no font covers, when every stage ran without error.
    pub unresolved: Vec<char>,
    /// Codepoints left uncovered by a request in which some stage failed.
    pub lookup_failed: Vec<char>,
    /// Requested codepoints the request did cover; they leave both sets.
    pub covered: Vec<char>,
}

/// A copy of the unresolved set, read when its revision moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnresolvedSnapshot {
    /// The revision this copy reflects.
    pub revision: u64,
    /// Codepoints no font SonicTerm can find covers.
    pub codepoints: Vec<char>,
    /// A set overflowed, so membership cannot be proven for every codepoint.
    pub overflow: bool,
}

impl UnresolvedSnapshot {
    /// Whether `codepoint` is known to be unresolved.
    #[must_use]
    pub fn contains(&self, codepoint: char) -> bool {
        self.codepoints.binary_search(&codepoint).is_ok()
    }
}

impl std::fmt::Debug for FallbackNotice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter
            .debug_struct("FallbackNotice")
            .field("id", &self.id)
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl FallbackNotice {
    /// A notice with a process-unique id and generation 0.
    // Ordering: `NEXT_NOTICE_ID` is Relaxed; ids need uniqueness only and publish nothing.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_NOTICE_ID.fetch_add(1, Ordering::Relaxed),
            generation: AtomicU64::new(0),
            delivery: Mutex::new(Delivery::default()),
            unresolved_revision: AtomicU64::new(0),
            unresolved: Mutex::new(UnresolvedState::default()),
        })
    }

    /// This notice's process-unique id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The count of completions so far; frame preparation compares it with what it applied.
    // Ordering: Acquire pairs with `complete`'s AcqRel, so a reader that sees a generation also
    // sees the handles the worker appended before it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn delivery(&self) -> std::sync::MutexGuard<'_, Delivery> {
        self.delivery.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record one publication: bump the generation, then post one event unless one is
    /// already undelivered, or remember it as owed when no waker is installed yet.
    // Ordering: AcqRel publishes the worker's appended handles to `generation`'s Acquire readers.
    pub fn complete(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.post();
    }

    /// Post one event unless one is undelivered, or owe it when no waker is installed yet.
    fn post(&self) {
        let mut delivery = self.delivery();
        if delivery.posted {
            // When: `posted` means an event is undelivered; its handler reads this generation too.
            return;
        }
        match delivery.waker.clone() {
            Some(waker) => {
                delivery.posted = true;
                waker(self.id);
            }
            None => delivery.owed = true,
        }
    }

    /// Install the renderer's waker. An owed completion is delivered once now; a claim
    /// already posted keeps its queued event and is never cleared here.
    pub fn attach_waker(&self, waker: FallbackWaker) {
        let mut delivery = self.delivery();
        delivery.waker = Some(Arc::clone(&waker));
        if delivery.owed {
            // A completion arrived before any waker; deliver it exactly once now.
            delivery.owed = false;
            delivery.posted = true;
            waker(self.id);
        }
    }

    fn unresolved_state(&self) -> std::sync::MutexGuard<'_, UnresolvedState> {
        self.unresolved.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Apply one request's outcome unless its epoch predates the last reset; when the sets
    /// changed, bump the revision and post a wake as `complete` does, leaving `generation` alone.
    /// Returns whether anything changed.
    // Ordering: unresolved_revision is bumped Release after the sets change under their lock,
    // pairing with unresolved_revision()'s Acquire.
    pub fn publish(&self, publication: &UnresolvedPublication) -> bool {
        let changed = {
            let mut state = self.unresolved_state();
            if publication.epoch < state.reset_epoch {
                // When: publication.epoch is before reset_epoch, the result is for an old configuration.
                return false;
            }
            let before = (state.codepoints.clone(), state.lookup_failed.clone(), state.overflow);
            for codepoint in &publication.covered {
                state.codepoints.remove(*codepoint as u32);
                state.lookup_failed.remove(*codepoint as u32);
            }
            for codepoint in &publication.unresolved {
                state.lookup_failed.remove(*codepoint as u32);
                if state.codepoints.contains(*codepoint as u32) {
                    // When: codepoints already contains this codepoint, there is nothing to add.
                    continue;
                }
                if state.codepoints.len() as usize >= UNRESOLVED_CAP {
                    state.overflow = true;
                } else {
                    // When: codepoints is below UNRESOLVED_CAP, the codepoint is kept exactly.
                    state.codepoints.add(*codepoint as u32);
                }
            }
            for codepoint in &publication.lookup_failed {
                state.codepoints.remove(*codepoint as u32);
                if state.lookup_failed.contains(*codepoint as u32) {
                    // When: lookup_failed already contains this codepoint, there is nothing to add.
                    continue;
                }
                if state.lookup_failed.len() as usize >= UNRESOLVED_CAP {
                    state.overflow = true;
                } else {
                    // When: lookup_failed is below UNRESOLVED_CAP, the codepoint is kept exactly.
                    state.lookup_failed.add(*codepoint as u32);
                }
            }
            let changed =
                before != (state.codepoints.clone(), state.lookup_failed.clone(), state.overflow);
            if changed {
                self.unresolved_revision.fetch_add(1, Ordering::Release);
            }
            changed
        };
        if changed {
            self.post();
        }
        changed
    }

    /// Forget both sets for a new configuration whose face epoch is `epoch`; any result scheduled
    /// before it is discarded when it arrives.
    // Ordering: unresolved_revision is bumped Release under the lock, pairing with its Acquire load.
    pub fn reset(&self, epoch: u64) {
        let mut state = self.unresolved_state();
        let had_any =
            !state.codepoints.is_empty() || !state.lookup_failed.is_empty() || state.overflow;
        *state = UnresolvedState { reset_epoch: epoch, ..UnresolvedState::default() };
        if had_any {
            self.unresolved_revision.fetch_add(1, Ordering::Release);
        }
    }

    /// The revision of the unresolved set; it moves whenever the set changes.
    // Ordering: unresolved_revision is loaded Acquire, pairing with publish's and reset's Release.
    #[must_use]
    pub fn unresolved_revision(&self) -> u64 {
        self.unresolved_revision.load(Ordering::Acquire)
    }

    /// A copy of the unresolved set with its revision, taken under the set's lock.
    // Ordering: unresolved_revision is loaded Relaxed under the lock every writer holds.
    #[must_use]
    pub fn unresolved_snapshot(&self) -> UnresolvedSnapshot {
        let state = self.unresolved_state();
        UnresolvedSnapshot {
            revision: self.unresolved_revision.load(Ordering::Relaxed),
            codepoints: state.codepoints.iter_values().filter_map(char::from_u32).collect(),
            overflow: state.overflow,
        }
    }

    /// Codepoints whose last lookup failed, in order; for diagnostics and tests.
    #[must_use]
    pub fn lookup_failed(&self) -> Vec<char> {
        self.unresolved_state().lookup_failed.iter_values().filter_map(char::from_u32).collect()
    }

    /// The handler's acknowledgement of a delivered event: clear the claim, then read the
    /// generation, so a completion before this is seen here and one after it posts again.
    pub fn acknowledge(&self) -> u64 {
        self.delivery().posted = false;
        self.generation()
    }
}

#[cfg(test)]
#[path = "fallback_notice_tests.rs"]
mod fallback_notice_tests;
