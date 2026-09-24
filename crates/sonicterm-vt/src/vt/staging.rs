//! Bounded staging for in-flight media captures.
//!
//! A capture holds bytes only between its introducer and its terminator, but a
//! stalled transfer never terminates, so staging is handed out from fixed
//! pools rather than grown per parser. A [`CaptureStagingPool`] owns those
//! pools. Production parsers share [`CaptureStagingPool::process_default`],
//! which keeps the ceiling process-wide; a unit test injects a private pool
//! through [`super::Parser::new_with_staging_pool`], so captures in sibling
//! tests cannot change what it measures.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};

use super::MAX_MEDIA_PAYLOAD_BYTES;

/// Ceiling on in-flight capture staging summed over every parser that shares
/// one [`CaptureStagingPool`]. Production parsers all share
/// [`CaptureStagingPool::process_default`], so in production this bounds the
/// whole process.
///
/// A capture is *staging*, not retained: it exists only between an
/// APC/DCS introducer and its terminator, and the bytes are handed to the host
/// as a `MediaEvent` the moment the sequence completes. What makes it worth
/// bounding is that the terminator is not guaranteed to arrive. A capture
/// whose stream stalls — `imgcat` over a dropped SSH link, a program killed
/// mid-transfer — pins its buffer until the pane dies, and no eviction pass
/// can reclaim it, because the parser cannot distinguish a stalled transfer
/// from a slow one.
///
/// Per-parser the buffer is bounded. Composed across panes it was not:
/// 20 panes each mid-capture measured 320 MiB, every parser individually
/// compliant. That composition is the shape this ceiling exists to close.
///
/// This is a real ceiling, not a target: staging is handed out from two fixed
/// pools that sum to exactly this figure, so the total cannot exceed it at any
/// number of panes. A per-capture share with a floor cannot make that promise
/// — past the point where the floor wins the clamp, the sum is the floor times
/// the number of panes, and nothing bounds the number of panes.
///
/// Public so the bound can be measured against real heap from outside the
/// crate. Checking only per-parser arithmetic would miss the cross-pane
/// composition this process-wide ceiling controls.
pub const MAX_PROCESS_CAPTURE_STAGING_BYTES: usize = 64 * 1024 * 1024;

/// Smallest staging budget an admitted capture is ever given.
///
/// A fair share alone shrinks toward zero as captures multiply, and a capture
/// truncated below the size of a typical encoded image renders a broken
/// picture — the outcome this floor exists to prevent. Sized to hold a
/// representative PNG/JPEG payload whole so that an admitted pane can always
/// complete an ordinary image.
///
/// Public so the guarantee can be asserted from outside the crate: the floor
/// is the promise made to an admitted pane, and a bound that held by quietly
/// withdrawing it would be a regression dressed as a fix.
pub const MIN_CAPTURE_STAGING_BYTES: usize = 4 * 1024 * 1024;

/// Staging reserved for growth beyond the floor.
///
/// Exactly what one capture needs to climb from the floor to the per-capture
/// maximum, so a lone pane receiving a large image still gets all 16 MiB of it
/// — the common case, and not the one that needs constraining. Held apart from
/// the floor pool so that a capture growing large cannot consume the floors
/// other panes are guaranteed.
const CAPTURE_GROWTH_POOL_BYTES: usize = MAX_MEDIA_PAYLOAD_BYTES - MIN_CAPTURE_STAGING_BYTES;

/// Staging reserved for the floors of concurrent captures.
const CAPTURE_FLOOR_POOL_BYTES: usize =
    MAX_PROCESS_CAPTURE_STAGING_BYTES - CAPTURE_GROWTH_POOL_BYTES;

/// How many panes can hold an ordinary image whole at the same time.
///
/// The honest form of the promise the floor makes. The old formulation —
/// every pane, however many are active, gets at least the floor — is not
/// something a fixed ceiling can promise, because panes are not bounded:
/// nothing in the workspace caps tab or split count, so "every pane" is
/// unbounded and `N × floor` has no maximum.
///
/// Derived rather than chosen, so it cannot drift from the pools it describes.
/// Raising it means raising the ceiling; the arithmetic is the trade, stated.
pub const GUARANTEED_CONCURRENT_CAPTURES: usize =
    CAPTURE_FLOOR_POOL_BYTES / MIN_CAPTURE_STAGING_BYTES;

