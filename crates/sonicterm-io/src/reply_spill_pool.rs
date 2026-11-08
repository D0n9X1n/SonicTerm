//! The process-wide disk budget every pane's reply spill draws from.
//!
//! Each spool keeps its own live backlog under [`crate::SPILL_BUDGET_BYTES`]; this pool bounds the
//! sum across spools. It charges each spill file's length (consumed prefix included), so the summed
//! file lengths stay within the pool's capacity. It is file-length accounting, not an exact bound on
//! filesystem allocation: block rounding and metadata are outside it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// The default process budget for all reply spill files: sixteen panes at the per-pane cap.
pub const PROCESS_SPILL_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// A shared budget of spill-file bytes. Spools reserve before they write and refund what they free.
#[derive(Debug)]
pub struct SpillPool {
    capacity: u64,
    charged: AtomicU64,
    /// The most bytes ever charged at once, for tests and reports.
    peak: AtomicU64,
}

impl SpillPool {
    /// A pool admitting at most `capacity` charged bytes.
    pub fn new(capacity: u64) -> Arc<Self> {
        Arc::new(Self { capacity, charged: AtomicU64::new(0), peak: AtomicU64::new(0) })
    }

    /// The pool every production spool shares.
    pub fn process_default() -> Arc<Self> {
        static POOL: OnceLock<Arc<SpillPool>> = OnceLock::new();
        Arc::clone(POOL.get_or_init(|| Self::new(PROCESS_SPILL_BUDGET_BYTES)))
    }

    /// Charge `bytes` if the pool can hold them; `false` leaves the pool unchanged.
    // Ordering: charged Acquire load, then AcqRel/Acquire compare_exchange_weak so racing reservations
    // never pass capacity; peak Relaxed fetch_max is a statistic.
    pub(crate) fn try_reserve(&self, bytes: u64) -> bool {
        let mut charged = self.charged.load(Ordering::Acquire);
        loop {
            let Some(total) = charged.checked_add(bytes).filter(|total| *total <= self.capacity)
            else {
                // When: adding `bytes` would pass the capacity, nothing is charged.
                return false;
            };
            match self.charged.compare_exchange_weak(
                charged,
                total,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // When: the exchange stored `total`, the reservation holds.
                    self.peak.fetch_max(total, Ordering::Relaxed);
                    return true;
                }
                // Another reservation moved `charged` to `current`; retry from it.
                Err(current) => charged = current,
            }
        }
    }

    /// Return `bytes` a spool no longer holds.
    // Ordering: charged AcqRel pairs with try_reserve's compare-exchange.
    pub(crate) fn refund(&self, bytes: u64) {
        let previous = self.charged.fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(previous >= bytes, "a spool refunded {bytes} B of {previous} B charged");
    }

    /// Bytes charged now across every spool sharing this pool.
    // Ordering: charged Acquire; a sampled gauge.
    pub fn charged(&self) -> u64 {
        self.charged.load(Ordering::Acquire)
    }

    /// The pool's capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// The most bytes ever charged at once.
    // Ordering: peak Relaxed; a statistic.
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }
}

/// One spill file's charge against its pool, refunded when the file shrinks or is dropped.
#[derive(Debug)]
pub(crate) struct SpillCharge {
    pool: Arc<SpillPool>,
    held: u64,
}

impl SpillCharge {
    /// A charge holding nothing yet.
    pub(crate) fn new(pool: Arc<SpillPool>) -> Self {
        Self { pool, held: 0 }
    }

    /// Grow the charge by `bytes`; `false` when the pool cannot hold them.
    pub(crate) fn grow(&mut self, bytes: u64) -> bool {
        let reserved = self.pool.try_reserve(bytes);
        if reserved {
            self.held += bytes;
        }
        reserved
    }

    /// Shrink the charge to `bytes` (the file's new length) and refund the rest.
    pub(crate) fn shrink_to(&mut self, bytes: u64) {
        let freed = self.held.saturating_sub(bytes);
        if freed > 0 {
            self.pool.refund(freed);
            self.held -= freed;
        }
    }
}

// Lifecycle: SpillCharge drop refunds held through SpillPool::refund; it is the last field of
// `Spill`, so the file is closed before the refund.
impl Drop for SpillCharge {
    fn drop(&mut self) {
        self.shrink_to(0);
    }
}

#[cfg(test)]
#[path = "reply_spill_pool_tests.rs"]
mod reply_spill_pool_tests;
