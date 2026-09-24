//! Unit tests for capture staging pools and reservations.

use super::*;

/// The pools must sum to the ceiling exactly.
///
/// The ceiling is only a ceiling if nothing can be handed out that is not
/// drawn from one of the two pools. Asserted as arithmetic over the
/// constants — the heap-truth integration tests are what check that the code
/// actually obeys them.
#[test]
fn the_pools_sum_to_the_process_ceiling() {
    assert_eq!(
        CAPTURE_FLOOR_POOL_BYTES + CAPTURE_GROWTH_POOL_BYTES,
        MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "staging handed out from pools that do not sum to the ceiling is not bounded by it"
    );
}

/// The growth pool must let one capture reach the per-capture maximum.
///
/// A lone pane receiving a large image is the common case and the one the
/// ceiling must not touch. If growth were smaller than the climb from the
/// floor to the maximum, no capture could ever reach the maximum and
/// `MAX_MEDIA_PAYLOAD_BYTES` would be a number no code path can produce.
#[test]
fn the_growth_pool_covers_one_capture_climbing_to_the_maximum() {
    assert_eq!(
        CAPTURE_GROWTH_POOL_BYTES,
        MAX_MEDIA_PAYLOAD_BYTES - MIN_CAPTURE_STAGING_BYTES,
        "the growth pool must be exactly one capture's climb from the floor to the maximum"
    );
}

/// The guarantee must be the floor pool divided by the floor.
///
/// Derived rather than chosen, so the promise cannot drift from the pool that
/// backs it. A guarantee larger than the pool would be a promise the pools
/// cannot keep; smaller would be leaving panes unrendered for no reason.
#[test]
fn the_guarantee_is_derived_from_the_floor_pool() {
    assert_eq!(
        GUARANTEED_CONCURRENT_CAPTURES * MIN_CAPTURE_STAGING_BYTES,
        CAPTURE_FLOOR_POOL_BYTES,
        "the guaranteed count must be exactly what the floor pool can floor"
    );
}

/// The guarantee must cover a plausible session.
///
/// A change that held the ceiling by guaranteeing one or two panes would
/// satisfy every bound assertion in the suite while making the terminal
/// useless for the case the floor exists to serve. Compile-time because both
/// sides are constants.
const _: () = assert!(
    GUARANTEED_CONCURRENT_CAPTURES >= 8,
    "the staging pools guarantee too few concurrent captures to cover a plausible \
     working session"
);

/// Two private pools admit and account independently: filling one pool's
/// floor refuses its next capture without touching the other pool.
#[test]
fn private_pools_admit_and_account_independently() {
    let a = CaptureStagingPool::new();
    let b = CaptureStagingPool::new();

    let held: Vec<StagingReservation> =
        (0..GUARANTEED_CONCURRENT_CAPTURES).map(|_| StagingReservation::admit(&a)).collect();
    assert!(held.iter().all(StagingReservation::admitted), "the guarantee is admitted");
    let refused = StagingReservation::admit(&a);
    assert!(!refused.admitted(), "a's floor is fully committed");
    let independent = StagingReservation::admit(&b);
    assert!(independent.admitted(), "b's floor is untouched by a's captures");

    assert_eq!(a.live_captures(), GUARANTEED_CONCURRENT_CAPTURES + 1);
    assert_eq!(a.floor_reserved(), GUARANTEED_CONCURRENT_CAPTURES * MIN_CAPTURE_STAGING_BYTES);
    assert_eq!((b.live_captures(), b.floor_reserved()), (1, MIN_CAPTURE_STAGING_BYTES));

    drop((held, refused, independent));
    assert_eq!((a.live_captures(), a.floor_reserved()), (0, 0));
    assert_eq!((b.live_captures(), b.floor_reserved()), (0, 0));
}

/// A cloned reservation is a second live capture on the same pool, so it makes
/// its own claim instead of sharing the original's grant.
#[test]
fn a_cloned_reservation_claims_its_own_share_of_the_same_pool() {
    let pool = CaptureStagingPool::new();
    let original = StagingReservation::admit(&pool);
    let copy = original.clone();

    assert!(Arc::ptr_eq(&copy.pool, &pool), "the clone reserves from its original's pool");
    assert_eq!((pool.live_captures(), pool.floor_reserved()), (2, 2 * MIN_CAPTURE_STAGING_BYTES));

    drop(original);
    assert_eq!(
        (pool.live_captures(), pool.floor_reserved()),
        (1, MIN_CAPTURE_STAGING_BYTES),
        "dropping one reservation returns only its own claim"
    );
    drop(copy);
    assert_eq!((pool.live_captures(), pool.floor_reserved()), (0, 0));
}