/// Floor and growth pools that bound media-capture staging for every parser
/// that shares them.
///
/// Production parsers share [`Self::process_default`], so
/// [`MAX_PROCESS_CAPTURE_STAGING_BYTES`] bounds every pane in the process
/// together. A test that measures admission or accounting creates its own pool
/// with [`Self::new`] and passes it to
/// [`super::Parser::new_with_staging_pool`], so no sibling test's capture can
/// change what it observes.
#[derive(Debug)]
pub struct CaptureStagingPool {
    /// Floor bytes currently reserved by live captures.
    floor_reserved: AtomicUsize,
    /// Growth bytes currently reserved by live captures.
    growth_reserved: AtomicUsize,
    /// Captures currently alive against this pool, admitted or refused.
    live_captures: AtomicUsize,
}

impl CaptureStagingPool {
    /// Create an empty pool with the production ceilings.
    ///
    /// Production code shares [`Self::process_default`]; a separate pool is for
    /// a test that must not observe captures held by sibling tests.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            floor_reserved: AtomicUsize::new(0),
            growth_reserved: AtomicUsize::new(0),
            live_captures: AtomicUsize::new(0),
        })
    }

    /// The pool every production parser stages captures in.
    ///
    /// One shared pool is what makes [`MAX_PROCESS_CAPTURE_STAGING_BYTES`] a
    /// process-wide ceiling. Parsers built with [`super::Parser::new`] or
    /// [`super::Parser::new_with_reply`] use it.
    #[must_use]
    pub fn process_default() -> Arc<Self> {
        static PROCESS_DEFAULT: OnceLock<Arc<CaptureStagingPool>> = OnceLock::new();
        Arc::clone(PROCESS_DEFAULT.get_or_init(Self::new))
    }

    /// Captures currently alive against this pool, summed over every parser
    /// that shares it and including refused captures.
    ///
    /// Distinct from [`super::Parser::live_capture_count`], which counts one
    /// parser's own captures.
    // Ordering: live_captures uses Relaxed because the count is observational, not a publication barrier.
    #[must_use]
    pub fn live_captures(&self) -> usize {
        self.live_captures.load(Ordering::Relaxed)
    }

    /// Floor bytes currently reserved from this pool.
    // Ordering: floor_reserved uses Relaxed because tests read an accounting total, not published data.
    #[cfg(test)]
    pub(super) fn floor_reserved(&self) -> usize {
        self.floor_reserved.load(Ordering::Relaxed)
    }
}

/// Take `want` bytes from `pool` if `capacity` has them, all or nothing.
///
/// All-or-nothing because a partial grant would put a capture's buffer at a
/// size that is not a power of two, and `Vec` growth rounds up: a 6 MiB budget
/// is held in an 8 MiB allocation, so the pool would be handing out bytes the
/// allocator does not honour and the ceiling would fail by the rounding.
// Ordering: pool uses Relaxed because reservation totals enforce capacity without publishing payload data.
fn reserve_from(pool: &AtomicUsize, capacity: usize, want: usize) -> bool {
    let mut reserved = pool.load(Ordering::Relaxed);
    loop {
        if want > capacity.saturating_sub(reserved) {
            // When: want cannot fit without exceeding capacity, so refusing preserves the process-wide staging ceiling.
            return false;
        }
        match pool.compare_exchange_weak(
            reserved,
            reserved + want,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                // When: pool accepted the full reservation, so the caller may stage against the enlarged budget.
                return true;
            }
            Err(current) => reserved = current,
        }
    }
}

/// One capture's claim on its parser's staging pool.
///
/// A separate guard rather than `impl Drop for MediaCapture` because
/// `into_event`/`into_kitty_event` move fields out of the capture, which a
/// `Drop` impl on the capture itself would forbid. As a field, it is dropped
/// by those destructurings exactly as it is by an explicit `= None`, so every
/// release path returns its bytes without any of them naming the pool.
#[derive(Debug)]
pub(super) struct StagingReservation {
    /// Pool this reservation was admitted to, and returns its bytes to.
    pool: Arc<CaptureStagingPool>,
    floor: usize,
    growth: usize,
}

