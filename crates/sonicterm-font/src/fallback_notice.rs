//! The notice a fallback worker completes when it publishes faces for a configuration.
//!
//! Frame shaping never waits for fallback discovery, so a renderer learns about a new face
//! through this notice instead: the worker bumps its generation after it releases the
//! pending-handle lock, and at most one wake event per notice is ever undelivered.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

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
        let mut delivery = self.delivery();
        if delivery.posted {
            // When: an event is already undelivered, its handler reads this generation too.
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
            // When: a completion arrived before any waker, deliver it exactly once now.
            delivery.owed = false;
            delivery.posted = true;
            waker(self.id);
        }
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