/// Cloning a grown reservation claims only a fresh floor share: the growth
/// stays with the original and returns to the pool when the original drops.
#[test]
fn a_clone_of_a_grown_reservation_claims_only_a_floor_share() {
    let pool = CaptureStagingPool::new();
    let mut original = StagingReservation::admit(&pool);
    assert!(original.try_double(), "an uncontended reservation can grow");
    let copy = original.clone();

    assert_eq!(copy.budget(), MIN_CAPTURE_STAGING_BYTES, "the clone holds only a floor share");
    assert_eq!(original.budget(), 2 * MIN_CAPTURE_STAGING_BYTES, "the original keeps its growth");
    assert_eq!(pool.floor_reserved(), 2 * MIN_CAPTURE_STAGING_BYTES);
    assert_eq!(pool.growth_reserved.load(Ordering::Relaxed), MIN_CAPTURE_STAGING_BYTES);

    drop(original);
    assert_eq!(pool.growth_reserved.load(Ordering::Relaxed), 0, "growth leaves with the original");
    assert_eq!((pool.live_captures(), pool.floor_reserved()), (1, MIN_CAPTURE_STAGING_BYTES));
    drop(copy);
    assert_eq!((pool.live_captures(), pool.floor_reserved()), (0, 0));
}

/// A clone made while the floor is exhausted is refused like any new capture:
/// it stages nothing, cannot grow, and its drop returns only its live count.
#[test]
fn a_clone_refused_by_an_exhausted_floor_reserves_nothing() {
    let pool = CaptureStagingPool::new();
    let held: Vec<StagingReservation> =
        (0..GUARANTEED_CONCURRENT_CAPTURES).map(|_| StagingReservation::admit(&pool)).collect();
    let full = pool.floor_reserved();

    let mut copy = held[0].clone();
    assert!(!copy.admitted(), "an exhausted floor refuses the clone");
    assert_eq!(copy.budget(), 0);
    assert!(!copy.try_double(), "a refused clone cannot grow");
    assert_eq!(pool.floor_reserved(), full, "the refused clone reserved no floor");
    assert_eq!(pool.growth_reserved.load(Ordering::Relaxed), 0);
    assert_eq!(pool.live_captures(), GUARANTEED_CONCURRENT_CAPTURES + 1);

    drop(copy);
    assert_eq!(pool.live_captures(), GUARANTEED_CONCURRENT_CAPTURES);
    assert_eq!(pool.floor_reserved(), full);
    drop(held);
    assert_eq!((pool.live_captures(), pool.floor_reserved()), (0, 0));
}

/// Growth comes from each pool's own growth budget: one capture climbing to the
/// per-capture maximum exhausts its pool's growth but not another pool's.
#[test]
fn growth_is_accounted_per_pool() {
    let a = CaptureStagingPool::new();
    let b = CaptureStagingPool::new();

    let mut climbing = StagingReservation::admit(&a);
    while climbing.try_double() {}
    assert_eq!(climbing.budget(), MAX_MEDIA_PAYLOAD_BYTES, "one capture reaches the maximum");

    let mut second_a = StagingReservation::admit(&a);
    assert!(!second_a.try_double(), "a's growth is committed to the first capture");
    let mut first_b = StagingReservation::admit(&b);
    assert!(first_b.try_double(), "b's growth is independent of a's");
}

/// Every call to `process_default` returns the same pool, and a new pool is a
/// separate domain.
#[test]
fn the_process_default_is_one_shared_domain() {
    assert!(Arc::ptr_eq(
        &CaptureStagingPool::process_default(),
        &CaptureStagingPool::process_default()
    ));
    assert!(!Arc::ptr_eq(&CaptureStagingPool::process_default(), &CaptureStagingPool::new()));
}

/// Under this crate's unit tests, admitting to the process-default pool panics
/// before touching its counters, so a test that forgets to inject a private
/// pool fails loudly instead of perturbing a sibling's measurement.
#[test]
#[should_panic(expected = "process-default pool")]
fn unit_tests_cannot_stage_on_the_process_default_pool() {
    drop(StagingReservation::admit(&CaptureStagingPool::process_default()));
}