impl StagingReservation {
    /// Admit a capture if `pool`'s floor can still guarantee it an ordinary
    /// image, otherwise refuse it.
    ///
    /// Refusing rather than admitting at a reduced size is what makes the
    /// ceiling hold. It is also the better rendering outcome: a capture
    /// truncated below a whole image decodes to nothing for Kitty and iTerm2,
    /// and for Sixel to a silently cut-off picture, which is the broken
    /// picture the floor exists to prevent rather than an approximation of the
    /// image the user asked for.
    ///
    /// Under this crate's unit tests, admitting to the process-default pool
    /// panics: every unit test injects a private pool, so no capture it opens
    /// can perturb a sibling's measurement.
    // Ordering: live_captures uses Relaxed because it is an observational count, not a publication barrier.
    pub(super) fn admit(pool: &Arc<CaptureStagingPool>) -> Self {
        #[cfg(test)]
        assert!(
            !Arc::ptr_eq(pool, &CaptureStagingPool::process_default()),
            "a unit test staged a media capture on the process-default pool; inject a private \
             CaptureStagingPool with Parser::new_with_staging_pool"
        );
        pool.live_captures.fetch_add(1, Ordering::Relaxed);
        let floor = if reserve_from(
            &pool.floor_reserved,
            CAPTURE_FLOOR_POOL_BYTES,
            MIN_CAPTURE_STAGING_BYTES,
        ) {
            MIN_CAPTURE_STAGING_BYTES
        } else {
            // When: reserve_from refuses the floor, so admitting without staging would permit a partial media payload.
            tracing::warn!(
                guaranteed = GUARANTEED_CONCURRENT_CAPTURES,
                "media capture refused: staging pool is fully committed to captures \
                     already in flight"
            );
            0
        };
        Self { pool: Arc::clone(pool), floor, growth: 0 }
    }

    /// Whether this capture was given staging at all.
    pub(super) fn admitted(&self) -> bool {
        self.floor > 0
    }

    /// Bytes this capture may hold.
    pub(super) fn budget(&self) -> usize {
        self.floor + self.growth
    }

    /// Double the budget out of the growth pool.
    ///
    /// Doubling rather than a fixed block so every budget stays a power of
    /// two. `Vec` grows by doubling, so a power-of-two budget is held in an
    /// allocation of exactly that size and the reservation matches the heap;
    /// any other budget would be rounded up by the allocator into bytes the
    /// pool never granted.
    pub(super) fn try_double(&mut self) -> bool {
        let current = self.budget();
        if current == 0 || current >= MAX_MEDIA_PAYLOAD_BYTES {
            // When: current is absent or capped, so further growth would violate admission or the per-capture ceiling.
            return false;
        }
        if !reserve_from(&self.pool.growth_reserved, CAPTURE_GROWTH_POOL_BYTES, current) {
            // When: reserve_from cannot grant current bytes atomically, so retaining a partial grant would undercount heap capacity.
            return false;
        }
        self.growth += current;
        true
    }
}

impl Clone for StagingReservation {
    /// A cloned capture is a second live capture, so it makes its own claim
    /// on the same pool rather than duplicating one the pool only granted once.
    fn clone(&self) -> Self {
        Self::admit(&self.pool)
    }
}

// Lifecycle: dropping StagingReservation returns floor_reserved and growth_reserved bytes to its pool and decrements live_captures.
impl Drop for StagingReservation {
    // Ordering: live_captures, floor_reserved, and growth_reserved use Relaxed for independent accounting totals.
    fn drop(&mut self) {
        self.pool.live_captures.fetch_sub(1, Ordering::Relaxed);
        self.pool.floor_reserved.fetch_sub(self.floor, Ordering::Relaxed);
        self.pool.growth_reserved.fetch_sub(self.growth, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod staging_tests;
